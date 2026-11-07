"""Pre-draws the blob's body and the press heads as PNGs for ui/.

Slint's lightweight CPU renderer can't round or cut gradient shapes, so these are drawn
once here and shipped as small images; they look the same in both drawing modes.
Run: python3 tools/art.py (needs numpy + Pillow).
"""
import numpy as np
from PIL import Image

W, H, S = 132, 116, 3  # design size, drawn at 3x for sharp high-DPI screens
SS = 4                 # supersampling for smooth edges

def hexc(h):
    return np.array([int(h[i:i + 2], 16) for i in (1, 3, 5)], float) / 255

def body(a, b, c, path, gloss_path=None):
    n = W * S * SS
    m = H * S * SS
    x = (np.arange(n) + 0.5) / (S * SS)
    y = (np.arange(m) + 0.5) / (S * SS)
    X, Y = np.meshgrid(x, y)
    # Four elliptical corners (CSS border-radius 47% 53% 45% 55% / 55% 52% 48% 45%).
    tl = (X < 62.04) & (Y < 63.8)
    tr = (X >= 62.04) & (Y < 60.32)
    br = (X >= 72.6) & (Y >= 60.32)
    bl = (X < 72.6) & (Y >= 63.8)
    inside = np.ones_like(X, bool)
    inside &= ~tl | (((X - 62.04) / 62.04) ** 2 + ((Y - 63.8) / 63.8) ** 2 <= 1)
    inside &= ~tr | (((X - 62.04) / 69.96) ** 2 + ((Y - 60.32) / 60.32) ** 2 <= 1)
    inside &= ~br | (((X - 72.6) / 59.4) ** 2 + ((Y - 60.32) / 55.68) ** 2 <= 1)
    inside &= ~bl | (((X - 72.6) / 72.6) ** 2 + ((Y - 63.8) / 52.2) ** 2 <= 1)

    # 150° linear gradient: a → b (55%) → c.
    ang = np.radians(150)
    dx, dy = np.sin(ang), -np.cos(ang)
    L = abs(W * dx) + abs(H * dy)
    t = np.clip(((X - W / 2) * dx + (Y - H / 2) * dy) / L + 0.5, 0, 1)[..., None]
    A, B, C = hexc(a), hexc(b), hexc(c)
    col = np.where(t < 0.55, A + (B - A) * (t / 0.55), B + (C - B) * ((t - 0.55) / 0.45))

    # Inner shade at the bottom, inner light at the top.
    shade = np.clip((Y - H * 0.62) / (H * 0.38), 0, 1)[..., None] ** 1.6 * 0.24
    col = col * (1 - shade) + np.array([25, 45, 120]) / 255 * shade
    light = np.clip(1 - Y / (H * 0.2), 0, 1)[..., None] ** 2 * 0.45
    col = col * (1 - light) + light

    # Gloss: a soft white ellipse, tilted, at the top left. Its own image, so the body can
    # slowly turn (morph) while the highlight stays put.
    gx, gy, rx, ry, rot = W * 0.38, H * 0.21, W * 0.19, H * 0.105, np.radians(-16)
    u = ((X - gx) * np.cos(rot) + (Y - gy) * np.sin(rot)) / rx
    v = (-(X - gx) * np.sin(rot) + (Y - gy) * np.cos(rot)) / ry
    g = np.clip(1 - np.sqrt(u * u + v * v), 0, 1) ** 1.2 * 0.9
    if gloss_path:
        ga = (g * inside).reshape(H * S, SS, W * S, SS).mean(axis=(1, 3))
        gl = np.dstack([np.ones_like(ga), np.ones_like(ga), np.ones_like(ga), ga])
        Image.fromarray((np.clip(gl, 0, 1) * 255 + 0.5).astype(np.uint8), "RGBA").save(gloss_path, optimize=True)

    rgba = np.dstack([col, inside.astype(float)])
    rgba = rgba.reshape(H * S, SS, W * S, SS, 4).mean(axis=(1, 3))
    # Premultiply-safe: color where alpha is 0 doesn't matter.
    img = Image.fromarray((np.clip(rgba, 0, 1) * 255 + 0.5).astype(np.uint8), "RGBA")
    img.save(path, optimize=True)

def shadow(path):
    w, h = int(W * 0.64 * S), 12 * S
    x = (np.arange(w) + 0.5) / w * 2 - 1
    y = (np.arange(h) + 0.5) / h * 2 - 1
    X, Y = np.meshgrid(x, y)
    a = np.clip(1 - np.sqrt(X * X + Y * Y), 0, 1) * 0.22
    img = np.zeros((h, w, 4))
    img[..., 3] = a
    Image.fromarray((img * 255 + 0.5).astype(np.uint8), "RGBA").save(path, optimize=True)

body("#e9f2ff", "#9cc4ff", "#5b8cff", "ui/blob-light.png", "ui/blob-gloss.png")
body("#cfe2ff", "#7aa9ff", "#3d6fff", "ui/blob-dark.png")
body("#e6e8ff", "#9aa4ff", "#5865f2", "ui/blob-discord-light.png")
body("#d6daff", "#8891f2", "#5865f2", "ui/blob-discord-dark.png")
shadow("ui/blob-shadow.png")

