"""Draws the tray / Dock moods and the app icon from the in-app blob.

Writes, under frontend/tray/ (one set per tint: kompacter, discord):
  {tint}-moods-{n}.png    strip of 3 n×n icons: idle, done, error   (n = 16 20 24 32 48)
  {tint}-busy-{n}.png     strip of 16 n×n compressing frames (10 fps loop)
  {tint}-dock-{mood}.png  256 px macOS Dock moods: idle, done, error (no kompacter idle:
                          that one is the bundle icon, the 256 px frame of icon.ico)
  {tint}-dock-busy-{i}.png  the 16 compressing frames at 128 px for the Dock (00-15)
The compressing frames are palette PNGs (a quarter of the size, no visible loss on the
metal and the flat blob); the moods keep full colour (their gradients and soft shadow band
in a palette). And the static app icon from the idle Kompacter blob: frontend/tray.png (32 px window
icon) and icon.ico (also the source of the macOS and Linux bundle icons).

Body = tools/art.py's blob (same shape, gradient, inner light and gloss) with a thin rim
in the family accent so it holds on light taskbars. Faces follow frontend/stage.slint,
scaled up at small sizes. Done = happy face + green check, error = dizzy face (X eyes,
spiral eyes at Dock size) + red X, both badges cut out of the blob with a transparent gap.
Compressing = the app's press (press-top/bottom.png) squashing the blob (blob-*.png).
Deterministic: same inputs, same bytes. Run from apps/kompacter:
    python3 tools/tray_icons.py      (needs numpy + Pillow)
"""
import math
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

FE = Path("frontend")
OUT = FE / "tray"
SSR = 16  # supersampling for the vector icons
W, H = 132, 116  # blob design size (art.py)
FACE = (0x13, 0x21, 0x3F)  # Theme.face
TRAY_SIZES = (16, 20, 24, 32, 48)
FRAMES = 16  # compressing loop: 16 frames at 10 fps
DOCK, DOCK_BUSY = 256, 128
PALETTE = 255  # colours in the compressing frames

TINTS = {
    # The light body of each family on every taskbar; the rim is the family accent.
    "kompacter": dict(pal=("#e9f2ff", "#9cc4ff", "#5b8cff"), rim="#3d5fc4", blob="blob-light.png"),
    "discord": dict(pal=("#e6e8ff", "#9aa4ff", "#5865f2"), rim="#4752c4", blob="blob-discord-light.png"),
}
MOODS = ("idle", "done", "error")
RED, RED_EDGE = (232, 36, 44), (120, 10, 16)
GREEN, GREEN_EDGE = (30, 178, 84), (8, 82, 36)
pi = math.pi


def interp(table, n):
    ks = sorted(table)
    if n <= ks[0]:
        return table[ks[0]]
    if n >= ks[-1]:
        return table[ks[-1]]
    for a, b in zip(ks, ks[1:]):
        if a <= n <= b:
            return table[a] + (table[b] - table[a]) * (n - a) / (b - a)


# Face scale-up and minimum line width (output px) by icon size.
BOOST = {14: 1.5, 16: 1.45, 20: 1.35, 24: 1.3, 32: 1.2, 48: 1.1, 64: 1.15, 128: 1.35}
MINLINE = {14: 1.2, 16: 1.25, 20: 1.3, 24: 1.4, 32: 1.6, 48: 2.0, 64: 2.0, 128: 2.4}


def hexc(h):
    return np.array([int(h[i:i + 2], 16) for i in (1, 3, 5)], float) / 255


