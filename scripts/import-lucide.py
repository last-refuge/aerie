#!/usr/bin/env python3
"""Converts Lucide icons into GTK symbolic icons.

Lucide draws with strokes. GTK fills symbolic icon shapes unless they're marked as
foreground strokes in its "grappa" format (GTK 4.20+), so each shape gets
gpa:stroke="foreground" and the brand's 1.75 stroke width.

Usage: scripts/import-lucide.py <lucide-static>/icons name [name...]
Writes assets/icons/hicolor/scalable/actions/aerie-<name>-symbolic.svg.
"""

import pathlib
import re
import sys

SHAPES = r"<(path|circle|rect|line|polyline|polygon|ellipse)\b"
STROKE = (
    'gpa:stroke="foreground" fill="none" stroke="black" stroke-width="1.75" '
    'stroke-linecap="round" stroke-linejoin="round"'
)


def convert(svg: str) -> str:
    svg = re.sub(r"<!--.*?-->\s*", "", svg, flags=re.S)
    # Keep only the attributes GTK needs on the root element.
    svg = re.sub(
        r"<svg\b[^>]*>",
        '<svg xmlns="http://www.w3.org/2000/svg" xmlns:gpa="https://www.gtk.org/grappa" '
        'gpa:version="1" width="24" height="24" viewBox="0 0 24 24">',
        svg,
        count=1,
    )
    return re.sub(SHAPES, lambda m: f"<{m.group(1)} {STROKE}", svg)


def main() -> None:
    source = pathlib.Path(sys.argv[1])
    out = pathlib.Path(__file__).resolve().parent.parent / "assets/icons/hicolor/scalable/actions"
    out.mkdir(parents=True, exist_ok=True)
    for name in sys.argv[2:]:
        svg = (source / f"{name}.svg").read_text()
        (out / f"aerie-{name}-symbolic.svg").write_text(convert(svg))


if __name__ == "__main__":
    main()
