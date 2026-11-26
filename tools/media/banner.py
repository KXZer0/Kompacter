"""README banner: the two press heads squeeze the blob from the sides, then let go.

Uses the app's own art (apps/kompacter/frontend/*.png). Run from the repo root:
    python3 tools/media/banner.py      (needs Pillow + numpy, and ffmpeg for the GIF)
Writes docs/media/banner.gif.
"""
import math
import os
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFilter

UI = "apps/kompacter/frontend"
OUT = "docs/media/banner.gif"
W, H = 1280, 300
SS = 2               # drawn at 2x, scaled down for smooth edges
FPS = 30
SECONDS = 4.4
FACE = (19, 33, 63, 255)
BG_TOP, BG_BOT = (250, 251, 254), (232, 238, 250)


def load(name):
    return Image.open(os.path.join(UI, name)).convert("RGBA")


press_l = load("press-top.png").rotate(90, expand=True)     # plate faces right
press_r = load("press-bottom.png").rotate(90, expand=True)  # plate faces left
body_img = load("blob-light.png")
gloss_img = load("blob-gloss.png")
shadow_img = load("blob-shadow.png")

K = 1.55 * SS        # blob scale (132x116 design units)
PRESS_H = int(124 * K * 0.92)      # press thickness (vertical, once rotated)
PRESS_W = int(252 * K * 0.92)      # press length
ROD = W * SS // 2                   # extra rod so it always runs off the edge


def long_press(img, plate_right):
    """The press head with its rod stretched long (the rod is the same all along)."""
    img = img.resize((PRESS_W, PRESS_H), Image.LANCZOS)
    out = Image.new("RGBA", (PRESS_W + ROD, PRESS_H))
    e = 16  # skip the resized image's soft end
    col_x = e if plate_right else PRESS_W - e - 1
    rod = img.crop((col_x, 0, col_x + 1, PRESS_H)).resize((ROD + e, PRESS_H))
    if plate_right:
        out.alpha_composite(rod, (0, 0))
        out.alpha_composite(img.crop((e, 0, PRESS_W, PRESS_H)), (ROD + e, 0))
    else:
        out.alpha_composite(img.crop((0, 0, PRESS_W - e, PRESS_H)), (0, 0))
        out.alpha_composite(rod, (PRESS_W - e, 0))
    return out


PL = long_press(press_l, True)
PR = long_press(press_r, False)


def ease_out(t):
    return 1 - (1 - t) ** 3


def ease_in_out(t):
    return 0.5 - 0.5 * math.cos(math.pi * min(max(t, 0), 1))


def elastic(t):
    if t <= 0:
        return 0.0
    if t >= 1:
        return 1.0
    return 2 ** (-9 * t) * math.sin((t * 9 - 0.75) * (2 * math.pi) / 3) + 1


def background():
    bg = Image.new("RGBA", (W * SS, H * SS))
    d = ImageDraw.Draw(bg)
    for y in range(H * SS):
        t = y / (H * SS - 1)
        c = tuple(int(BG_TOP[i] + (BG_BOT[i] - BG_TOP[i]) * t) for i in range(3))
        d.line([(0, y), (W * SS, y)], fill=c + (255,))
    # A soft light behind the blob.
    glow = Image.new("L", (W * SS, H * SS), 0)
    ImageDraw.Draw(glow).ellipse(
        [W * SS / 2 - 360 * SS, H * SS / 2 - 150 * SS, W * SS / 2 + 360 * SS, H * SS / 2 + 150 * SS],
        fill=110)
    glow = glow.filter(ImageFilter.GaussianBlur(70 * SS))
    bg.paste(Image.new("RGBA", bg.size, (255, 255, 255, 255)), (0, 0), glow)
    return bg


BG = background()


def stroke(d, pts, k, ox, oy, width):
    xy = [(ox + x * k, oy + y * k) for x, y in pts]
    d.line(xy, fill=FACE, width=max(1, int(width * k)), joint="curve")
    r = width * k / 2
    for x, y in (xy[0], xy[-1]):
        d.ellipse([x - r, y - r, x + r, y + r], fill=FACE)


def arc_pts(cx, cy, rx, ry, a0, a1, n=24):
    return [(cx + rx * math.cos(math.radians(a0 + (a1 - a0) * i / n)),
             cy + ry * math.sin(math.radians(a0 + (a1 - a0) * i / n))) for i in range(n + 1)]


def face(d, mood, k, ox, oy, c=0.0, blink=0.0):
    """The app's face (stage.slint), 52x28 units at (ox, oy)."""
    if mood == "idle":
        eh = 13 * (1 - 0.88 * blink)
        for x in (9, 35):
            y = (13 - eh) * 0.6
            d.rounded_rectangle([ox + x * k, oy + y * k, ox + (x + 8) * k, oy + (y + eh) * k],
                                radius=min(8, eh) * k / 2, fill=FACE)
        stroke(d, arc_pts(26, 18, 6, 6, 0, 180), k, ox, oy, 2.2)
    elif mood == "busy":
        h = 3 + c * 3
        stroke(d, [(8, 4), (19, 4 + h)], k, ox, oy, 3)
        stroke(d, [(33, 4 + h), (44, 4)], k, ox, oy, 3)
        r = 3.5 + c * 1.2
        d.ellipse([ox + (26 - r) * k, oy + (20.5 - r) * k, ox + (26 + r) * k, oy + (20.5 + r) * k],
                  outline=FACE, width=int(2 * k))
    elif mood == "happy":
        for x in (8, 33):
            stroke(d, arc_pts(x + 5.5, 10, 5.5, 6, 180, 360), k, ox, oy, 2.4)
        d.chord([ox + 17 * k, oy + 9 * k, ox + 35 * k, oy + 27 * k], 0, 180, fill=FACE)