def blob_mask(u, v):
    """art.py's shape: four elliptical corners on the 132×116 box."""
    tl = (u < 62.04) & (v < 63.8)
    tr = (u >= 62.04) & (v < 60.32)
    br = (u >= 72.6) & (v >= 60.32)
    bl = (u < 72.6) & (v >= 63.8)
    inside = (u >= 0) & (u <= W) & (v >= 0) & (v <= H)
    inside &= ~tl | (((u - 62.04) / 62.04) ** 2 + ((v - 63.8) / 63.8) ** 2 <= 1)
    inside &= ~tr | (((u - 62.04) / 69.96) ** 2 + ((v - 60.32) / 60.32) ** 2 <= 1)
    inside &= ~br | (((u - 72.6) / 59.4) ** 2 + ((v - 60.32) / 55.68) ** 2 <= 1)
    inside &= ~bl | (((u - 72.6) / 72.6) ** 2 + ((v - 63.8) / 52.2) ** 2 <= 1)
    return inside


def body(n, pal, rim, rimw):
    """The blob, width n, vertically centred on an n×n canvas, at SSR× (float RGBA)
    with a rim of `rimw` output px; also returns its box (x0, y0, w, h) in output px."""
    m = n * SSR
    t = (np.arange(m) + 0.5) / SSR
    X, Y = np.meshgrid(t, t)
    bw, bh = n, n * H / W
    x0, y0 = 0.0, (n - bh) / 2
    u = (X - x0) / bw * W
    v = (Y - y0) / bh * H
    outer = blob_mask(u, v)
    ui = W / 2 + (u - W / 2) * bw / (bw - 2 * rimw)
    vi = H / 2 + (v - H / 2) * bh / (bh - 2 * rimw)
    inner = blob_mask(ui, vi)
    a_, b_, c_ = (hexc(c) for c in pal)
    ang = math.radians(150)
    dx, dy = math.sin(ang), -math.cos(ang)
    L = abs(W * dx) + abs(H * dy)
    tt = np.clip(((ui - W / 2) * dx + (vi - H / 2) * dy) / L + 0.5, 0, 1)[..., None]
    col = np.where(tt < 0.55, a_ + (b_ - a_) * (tt / 0.55), b_ + (c_ - b_) * ((tt - 0.55) / 0.45))
    shade = np.clip((vi - H * 0.62) / (H * 0.38), 0, 1)[..., None] ** 1.6 * 0.24
    col = col * (1 - shade) + np.array([25, 45, 120]) / 255 * shade
    light = np.clip(1 - vi / (H * 0.2), 0, 1)[..., None] ** 2 * 0.45
    col = col * (1 - light) + light
    gx, gy, rx, ry, rot = W * 0.38, H * 0.21, W * 0.19, H * 0.105, math.radians(-16)
    gu = ((ui - gx) * math.cos(rot) + (vi - gy) * math.sin(rot)) / rx
    gv = (-(ui - gx) * math.sin(rot) + (vi - gy) * math.cos(rot)) / ry
    g = (np.clip(1 - np.sqrt(gu * gu + gv * gv), 0, 1) ** 1.2 * 0.9)[..., None]
    col = col * (1 - g) + g
    rgb = np.where(inner[..., None], col, hexc(rim))
    return np.dstack([rgb, outer.astype(float)]), (x0, y0, bw, bh)


def arc(cx, cy, rx, ry, t0, t1, steps=40):
    return [(cx + rx * math.cos(t), cy + ry * math.sin(t)) for t in np.linspace(t0, t1, steps)]


