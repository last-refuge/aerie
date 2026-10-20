#!/usr/bin/env python3
"""Turns a recording from record.sh into the OAuth verification video.

Each step becomes a clip that follows the window it is about (Aerie or the browser),
framed on Midnight with its caption in a band below. Consent steps add a magnified
address bar so reviewers can read the client ID. A title card opens the video with the
app, its client ID and the scopes it requests; a closing card says how the data is used.

    tools/verification-video/assemble.py ~/Videos/aerie-verification/<run> \
        [--privacy-url https://…] [--homepage https://…]

Needs ffmpeg (with libass) and rsvg-convert. Writes <run>/aerie-oauth-verification.mp4.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
FONTS = REPO / "assets" / "fonts"
MARK = REPO / "assets" / "icons" / "hicolor" / "scalable" / "apps" / "ai.lastrefuge.Aerie-mark.svg"

W, H = 1920, 1080
BAND = 180  # caption band at the bottom
MARGIN = 28
FPS = 30
MIDNIGHT = "0x0B1220"
INTRO_SECONDS = 7
OUTRO_SECONDS = 9

SCOPES = [
    ("https://www.googleapis.com/auth/drive.readonly",
     "Asked at sign-in. Lists, searches and downloads the user's Drive files to browse them in Aerie."),
    ("https://www.googleapis.com/auth/drive",
     "Asked only when the user first changes something. Creates, uploads, renames, moves, trashes and syncs files."),
]


def run(*args, **kwargs):
    subprocess.run(args, check=True, **kwargs)


def load_jsonl(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def load_steps():
    """Captions and targets from steps.tsv, by step id."""
    steps = {}
    for line in (HERE / "steps.tsv").read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        step_id, show, caption, _ = line.split("\t")
        steps[step_id] = {"show": show, "caption": caption}
    return steps


def matches(window, show):
    cls = (window or {}).get("class", "").lower()
    if show == "aerie":
        return "aerie" in cls
    if show == "browser":
        return any(name in cls for name in ("chrome", "chromium", "firefox", "brave"))
    return False


def window_for(focus, show, start, end):
    """The geometry of the last focused window of the right kind in [start, end), else
    the last one before it."""
    best = None
    for sample in focus:
        if sample["t"] >= end:
            break
        if matches(sample["window"], show):
            best = sample["window"]
    return best


def physical(window, monitor, video_size):
    """A window's rectangle in the recording's pixels."""
    scale = monitor["scale"]
    vx = video_size[0] / monitor["width"]
    vy = video_size[1] / monitor["height"]
    x = (window["at"][0] - monitor["x"]) * scale * vx
    y = (window["at"][1] - monitor["y"]) * scale * vy
    w = window["size"][0] * scale * vx
    h = window["size"][1] * scale * vy
    even = lambda v: int(v) // 2 * 2
    x, y = max(0, even(x)), max(0, even(y))
    w = even(min(w, video_size[0] - x))
    h = even(min(h, video_size[1] - y))
    return x, y, w, h


def video_size(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
         "stream=width,height", "-of", "json", str(path)],
        check=True, capture_output=True, text=True).stdout
    stream = json.loads(out)["streams"][0]
    return stream["width"], stream["height"]


def clip(raw, start, end, rect, inset, out):
    """One step: the window framed on Midnight above the caption band, plus the
    magnified address bar for consent steps."""
    x, y, w, h = rect
    box_w, box_h = W - 2 * MARGIN, H - BAND - 2 * MARGIN
    graph = [
        f"[0:v]crop={w}:{h}:{x}:{y},"
        f"scale={box_w}:{box_h}:force_original_aspect_ratio=decrease:flags=lanczos,"
        f"pad={W}:{H}:(ow-iw)/2:{MARGIN}+({box_h}-ih)/2:color={MIDNIGHT},setsar=1[framed]"
    ]
    last = "framed"
    if inset:
        # The browser's toolbar: tabs and address bar, left part where client_id appears.
        bar_h = int(h * 0.075) // 2 * 2
        bar_w = int(w * 0.62) // 2 * 2
        width = W - 2 * MARGIN - 8
        graph.append(
            f"[0:v]crop={bar_w}:{bar_h}:{x}:{y},scale={width}:-2:flags=lanczos,"
            f"pad=iw+8:ih+8:4:4:color=0x38BDF8[bar]")
        # Shown for the first part of the step only, so the consent screen itself is
        # then seen unobstructed.
        shown = max(2.5, (end - start) * 0.45)
        graph.append(f"[framed][bar]overlay=x=(W-w)/2:y={H - BAND}-h-{MARGIN}"
                     f":enable='lt(t,{shown:.2f})'[withbar]")
        last = "withbar"
    duration = max(0.5, end - start)
    graph.append(f"[{last}]fps={FPS},fade=t=in:st=0:d=0.25:color={MIDNIGHT},"
                 f"fade=t=out:st={max(0, duration - 0.25):.2f}:d=0.25:color={MIDNIGHT}[out]")
    run("ffmpeg", "-v", "error", "-y", "-ss", f"{start:.2f}", "-t", f"{duration:.2f}",
        "-i", str(raw), "-filter_complex", ";".join(graph), "-map", "[out]", "-an",
        "-c:v", "libx264", "-preset", "medium", "-crf", "16", "-pix_fmt", "yuv420p", str(out))
    return duration


def escape(text):
    return (text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
            .replace('"', "&quot;"))


def card_svg(lines, mark_size=150):
    """A Midnight title card: the mark, then lines of (text, size, weight, color, font)."""
    mark = MARK.read_text()
    mark_body = mark[mark.index(">", mark.index("<svg")) + 1:mark.rindex("</svg>")]
    mark_w = mark_size * 4 / 3
    total = mark_size + 30 + sum(size * 1.45 + (size if text == "" else 0)
                                 for text, size, *_ in lines)
    y = (H - total) / 2
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" '
        f'width="{W}" height="{H}" viewBox="0 0 {W} {H}">',
        '<defs><radialGradient id="card-halo" cx="0.5" cy="0.32" r="0.5">'
        '<stop offset="0" stop-color="#6366f1" stop-opacity="0.22"/>'
        '<stop offset="1" stop-color="#6366f1" stop-opacity="0"/></radialGradient>'
        '<radialGradient id="card-horizon" cx="0.5" cy="1.15" r="0.6">'
        '<stop offset="0" stop-color="#8b5cf6" stop-opacity="0.22"/>'
        '<stop offset="1" stop-color="#8b5cf6" stop-opacity="0"/></radialGradient></defs>',
        f'<rect width="{W}" height="{H}" fill="#0b1220"/>',
        f'<rect width="{W}" height="{H}" fill="url(#card-halo)"/>',
        f'<rect width="{W}" height="{H}" fill="url(#card-horizon)"/>',
        f'<svg x="{(W - mark_w) / 2}" y="{y}" width="{mark_w}" height="{mark_size}" '
        f'viewBox="0 0 128 96">{mark_body}</svg>',
    ]
    y += mark_size + 30
    for text, size, weight, color, font in lines:
        if text == "":
            y += size
            continue
        y += size * 1.2
        family = "JetBrains Mono" if font == "mono" else "Inter Variable"
        spacing = ' letter-spacing="6"' if font == "caps" else ""
        parts.append(
            f'<text x="{W / 2}" y="{y}" text-anchor="middle" font-family="{family}" '
            f'font-size="{size}" font-weight="{weight}" fill="{color}"{spacing}>{escape(text)}</text>')
        y += size * 0.25
    parts.append("</svg>")
    return "\n".join(parts)


def card_clip(svg, seconds, workdir, name):
    svg_path = workdir / f"{name}.svg"
    png_path = workdir / f"{name}.png"
    out = workdir / f"{name}.mp4"
    svg_path.write_text(svg)
    run("rsvg-convert", "-w", str(W), "-h", str(H), "-o", str(png_path), str(svg_path))
    run("ffmpeg", "-v", "error", "-y", "-loop", "1", "-t", str(seconds), "-i", str(png_path),
        "-vf", f"fps={FPS},format=yuv420p,fade=t=in:st=0:d=0.4:color={MIDNIGHT},"
               f"fade=t=out:st={seconds - 0.4}:d=0.4:color={MIDNIGHT}",
        "-c:v", "libx264", "-preset", "medium", "-crf", "16", str(out))
    return out


def ass_time(seconds):
    cs = int(round(seconds * 100))
    return f"{cs // 360000}:{cs // 6000 % 60:02d}:{cs // 100 % 60:02d}.{cs % 100:02d}"


def captions(timeline, path):
    """Lower-third captions in the band under each clip; scope lines in Aerie Blue."""
    header = f"""[Script Info]
