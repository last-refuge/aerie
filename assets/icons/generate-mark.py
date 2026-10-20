#!/usr/bin/env python3
# Regenerate with: python3 assets/icons/generate-mark.py mark > assets/icons/hicolor/scalable/apps/ai.lastrefuge.Aerie-mark.svg
#             and: python3 assets/icons/generate-mark.py icon > assets/icons/hicolor/scalable/apps/ai.lastrefuge.Aerie.svg
"""Generates the Aerie mark: one ribbon that loops through three cloud lobes and ends in a
tapering swoosh, drawn as filled outlines so its width can vary."""
import math, sys

def arc(c, r, a0, a1, n=48):
    pts = []
    for i in range(n + 1):
        t = math.radians(a0 + (a1 - a0) * i / n)
        pts.append((c[0] + r * math.cos(t), c[1] + r * math.sin(t)))
    return pts

def bez(p0, p1, p2, p3, n=32):
    pts = []
    for i in range(n + 1):
        t = i / n; u = 1 - t
        pts.append(tuple(u**3*a + 3*u*u*t*b + 3*u*t*t*c + t**3*d for a, b, c, d in zip(p0, p1, p2, p3)))
    return pts

def line(a, b, n=16):
    return [(a[0] + (b[0]-a[0])*i/n, a[1] + (b[1]-a[1])*i/n) for i in range(n + 1)]

def ribbon(pts, w0, w1, ease=lambda t: t):
    """Outline of a ribbon along pts whose width goes from w0 to w1."""
    left, right = [], []
    n = len(pts)
    for i, p in enumerate(pts):
        a = pts[max(i-1, 0)]; b = pts[min(i+1, n-1)]
        dx, dy = b[0]-a[0], b[1]-a[1]; d = math.hypot(dx, dy) or 1
        nx, ny = -dy/d, dx/d
        w = (w0 + (w1-w0)*ease(i/(n-1))) / 2
        left.append((p[0]+nx*w, p[1]+ny*w)); right.append((p[0]-nx*w, p[1]-ny*w))
    poly = left + right[::-1]
    return "M" + " L".join(f"{x:.1f} {y:.1f}" for x, y in poly) + " Z"

def centerline(pts):
    return "M" + " L".join(f"{x:.1f} {y:.1f}" for x, y in pts)

Cr, rr = (103, 59), 13      # right lobe, behind
Cl, rl = (34, 55), 17       # left lobe
Ct, rt = (71, 35), 19       # top lobe

def ribbon_w(segments):
    """One continuous ribbon from (points, w0, w1, ease) segments, so there are no seams."""
    pts, widths = [], []
    for seg, w0, w1, ease in segments:
        seg = seg if not pts else seg[1:]
        n = len(seg)
        for i, p in enumerate(seg):
            pts.append(p)
            widths.append(w0 + (w1 - w0) * ease((i + 1) / n if n else 1))
    left, right = [], []
    for i, p in enumerate(pts):
        a = pts[max(i-1, 0)]; b = pts[min(i+1, len(pts)-1)]
        dx, dy = b[0]-a[0], b[1]-a[1]; d = math.hypot(dx, dy) or 1
        nx, ny = -dy/d, dx/d
        w = widths[i] / 2
        left.append((p[0]+nx*w, p[1]+ny*w)); right.append((p[0]-nx*w, p[1]-ny*w))
    poly = left + right[::-1]
    return "M" + " L".join(f"{x:.1f} {y:.1f}" for x, y in poly) + " Z"

lin = lambda t: t
smooth = lambda t: 0.5 - 0.5 * math.cos(math.pi * t)

back = arc(Cr, rr, -150, 90) + line((Cr[0], Cr[1]+rr), (60, Cl[1]+rl))[1:]

def pt(c, r, deg):
    t = math.radians(deg); return (c[0] + r*math.cos(t), c[1] + r*math.sin(t))
def tangent(deg, k):
    t = math.radians(deg); return (-math.sin(t)*k, math.cos(t)*k)

a, b = pt(Cl, rl, 250), pt(Ct, rt, 200)
ta, tb = tangent(250, 9), tangent(200, 9)
bridge = bez(a, (a[0]+ta[0], a[1]+ta[1]), (b[0]-tb[0], b[1]-tb[1]), b, 24)
end = pt(Ct, rt, 60)
te = tangent(60, 16)
swoosh = bez(end, (end[0]+te[0], end[1]+te[1]), (58, 59), (45, 66.5), 44)

front = ribbon_w([
    (line((66, Cl[1]+rl), (Cl[0], Cl[1]+rl)), 10.5, 10.5, lin),
    (arc(Cl, rl, 90, 250, 64), 10.5, 12, lin),
    (bridge, 12, 12.5, lin),
    (arc(Ct, rt, 200, 300, 50), 12.5, 15, smooth),
    (arc(Ct, rt, 300, 420, 50), 15, 13, smooth),
    (swoosh, 13, 0.6, lambda t: t**1.3),
])
parts = {"back": ribbon(back, 9, 10.5), "front": front}
tail = ribbon(swoosh, 13, 0.6, ease=lambda t: t**1.3)
center = {k: centerline(v) for k, v in
          {"top": arc(Ct, rt + 5, 200, 320), "left": arc(Cl, rl + 3.2, 150, 235)}.items()}