def face_shapes(mood, n):
    """stage.slint's marks in face units (52×28 box)."""
    S = []
    if mood == "idle":  # round eyes and a small smile
        S += [("cap", 9, 0, 8, 13), ("cap", 35, 0, 8, 13)]
        S.append(("line", arc(26, 18, 6, 6, pi, 0), 2.2))
    elif mood == "happy":  # arched eyes, open smile
        S += [("line", arc(13.5, 10, 5.5, 6, pi, 2 * pi), 2.4),
              ("line", arc(38.5, 10, 5.5, 6, pi, 2 * pi), 2.4)]
        S.append(("fill", [(17, 18), (35, 18)] + arc(26, 18, 9, 9, 0, pi)))
    elif mood == "dizzy":
        if n >= 64:  # spiral eyes; they only resolve at Dock size
            for cx in (13.5, 38.5):
                turns, r = 2.0, 6.6
                ph = 0 if cx < 26 else pi
                pts = [(cx + r * (t / (turns * 2 * pi)) * math.cos(t + ph),
                        6.5 + r * (t / (turns * 2 * pi)) * math.sin(t + ph))
                       for t in np.linspace(0.01, turns * 2 * pi, 80)]
                S.append(("line", pts, 1.5))
        else:  # X eyes
            for cx in (13.5, 38.5):
                s = 4.2
                S += [("line", [(cx - s, 6.5 - s), (cx + s, 6.5 + s)], 2.6),
                      ("line", [(cx - s, 6.5 + s), (cx + s, 6.5 - s)], 2.6)]
        waves = 2 if n >= 48 else 1.5
        amp = 2.2 if n >= 24 else 1.4
        S.append(("line", [(17 + i * 18 / 30, 22 + amp * math.sin(i / 30 * waves * 2 * pi))
                           for i in range(31)], 2.3))
    return S


def draw_face(img, n, box, mood):
    x0, y0, bw, bh = box
    k = bw / W * interp(BOOST, n) * SSR  # hi-res px per face unit
    fcx = (x0 + bw / 2) * SSR
    fcy = (y0 + bh * 0.44) * SSR + 14 * (bw / W) * SSR
    ox, oy = fcx - 26 * k, fcy - 14 * k
    d = ImageDraw.Draw(img)
    minl = interp(MINLINE, n) * SSR
    P = lambda x, y: (ox + x * k, oy + y * k)
    for s in face_shapes(mood, n):
        if s[0] == "cap":
            _, x, y, w, h = s
            w2 = max(w * k, minl * 0.95)
            h2 = max(h * k, minl * 1.4)
            cx, cy = P(x + w / 2, y + h / 2)
            d.rounded_rectangle([cx - w2 / 2, cy - h2 / 2, cx + w2 / 2, cy + h2 / 2],
                                radius=min(w2, h2) / 2, fill=FACE)
        elif s[0] == "line":
            _, pts, lw = s
            lw2 = max(lw * k, minl)
            q = [P(*p) for p in pts]
            d.line(q, fill=FACE, width=int(round(lw2)), joint="curve")
            for e in (q[0], q[-1]):
                d.ellipse([e[0] - lw2 / 2, e[1] - lw2 / 2, e[0] + lw2 / 2, e[1] + lw2 / 2], fill=FACE)
        else:
            d.polygon([P(*p) for p in s[1]], fill=FACE)


def stroke_mask(size, paths, width):
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    for q in paths:
        d.line(q, fill=255, width=int(round(width)), joint="curve")
        for e in (q[0], q[-1]):
            d.ellipse([e[0] - width / 2, e[1] - width / 2, e[0] + width / 2, e[1] + width / 2], fill=255)
    return m


def apply_badge(hr, kind, n):
    """A check or X in the top-right corner (no disc): a transparent gap is cut out of the
    blob around it, and a thin darker edge sharpens it on light taskbars."""
    frac = 0.47 if n <= 16 else 0.43 if n <= 24 else 0.38 if n <= 32 else 0.32
    g = n * frac * SSR
    pad = max(0.4, n * 0.02) * SSR
    x1, y0 = n * SSR - pad, pad
    x0, y1 = x1 - g, y0 + g
    sw = max(1.7 * SSR, g * (0.26 if n <= 20 else 0.22))
    i = sw / 2
    if kind == "x":
        paths = [[(x0 + i, y0 + i), (x1 - i, y1 - i)], [(x0 + i, y1 - i), (x1 - i, y0 + i)]]
        fill, edge = RED, RED_EDGE
    else:
        paths = [[(x0 + i, y0 + g * 0.55), (x0 + g * 0.38, y1 - i * 1.1), (x1 - i, y0 + i * 1.3)]]
        fill, edge = GREEN, GREEN_EDGE
    size = hr.width
    edge_w = max(0.5 * SSR, sw * 0.12)
    gap = max(0.7 * SSR, n * 0.035 * SSR)
    a = np.array(hr).astype(float)
    a[..., 3] *= 1 - np.array(stroke_mask(size, paths, sw + 2 * edge_w + 2 * gap), float) / 255
    for mask, col in ((stroke_mask(size, paths, sw + 2 * edge_w), edge), (stroke_mask(size, paths, sw), fill)):
        m = np.array(mask, float)[..., None] / 255
        a[..., :3] = a[..., :3] * (1 - m) + np.array(col) * m
        a[..., 3:] = np.maximum(a[..., 3:], m * 255)
    return Image.fromarray(a.clip(0, 255).astype(np.uint8), "RGBA")