ScriptType: v4.00+
PlayResX: {W}
PlayResY: {H}
WrapStyle: 0

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Caption,Inter Variable,40,&H00FCFAF8,&H00FCFAF8,&H00201B0B,&H00201B0B,0,0,0,0,100,100,0,0,1,0,0,2,90,90,64,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
"""
    events = []
    for start, end, text in timeline:
        first, *rest = text.split("\\N")
        body = first + "".join(r"\N{\c&HF8BD38&\fs32}" + line for line in rest)
        events.append(f"Dialogue: 0,{ass_time(start + 0.2)},{ass_time(end - 0.2)},Caption,,0,0,0,,{body}")
    path.write_text(header + "\n".join(events) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("run", type=Path, help="the folder record.sh wrote")
    parser.add_argument("--privacy-url", default="", help="privacy policy URL for the closing card")
    parser.add_argument("--homepage", default="", help="homepage URL for the closing card")
    args = parser.parse_args()

    for tool in ("ffmpeg", "ffprobe", "rsvg-convert"):
        if not shutil.which(tool):
            sys.exit(f"missing: {tool}")

    run_dir = args.run.expanduser().resolve()
    raw = run_dir / "raw.mp4"
    marks = load_jsonl(run_dir / "steps.jsonl")
    focus = load_jsonl(run_dir / "focus.jsonl")
    monitor = json.loads((run_dir / "monitors.json").read_text())[0]
    steps = load_steps()
    size = video_size(raw)
    client_file = run_dir / "config" / "aerie" / "oauth-client.json"
    client = json.loads(client_file.read_text())
    client_id = next(iter(client.values()))["client_id"]

    # Inter and JetBrains Mono for the cards and captions, without installing them.
    workdir = Path(tempfile.mkdtemp(prefix="aerie-video-"))
    fonts_conf = workdir / "fonts.conf"
    fonts_conf.write_text(
        f'<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "fonts.dtd"><fontconfig>'
        f'<dir>{FONTS}</dir><include ignore_missing="yes">/etc/fonts/fonts.conf</include>'
        f'<cachedir>{workdir}/cache</cachedir></fontconfig>')
    os.environ["FONTCONFIG_FILE"] = str(fonts_conf)

    clips, timeline = [], []
    clock = 0.0

    intro = [
        ("Aerie", 72, 700, "#f8fafc", "sans"),
        ("GOOGLE DRIVE FOR LINUX", 20, 600, "#94a3b8", "caps"),
        ("OAuth verification demo", 34, 500, "#cbd5e1", "sans"),
        (f"OAuth client ID: {client_id}", 22, 500, "#94a3b8", "mono"),
        ("", 18, 0, "", ""),
        ("Scopes requested", 28, 650, "#f8fafc", "sans"),
    ]
    for scope, purpose in SCOPES:
        intro.append(("", 6, 0, "", ""))
        intro.append((scope, 24, 500, "#38bdf8", "mono"))
        intro.append((purpose, 22, 400, "#cbd5e1", "sans"))
    clips.append(card_clip(card_svg(intro, mark_size=120), INTRO_SECONDS, workdir, "intro"))
    clock += INTRO_SECONDS

    for index, (mark, following) in enumerate(zip(marks, marks[1:])):
        step = steps.get(mark["id"])
        if not step:
            continue
        start, end = mark["t"], following["t"]
        window = window_for(focus, step["show"], start, end)
        rect = physical(window, monitor, size) if window else (0, 0, size[0], size[1])
        inset = mark["id"].startswith("consent") and window is not None
        out = workdir / f"step-{index:02d}.mp4"
        duration = clip(raw, start, end, rect, inset, out)
        clips.append(out)
        timeline.append((clock, clock + duration, step["caption"]))
        clock += duration
        print(f"  {mark['id']}: {duration:.1f}s")

    outro = [
        ("How Aerie uses Google Drive data", 40, 650, "#f8fafc", "sans"),
        ("", 12, 0, "", ""),
        ("Aerie runs on the user's own computer and talks to Google directly.", 26, 400, "#cbd5e1", "sans"),
        ("File data and metadata stay on that computer; there are no Aerie servers.", 26, 400, "#cbd5e1", "sans"),
        ("Google user data is never sold, shared or used for advertising.", 26, 400, "#cbd5e1", "sans"),
        ("Signing out removes the account's cached data and its saved sign-in.", 26, 400, "#cbd5e1", "sans"),
    ]
    if args.homepage:
        outro.append((args.homepage, 24, 500, "#38bdf8", "mono"))
    if args.privacy_url:
        outro.append((f"Privacy policy: {args.privacy_url}", 24, 500, "#38bdf8", "mono"))
    clips.append(card_clip(card_svg(outro, mark_size=96), OUTRO_SECONDS, workdir, "outro"))

    listing = workdir / "clips.txt"
    listing.write_text("".join(f"file '{c}'\n" for c in clips))
    joined = workdir / "joined.mp4"
    run("ffmpeg", "-v", "error", "-y", "-f", "concat", "-safe", "0", "-i", str(listing),
        "-c", "copy", str(joined))

    subs = workdir / "captions.ass"
    captions(timeline, subs)
    final = run_dir / "aerie-oauth-verification.mp4"
    run("ffmpeg", "-v", "error", "-y", "-i", str(joined),
        "-vf", f"subtitles={subs}:fontsdir={FONTS}",
        "-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p",
        "-movflags", "+faststart", str(final))
    shutil.rmtree(workdir, ignore_errors=True)
    print(f"\nWrote {final}")


if __name__ == "__main__":
    main()
