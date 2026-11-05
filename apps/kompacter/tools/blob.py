"""Draws the blob's body (gradient, inner light/shade, gloss) as PNGs for ui/.

Slint's lightweight CPU renderer can't round gradient shapes, so the body is pre-drawn
once here and shipped as small images. Run: python3 tools/blob.py (needs numpy + Pillow).
"""
import numpy as np
from PIL import Image

W, H, S = 132, 116, 3  # design size, drawn at 3x for sharp high-DPI screens
SS = 4                 # supersampling for smooth edges

def hexc(h):
    return np.array([int(h[i:i + 2], 16) for i in (1, 3, 5)], float) / 255

def body(a, b, c, path):
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

    # Gloss: a soft white ellipse, tilted, at the top left.
    gx, gy, rx, ry, rot = W * 0.38, H * 0.21, W * 0.19, H * 0.105, np.radians(-16)
    u = ((X - gx) * np.cos(rot) + (Y - gy) * np.sin(rot)) / rx
    v = (-(X - gx) * np.sin(rot) + (Y - gy) * np.cos(rot)) / ry
    g = np.clip(1 - np.sqrt(u * u + v * v), 0, 1)[..., None] ** 1.2 * 0.9
    col = col * (1 - g) + g

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

body("#e9f2ff", "#9cc4ff", "#5b8cff", "ui/blob-light.png")
body("#cfe2ff", "#7aa9ff", "#3d6fff", "ui/blob-dark.png")
shadow("ui/blob-shadow.png")