def downsample(img, n):
    """Box filter weighted by coverage (no dark fringes)."""
    a = np.array(img).astype(float) / 255
    a[..., :3] *= a[..., 3:]
    a = a.reshape(n, SSR, n, SSR, 4).mean(axis=(1, 3))
    a[..., :3] /= np.maximum(a[..., 3:], 1e-9)
    return Image.fromarray((np.clip(a, 0, 1) * 255 + 0.5).astype(np.uint8), "RGBA")


def mood_icon(n, mood, tint):
    """A full-bleed n px tray icon for idle / done / error."""
    t = TINTS[tint]
    rimw = 0.6 if n <= 32 else n * 0.012
    badge = {"done": "check", "error": "x"}.get(mood)
    face = {"idle": "idle", "done": "happy", "error": "dizzy"}[mood]
    if badge and n <= 16:
        face = None  # 16 px: blob + badge only, a face there is mush
    # With a badge the body shrinks a little and sits bottom-left, clear of it.
    m = n if not badge else n - (3 if n <= 20 else 4 if n <= 32 else round(n * 0.10))
    arr, box = body(m, t["pal"], t["rim"], rimw)
    b = Image.fromarray((np.clip(arr, 0, 1) * 255 + 0.5).astype(np.uint8), "RGBA")
    if face:
        draw_face(b, m, box, face)
    hr = Image.new("RGBA", (n * SSR, n * SSR), (0, 0, 0, 0))
    hr.alpha_composite(b, (0, (n - m) * SSR))
    if badge:
        hr = apply_badge(hr, badge, n)
    return downsample(hr, n)


def dock_icon(n, mood, tint):
    """Dock / large app icon: the blob on 84% of the canvas (Apple's icon grid leaves a
    margin) with a soft shadow."""
    inner = round(n * 108 / 128)
    im = mood_icon(inner, mood, tint)
    canvas = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    sh = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    a = np.array(im)[..., 3]
    off = (n - inner) // 2
    sh.paste((10, 20, 50, 255), (off, off + round(n * 4 / 128)),
             Image.fromarray((a * 0.35).astype(np.uint8), "L"))
    canvas.alpha_composite(sh.filter(ImageFilter.GaussianBlur(n * 3 / 128)))
    canvas.alpha_composite(im, (off, off))
    return canvas


# ---------------------------------------------------------------- compressing (the press)
PS = 512  # frame canvas
K = 1.95  # design units → canvas px
_press = {}


def press_art():
    if not _press:
        pw = int(124 * K * 0.92)
        ph = int(504 * pw / 248)
        _press["top"] = Image.open(FE / "press-top.png").convert("RGBA").resize((pw, ph), Image.LANCZOS)
        _press["bot"] = Image.open(FE / "press-bottom.png").convert("RGBA").resize((pw, ph), Image.LANCZOS)
        _press["gloss"] = Image.open(FE / "blob-gloss.png").convert("RGBA")
        for tint, t in TINTS.items():
            _press[tint] = Image.open(FE / t["blob"]).convert("RGBA")
    return _press