def blob(frame, cx, cy, sx, sy, mood, c=0.0, blink=0.0, lift=0.0):
    bw, bh = int(132 * K * sx), int(116 * K * sy)
    # Floor shadow (stays on the floor while the blob hops).
    sw, sh = int(132 * K * 0.64 * sx), int(12 * K)
    sh_img = shadow_img.resize((sw, sh), Image.LANCZOS)
    frame.alpha_composite(sh_img, (int(cx - sw / 2), int(cy + 116 * K / 2 - sh / 2 + 4 * SS)))
    x0, y0 = int(cx - bw / 2), int(cy + 116 * K / 2 - bh - lift)
    frame.alpha_composite(body_img.resize((bw, bh), Image.LANCZOS), (x0, y0))
    frame.alpha_composite(gloss_img.resize((bw, bh), Image.LANCZOS), (x0, y0))
    k = K * min(1.0, 0.5 + 0.5 * sx) * (1 + 0.15 * (sy - 1))
    d = ImageDraw.Draw(frame)
    face(d, mood, k, cx - 26 * k, y0 + bh * 0.44, c, blink)


def sparks(frame, cx, cy, t):
    if t <= 0 or t >= 1:
        return
    d = ImageDraw.Draw(frame)
    cols = [(0, 113, 227), (156, 196, 255)]
    for i in range(10):
        a = math.radians(i * 36 + 18)
        r = (110 + 90 * t) * SS
        s = (7 * (1 - 0.8 * t)) * SS
        x, y = cx + r * math.cos(a), cy + r * math.sin(a) * 0.75
        col = cols[i % 2] + (int(255 * (1 - t)),)
        d.ellipse([x - s, y - s, x + s, y + s], fill=col)


def state(t):
    """(squeeze c, press reach 0..1, mood, sparks t, blink, lift) at time t seconds."""
    blink = 0.0
    if 0.25 < t < 0.4:
        blink = math.sin((t - 0.25) / 0.15 * math.pi)
    # Presses come in (0.5-1.1 s), squeeze (1.1-2.4), snap back out (2.4-2.9); the blob
    # springs back, hops and cheers.
    c = 0.0
    if t < 0.5:
        reach = 0.0
    elif t < 1.1:
        reach = ease_out((t - 0.5) / 0.6)
    elif t < 2.4:
        reach = 1.0
        u = (t - 1.1) / 1.3
        c = ease_in_out(min(u / 0.5, 1)) * 0.8 + 0.035 * math.sin(u * math.pi * 7) * min(u * 3, 1)
    else:
        u = (t - 2.4) / 0.5
        reach = 1 - ease_out(min(u, 1))
        c = 0.8 * (1 - elastic((t - 2.4) / 0.7))
    if 1.0 < t < 2.45:
        mood = "busy"
    elif 2.45 <= t < 3.9:
        mood = "happy"
    else:
        mood = "idle"
    spark = (t - 2.5) / 0.6
    lift = 0.0
    if 2.5 < t < 3.2:
        u = (t - 2.5) / 0.7
        lift = 26 * SS * math.sin(u * math.pi)
    return c, reach, mood, spark, blink, lift


def render(t):
    frame = BG.copy()
    cx, cy = W * SS / 2, H * SS / 2 + 6 * SS
    c, reach, mood, spark, blink, lift = state(t)
    breath = (1 - math.cos(t / SECONDS * 2 * math.pi * 2)) / 2
    sx = (1 - 0.34 * c) * (1 + 0.015 * breath)
    sy = (1 + 0.16 * c) * (1 - 0.012 * breath)
    blob(frame, cx, cy, sx, sy, mood, c, blink, lift)
    sparks(frame, cx, cy, spark)
    # Plates: resting just inside the edges, touching the blob at full reach.
    half = 66 * K * sx + 1 * SS
    rest = W * SS / 2 - 150 * SS
    gap = rest + (half - rest) * reach
    py = int(cy - PRESS_H / 2)
    frame.alpha_composite(PL, (int(cx - gap) - PL.width, py))
    frame.alpha_composite(PR, (int(cx + gap), py))
    return frame.resize((W, H), Image.LANCZOS).convert("RGB")


def main():
    n = int(SECONDS * FPS)
    with tempfile.TemporaryDirectory() as tmp:
        for i in range(n):
            render(i / FPS).save(os.path.join(tmp, f"f{i:04d}.png"))
        pattern = os.path.join(tmp, "f%04d.png")
        vf = f"fps={FPS},split[a][b];[a]palettegen=max_colors=96:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle"
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-framerate", str(FPS), "-i", pattern,
                        "-vf", vf, "-loop", "0", OUT], check=True)
    print(OUT, os.path.getsize(OUT) // 1024, "KB")


if __name__ == "__main__":
    main()
