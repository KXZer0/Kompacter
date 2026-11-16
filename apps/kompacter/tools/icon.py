"""Draws the app logo: the blue blob with the mascot's face, as icon.ico and ui/tray.png.

The .ico holds every size Windows asks for (taskbar, Start, Explorer, the installer and
the "Kompact" entry in Explorer's menu, which uses the exe's icon). Big sizes are drawn
at high resolution and scaled down, with the face of the blob in ui/stage.slint (52×28
units: two rounded eyes and a small smile). Scaled down as is, that face turns to mush,
so from 64 px down it is drawn bolder and on whole pixels, and at 16-24 px pixel by pixel.
Run from apps/kompacter: python3 tools/icon.py (needs numpy + Pillow).
"""
import numpy as np
from PIL import Image

FACE = np.array([0x13, 0x21, 0x3F], float) / 255  # Theme.face
SS = 8  # supersampling for smooth edges

# The body, on a 256 px canvas: an ellipse, lit from the top left.
CX, CY, RX, RY = 128, 133, 100, 90
LIGHT_X, LIGHT_Y = 115, 105.5


def app_face(n):
    """The face as in the app, for an n px icon: the blob is 132 units wide there (200 px
    of 256 here) and the face sits 0.44 of the way down it."""
    k = 2 * RX / 132 * n / 256
    top = (CY - RY + 2 * RY * 0.44) * n / 256
    return dict(ew=8 * k, eh=13 * k, ex=13 * k, top=top, my=top + 18 * k, mr=6 * k, line=2.2 * k)


def capsule(X, Y, x, y, w, h):
    """A rounded rectangle whose short side is fully round (an eye)."""
    r = min(w, h) / 2
    qx = np.maximum(np.abs(X - (x + w / 2)) - (w / 2 - r), 0)
    qy = np.maximum(np.abs(Y - (y + h / 2)) - (h / 2 - r), 0)
    return np.hypot(qx, qy) <= r


def smile(X, Y, cx, cy, r, line):
    """A lower half circle stroked with round caps (the idle mouth, "M0 0A6 6 0 0 0 12 0")."""
    d = np.where(Y >= cy, np.abs(np.hypot(X - cx, Y - cy) - r),
                 np.minimum(np.hypot(X - (cx - r), Y - cy), np.hypot(X - (cx + r), Y - cy)))
    return d <= line / 2


def render(n, face=None):
    """The logo at n px. `face` is in output pixels: eye size (ew × eh), eye centers ±ex
    from the middle, eye top, and the smile's center height, radius and line width."""
    m = n * SS
    t = (np.arange(m) + 0.5) / SS  # pixel coordinates, supersampled
    X, Y = np.meshgrid(t, t)
    s = 256 / n
    inside = ((X * s - CX) / RX) ** 2 + ((Y * s - CY) / RY) ** 2 <= 1
    # Radial light: #96d2ff in the middle, darkening evenly toward the edge.
    r = np.clip(149.5 - 0.462 * np.hypot(X * s - LIGHT_X, Y * s - LIGHT_Y), 0, 255) / 255
    rgb = np.dstack([r, np.clip(r + 60 / 255, 0, 1), np.ones_like(r)])

    if face:
        f, c = face, n / 2
        mark = capsule(X, Y, c - f["ex"] - f["ew"] / 2, f["top"], f["ew"], f["eh"])
        mark |= capsule(X, Y, c + f["ex"] - f["ew"] / 2, f["top"], f["ew"], f["eh"])
        mark |= smile(X, Y, c, f["my"], f["mr"], f["line"])
        rgb = np.where(mark[..., None], FACE, rgb)

    rgba = np.dstack([rgb, inside.astype(float)]).reshape(n, SS, n, SS, 4)
    # Average color weighted by coverage, so the edge doesn't pick up black from outside.
    a = rgba[..., 3].mean(axis=(1, 3))
    w = rgba[..., 3:].sum(axis=(1, 3))
    col = (rgba[..., :3] * rgba[..., 3:]).sum(axis=(1, 3)) / np.maximum(w, 1e-9)
    out = np.dstack([col, a])
    return Image.fromarray((np.clip(out, 0, 1) * 255 + 0.5).astype(np.uint8), "RGBA")


def pixels(n, marks):
    """Small sizes: the body scaled down, then the face set pixel by pixel. `marks` are
    (x, y, strength); less than 1 blends in the face color for softer corners."""
    a = np.array(render(n)).astype(float) / 255
    for x, y, k in marks:
        a[y, x, :3] = a[y, x, :3] * (1 - k) + FACE * k
    return Image.fromarray((a * 255 + 0.5).astype(np.uint8), "RGBA")


def eye(x, y, w, h, corner=1.0):
    """A w × h block of pixels; `corner` < 1 softens its four corners."""
    return [(x + i, y + j, corner if w > 1 and i in (0, w - 1) and j in (0, h - 1) else 1.0)
            for i in range(w) for j in range(h)]


def mouth(x, y, w, tip):
    """A pixel smile w wide: the ends one row up at strength `tip`, the middle below."""
    return [(x, y, tip), (x + w - 1, y, tip)] + [(x + i, y + 1, 1.0) for i in range(1, w - 1)]


SIZES = {
    # 16-24: two eyes and a few pixels of smile, placed by hand.
    16: lambda: pixels(16, eye(6, 7, 1, 2) + eye(9, 7, 1, 2) + mouth(6, 10, 4, 0.55)),
    20: lambda: pixels(20, eye(6, 9, 2, 3, 0.6) + eye(12, 9, 2, 3, 0.6) + mouth(8, 13, 4, 0.6)),
    24: lambda: pixels(24, eye(8, 11, 2, 4, 0.7) + eye(14, 11, 2, 4, 0.7) + mouth(10, 16, 4, 0.7)),
    # 32-64: the real shapes, bolder than scaled, with eyes on whole pixels.
    32: lambda: render(32, dict(ew=2, eh=4, ex=4, top=14, my=20, mr=2, line=1.3)),
    40: lambda: render(40, dict(ew=2, eh=4, ex=5, top=18, my=24.4, mr=2.4, line=1.4)),
    48: lambda: render(48, dict(ew=2, eh=4, ex=5, top=22, my=28.4, mr=2.7, line=1.5)),
    64: lambda: render(64, dict(ew=3, eh=5, ex=6.5, top=30, my=37.6, mr=3.2, line=1.6)),
    128: lambda: render(128, app_face(128)),
    256: lambda: render(256, app_face(256)),
}

if __name__ == "__main__":
    images = {n: make() for n, make in SIZES.items()}
    images[256].save("icon.ico", format="ICO", sizes=[(n, n) for n in SIZES],
                     append_images=[images[n] for n in SIZES if n != 256])
    # Tray: one 32 px image, which Windows also shrinks to 16 px at 100% scaling, so its
    # face is bolder still: 2 px eyes and a 2 px smile still read as a face at half size.
    render(32, dict(ew=2, eh=4, ex=4, top=13, my=19.5, mr=2.3, line=1.8)).save("ui/tray.png", optimize=True)