def ease_in(u):
    return u * u * u


def ease_out(u):
    return 1 - (1 - u) ** 3


def lerp(a, b, u):
    return a + (b - a) * u


def pose(t):
    """(blob height scale, extra plate gap, face) at loop time t in 0..1."""
    if t < 0.14:  # wind-up: the press lifts, the blob stretches up
        u = ease_out(t / 0.14)
        return lerp(1.0, 1.10, u), lerp(0, 18, u), "busy"
    if t < 0.26:  # slam
        u = ease_in((t - 0.14) / 0.12)
        return lerp(1.10, 0.24, u), lerp(18, 0, u), "busy" if u < 0.5 else "squished"
    if t < 0.34:  # impact rebound
        u = (t - 0.26) / 0.08
        return 0.24 + 0.08 * math.sin(u * pi) * (1 - u) + 0.06 * u, 0, "squished"
    if t < 0.60:  # hold, trembling
        u = (t - 0.34) / 0.26
        return 0.30 + 0.025 * math.sin(u * 2 * pi * 6), 0, "squished"
    if t < 0.70:  # release: the press flies open, the blob pops up tall
        u = ease_out((t - 0.60) / 0.10)
        return lerp(0.30, 1.32, u), lerp(0, 60, u), "pop"
    u = (t - 0.70) / 0.30  # wobble back to round, the press settles on it
    sy = 1 + 0.32 * math.exp(-4.5 * u) * math.cos(u * 3.2 * pi)
    return sy, 60 * (1 - ease_out(u)), "busy" if u > 0.5 else "pop"


def _stroke(d, pts, w):
    d.line(pts, fill=FACE + (255,), width=max(1, round(w)), joint="curve")
    r = w / 2
    for x, y in (pts[0], pts[-1]):
        d.ellipse((x - r, y - r, x + r, y + r), fill=FACE + (255,))