# Flyout shadow with a 34 px nine-slice border, stretched only along straight edges.
y, x = np.mgrid[0.5:72, 0.5:72]
dx = np.maximum(np.abs(x - 36) - 2, 0)
dy = np.maximum(np.abs(y - 38) - 2, 0)
distance = np.maximum(np.hypot(dx, dy) - 22, 0)
flyout = np.zeros((72, 72, 4), dtype=np.uint8)
flyout[..., 3] = np.rint(108 * np.exp(-0.5 * (distance / 4.5) ** 2))
Image.fromarray(flyout, "RGBA").save("ui/flyout-shadow.png", optimize=True)


def press_head(path_top, path_bottom, scale=2):
    """One hydraulic head: pole, hazard collar, trapezoid, plate (124×252 units)."""
    W2, H2 = 124, 252
    n, m = W2 * scale * SS, H2 * scale * SS
    x = (np.arange(n) + 0.5) / (scale * SS)
    y = (np.arange(m) + 0.5) / (scale * SS)
    X, Y = np.meshgrid(x, y)
    rgb = np.zeros((m, n, 3))
    a = np.zeros((m, n))

    stops = [(0, "#7b8088"), (.22, "#d4d7dc"), (.38, "#ffffff"), (.6, "#b9bdc4"), (.8, "#d4d7dc"), (1, "#8f949c")]
    def metal(x0, w):
        t = np.clip((X - x0) / w, 0, 1)
        out = np.zeros_like(rgb)
        for (p0, c0), (p1, c1) in zip(stops, stops[1:]):
            k = ((t - p0) / (p1 - p0))[..., None]
            seg = (t >= p0) & (t <= p1)
            out[seg] = (hexc(c0) + (hexc(c1) - hexc(c0)) * k)[seg]
        return out
    def paint(mask, color):
        rgb[mask] = color[mask]
        a[mask] = 1

    # Pole.
    pw = 33.5; px = (W2 - pw) / 2
    pole = (X >= px) & (X <= px + pw) & (Y < 200)
    col = metal(px, pw) * 0.94 + 0.06 * np.clip(1 - np.abs((X - px - pw / 2) / (pw / 2)), 0, 1)[..., None]
    paint(pole, col)

    # Hazard collar: yellow and black stripes at 45°, rounded corners.
    cw = 43.4; cx = (W2 - cw) / 2
    collar = (X >= cx) & (X <= cx + cw) & (Y >= 200) & (Y < 214)
    r = 3
    for (ax, ay) in [(cx + r, 200 + r), (cx + cw - r, 200 + r), (cx + r, 214 - r), (cx + cw - r, 214 - r)]:
        corner = (np.abs(X - ax) <= r) & (np.abs(Y - ay) <= r) & (((X < cx + r) | (X > cx + cw - r)) & ((Y < 200 + r) | (Y > 214 - r)))
        collar &= ~corner | ((X - ax) ** 2 + (Y - ay) ** 2 <= r * r)
    stripe = np.mod(X + Y, 10) < 5
    ccol = np.where(stripe[..., None], hexc("#141414"), hexc("#f5c400"))
    # Soft top light and bottom shade on the collar.
    ccol = ccol + np.clip(1 - (Y - 200) / 3, 0, 1)[..., None] * 0.25
    ccol = ccol * (1 - np.clip((Y - 211) / 3, 0, 1)[..., None] * 0.35)
    paint(collar, np.clip(ccol, 0, 1))

    # Trapezoid: narrow at the top, wide at the plate.
    tw = 101.7; tx = (W2 - tw) / 2; inset = 11.8
    k = (Y - 214) / 26
    trap = (Y >= 214) & (Y < 240) & (X >= tx + inset * (1 - k)) & (X <= tx + tw - inset * (1 - k))
    tcol = metal(tx, tw)
    tcol = tcol * (1 - 0.16 * k[..., None]) + 0.28 * (1 - k[..., None]) * (1 - tcol)
    paint(trap, np.clip(tcol, 0, 1))

    # Plate.
    plate = (Y >= 240) & (Y < 252)
    pk = ((Y - 240) / 12)[..., None]
    pcol = metal(0, W2)
    pcol = pcol + (1 - pcol) * np.clip(0.55 * (1 - pk / 0.45), 0, 0.55) - pcol * 0.2 * np.clip((pk - 0.45) / 0.55, 0, 1)
    pcol[(Y >= 240) & (Y < 241)] = pcol[(Y >= 240) & (Y < 241)] * 0.2 + 0.8
    paint(plate, np.clip(pcol, 0, 1))

    img = np.dstack([rgb, a]).reshape(H2 * scale, SS, W2 * scale, SS, 4).mean(axis=(1, 3))
    im = Image.fromarray((np.clip(img, 0, 1) * 255 + 0.5).astype(np.uint8), "RGBA")
    im.save(path_top, optimize=True)
    im.transpose(Image.FLIP_TOP_BOTTOM).save(path_bottom, optimize=True)

press_head("ui/press-top.png", "ui/press-bottom.png")