def mark(transform="", glow=True):
    use = lambda name, fill: f'<use href="#{name}" fill="{fill}"/>'
    body = []
    if glow:
        body.append('<g filter="url(#glow)" opacity="0.55">'
                    + use("back", "url(#base)") + use("front", "url(#base)") + "</g>")
    body += [
        use("back", "url(#base)"), use("back", "url(#behind)"), use("back", "url(#tube)"),
        use("front", "url(#base)"), use("front", "url(#tube)"),
        use("front", "url(#shade-top)"), use("front", "url(#shade-left)"),
        use("tail", "url(#swoosh)"),
    ]
    for d in center.values():
        body.append(f'<path d="{d}" fill="none" stroke="#ffffff" stroke-opacity="0.4" stroke-width="1.5" stroke-linecap="round"/>')
    return f'<g{transform}>' + "".join(body) + "</g>"

DEFS = f'''<defs>
    <path id="back" d="{parts['back']}"/>
    <path id="front" d="{parts['front']}"/>
    <path id="tail" d="{tail}"/>
    <linearGradient id="base" gradientUnits="userSpaceOnUse" x1="14" y1="70" x2="120" y2="40">
      <stop offset="0" stop-color="#22d3ee"/>
      <stop offset="0.38" stop-color="#38bdf8"/>
      <stop offset="0.68" stop-color="#3b82f6"/>
      <stop offset="1" stop-color="#8b5cf6"/>
    </linearGradient>
    <linearGradient id="swoosh" gradientUnits="userSpaceOnUse" x1="76" y1="56" x2="43" y2="71">
      <stop offset="0" stop-color="#67e8f9" stop-opacity="0"/>
      <stop offset="1" stop-color="#4f46e5" stop-opacity="0.55"/>
    </linearGradient>
    <linearGradient id="behind" gradientUnits="userSpaceOnUse" x1="68" y1="0" x2="104" y2="0">
      <stop offset="0" stop-color="#1e1b4b" stop-opacity="0"/>
      <stop offset="1" stop-color="#1e1b4b" stop-opacity="0.4"/>
    </linearGradient>
    <linearGradient id="tube" gradientUnits="userSpaceOnUse" x1="0" y1="{Cl[1]+rl-5}" x2="0" y2="{Cl[1]+rl+5}">
      <stop offset="0" stop-color="#e0f2fe" stop-opacity="0.16"/>
      <stop offset="0.45" stop-color="#e0f2fe" stop-opacity="0"/>
      <stop offset="1" stop-color="#0b1220" stop-opacity="0.3"/>
    </linearGradient>
    <radialGradient id="shade-top" gradientUnits="userSpaceOnUse" cx="{Ct[0]}" cy="{Ct[1]}" r="{rt+9}">
      <stop offset="{(rt-7.5)/(rt+9):.3f}" stop-color="#0b1220" stop-opacity="0.45"/>
      <stop offset="{(rt-1)/(rt+9):.3f}" stop-color="#0b1220" stop-opacity="0"/>
      <stop offset="{(rt+5)/(rt+9):.3f}" stop-color="#e0f2fe" stop-opacity="0.22"/>
      <stop offset="1" stop-color="#e0f2fe" stop-opacity="0"/>
    </radialGradient>
    <radialGradient id="shade-left" gradientUnits="userSpaceOnUse" cx="{Cl[0]}" cy="{Cl[1]}" r="{rl+7}">
      <stop offset="{(rl-6)/(rl+7):.3f}" stop-color="#0b1220" stop-opacity="0.4"/>
      <stop offset="{(rl-1)/(rl+7):.3f}" stop-color="#0b1220" stop-opacity="0"/>
      <stop offset="{(rl+4)/(rl+7):.3f}" stop-color="#e0f2fe" stop-opacity="0.18"/>
      <stop offset="1" stop-color="#e0f2fe" stop-opacity="0"/>
    </radialGradient>
    <filter id="glow" x="-25%" y="-25%" width="150%" height="150%">
      <feGaussianBlur stdDeviation="4"/>
    </filter>
    <linearGradient id="surface" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#1e293b"/>
      <stop offset="1" stop-color="#0b1220"/>
    </linearGradient>
    <radialGradient id="halo" cx="0.5" cy="0.45" r="0.55">
      <stop offset="0" stop-color="#6366f1" stop-opacity="0.38"/>
      <stop offset="1" stop-color="#6366f1" stop-opacity="0"/>
    </radialGradient>
  </defs>'''

which = sys.argv[1]
if which == "mark":
    print(f'<svg xmlns="http://www.w3.org/2000/svg" width="128" height="96" viewBox="0 0 128 96">\n  <title>Aerie</title>\n  {DEFS}\n  {mark()}\n</svg>')
else:
    print(f'''<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <title>Aerie</title>
  {DEFS}
  <rect x="8" y="8" width="112" height="112" rx="28" fill="url(#surface)"/>
  <rect x="8" y="8" width="112" height="112" rx="28" fill="url(#halo)"/>
  <rect x="8.5" y="8.5" width="111" height="111" rx="27.5" fill="none" stroke="#334155" stroke-opacity="0.6"/>
  {mark(' transform="translate(9 23) scale(0.86)"')}
</svg>''')