def press_frame(t, tint):
    """One 512 px compressing frame: cartoon squash (wider when flat, thinner when tall)."""
    art = press_art()
    sy, gap, face = pose(t)
    sx = sy ** -0.25 if sy < 1 else 1 / sy ** 0.7
    img = Image.new("RGBA", (PS, PS), (0, 0, 0, 0))
    bw, bh = int(132 * K * sx), max(8, int(116 * K * sy))
    bx, by = (PS - bw) // 2, (PS - bh) // 2
    img.alpha_composite(art[tint].resize((bw, bh), Image.LANCZOS), (bx, by))
    img.alpha_composite(art["gloss"].resize((bw, bh), Image.LANCZOS), (bx, by))
    d = ImageDraw.Draw(img)
    kx, ky = K * min(sx, 1.35), K * max(sy, 0.55)  # the face squashes, but stays readable
    fx, fy = PS / 2 - 26 * kx, by + bh * 0.42 - 6 * ky
    if face == "squished":  # > <  and a wavy strained mouth
        _stroke(d, [(fx + 8 * kx, fy + 2 * ky), (fx + 18 * kx, fy + 6.5 * ky), (fx + 8 * kx, fy + 11 * ky)], 3.2 * K)
        _stroke(d, [(fx + 44 * kx, fy + 2 * ky), (fx + 34 * kx, fy + 6.5 * ky), (fx + 44 * kx, fy + 11 * ky)], 3.2 * K)
        _stroke(d, [(fx + (14 + i) * kx, fy + (19 + 2.2 * math.sin(i / 3 * pi)) * ky) for i in range(25)], 2.6 * K)
    elif face == "pop":  # wide round eyes and a little "o": boing!
        for ex in (13, 39):
            r = 5.5 * K
            d.ellipse((fx + ex * kx - r, fy + 6 * ky - r * 1.2, fx + ex * kx + r, fy + 6 * ky + r * 1.2), fill=FACE)
        r = 4 * K
        d.ellipse((PS / 2 - r, fy + 21 * ky - r, PS / 2 + r, fy + 21 * ky + r), outline=FACE, width=round(2.4 * K))
    else:  # the app's busy squint
        _stroke(d, [(fx + 8 * kx, fy + 4 * ky), (fx + 19 * kx, fy + 8 * ky)], 3 * K)
        _stroke(d, [(fx + 33 * kx, fy + 8 * ky), (fx + 44 * kx, fy + 4 * ky)], 3 * K)
        r = 3.5 * K
        d.ellipse((PS / 2 - r, fy + 20 * ky - r, PS / 2 + r, fy + 20 * ky + r), outline=FACE, width=round(2 * K))
    off = bh / 2 + gap  # plates touch the blob (plus the wind-up / release gap)
    pw, ph = art["top"].size
    img.alpha_composite(art["top"], ((PS - pw) // 2, int(PS / 2 - off - ph + 2)))
    img.alpha_composite(art["bot"], ((PS - pw) // 2, int(PS / 2 + off - 2)))
    return img


# Small icons crop in on the centre so the blob stays big (the rods matter least).
CROP = {16: 0.72, 20: 0.78, 24: 0.86, 32: 0.94, 48: 0.97}


def busy_icon(frame, n):
    c = CROP[n]
    m = PS * (1 - c) / 2
    return frame.crop((round(m), round(m), round(PS - m), round(PS - m))).resize((n, n), Image.LANCZOS)


def busy_dock(frame, n):
    """Dock frame: the press on 92% of the canvas."""
    inner = round(n * 0.92)
    out = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    out.alpha_composite(frame.resize((inner, inner), Image.LANCZOS), ((n - inner) // 2, (n - inner) // 2))
    return out


# ---------------------------------------------------------------- output
def strip(images):
    n = images[0].height
    s = Image.new("RGBA", (n * len(images), n), (0, 0, 0, 0))
    for i, im in enumerate(images):
        s.alpha_composite(im, (i * n, 0))
    return s


def clean(im):
    """Zero the colour of fully transparent pixels (smaller PNGs, same look)."""
    a = np.array(im)
    a[a[..., 3] == 0] = 0
    return Image.fromarray(a, "RGBA")


def save(im, path, colors=None):
    im = clean(im)
    if colors:  # palette PNG with alpha
        im = im.quantize(colors=colors, method=Image.Quantize.FASTOCTREE, dither=Image.Dither.NONE)
    im.save(path, optimize=True)


def main():
    OUT.mkdir(exist_ok=True)
    for tint in TINTS:
        frames = [press_frame(i / FRAMES, tint) for i in range(FRAMES)]
        for n in TRAY_SIZES:
            save(strip([mood_icon(n, m, tint) for m in MOODS]), OUT / f"{tint}-moods-{n}.png")
            save(strip([busy_icon(f, n) for f in frames]), OUT / f"{tint}-busy-{n}.png", PALETTE)
        for m in MOODS:
            if (tint, m) != ("kompacter", "idle"):
                save(dock_icon(DOCK, m, tint), OUT / f"{tint}-dock-{m}.png")
        for i, f in enumerate(frames):
            save(busy_dock(f, DOCK_BUSY), OUT / f"{tint}-dock-busy-{i:02}.png", PALETTE)

    # The static app icon: the idle Kompacter blob. Small sizes full-bleed like the tray,
    # 128/256 with the Dock margin (the macOS and Linux bundles use the 256 px one).
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    icons = {n: (dock_icon(n, "idle", "kompacter") if n >= 128 else mood_icon(n, "idle", "kompacter"))
             for n in sizes}
    icons[256].save("icon.ico", format="ICO", sizes=[(n, n) for n in sizes],
                    append_images=[icons[n] for n in sizes if n != 256])
    save(icons[32], FE / "tray.png")


if __name__ == "__main__":
    main()
