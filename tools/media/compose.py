"""Puts frames from record.py on a made-up desktop and writes docs/media/<name>.gif.

    python3 tools/media/compose.py compress     (or trim, folder, settings, all)
"""
import json
import math
import os
import subprocess
import sys
import tempfile
from functools import lru_cache

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
TMP = os.environ.get("KOMPACTER_MEDIA_TMP", "/tmp/kompacter-media")
UI = os.path.join(ROOT, "apps", "kompacter", "frontend")
OUT = os.path.join(ROOT, "docs", "media")
FPS = 20
LAPSE_SECONDS = 2.6      # however long the job took, it plays in this long
FONT_DIR = "/usr/share/fonts/opentype/inter"


@lru_cache(None)
def font(size, weight="Regular"):
    return ImageFont.truetype(os.path.join(FONT_DIR, f"Inter-{weight}.otf"), size)


# ---------------------------------------------------------------- the desktop

def wallpaper(w, h):
    """Light, soft wallpaper: a pale gradient with a few big blurred color fields."""
    y = np.linspace(0, 1, h)[:, None, None]
    x = np.linspace(0, 1, w)[None, :, None]
    top = np.array([236, 242, 255], float)
    bot = np.array([246, 238, 252], float)
    img = top * (1 - y) + bot * y + 0 * x
    def field(cx, cy, r, col, a):
        d = np.sqrt(((x - cx) * w / r) ** 2 + ((y - cy) * h / r) ** 2)
        k = np.clip(1 - d, 0, 1) ** 2 * a
        return k * (np.array(col, float) - img)
    img = img + field(0.82, 0.25, 520, (170, 200, 255), 0.75)
    img = img + field(0.15, 0.95, 560, (215, 190, 255), 0.6)
    img = img + field(0.55, 0.65, 380, (190, 236, 246), 0.5)
    return Image.fromarray(np.clip(img, 0, 255).astype(np.uint8), "RGB").convert("RGBA")


def rounded(size, r, fill):
    im = Image.new("RGBA", size, (0, 0, 0, 0))
    ImageDraw.Draw(im).rounded_rectangle([0, 0, size[0] - 1, size[1] - 1], r, fill=fill)
    return im


def file_icon(kind, thumb=None):
    """A 56 px desktop icon: a video file with a thumbnail, or a folder."""
    s = 4
    im = Image.new("RGBA", (56 * s, 56 * s), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    if kind == "folder":
        d.rounded_rectangle([4 * s, 8 * s, 26 * s, 20 * s], 3 * s, fill=(226, 168, 38, 255))
        d.rounded_rectangle([4 * s, 13 * s, 52 * s, 48 * s], 4 * s, fill=(240, 186, 56, 255))
        d.rounded_rectangle([4 * s, 17 * s, 52 * s, 48 * s], 4 * s, fill=(252, 207, 86, 255))
        d.line([(8 * s, 20 * s), (48 * s, 20 * s)], fill=(255, 226, 140, 255), width=s)
    else:
        d.rounded_rectangle([5 * s, 9 * s, 51 * s, 47 * s], 4 * s, fill=(255, 255, 255, 255),
                            outline=(190, 198, 214, 255), width=s)
        if thumb is not None:
            t = thumb.resize((42 * s, 24 * s), Image.LANCZOS)
            im.paste(t, (7 * s, 11 * s))
        d.polygon([(24 * s, 19 * s), (24 * s, 29 * s), (33 * s, 24 * s)], fill=(255, 255, 255, 235))
        d.rounded_rectangle([7 * s, 37 * s, 21 * s, 44 * s], 2 * s, fill=(0, 113, 227, 255))
        d.text((14 * s, 40.5 * s), "MP4", font=font(5 * s, "Bold"), fill="white", anchor="mm")
    sh = im.split()[3].filter(ImageFilter.GaussianBlur(3 * s)).point(lambda a: a * 0.25)
    out = Image.new("RGBA", im.size, (0, 0, 0, 0))
    out.paste((30, 40, 80, 255), (0, 2 * s), sh)
    out.alpha_composite(im)
    return out.resize((56, 56), Image.LANCZOS)


def taskbar_icon(kind):
    s = 4
    im = Image.new("RGBA", (24 * s, 24 * s), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    if kind == "grid":       # a generic start button
        for i in range(2):
            for j in range(2):
                d.rounded_rectangle([(2 + i * 11) * s, (2 + j * 11) * s, (11 + i * 11) * s, (11 + j * 11) * s],
                                    2 * s, fill=(36, 120, 220, 255))
    elif kind == "search":
        d.ellipse([3 * s, 3 * s, 17 * s, 17 * s], outline=(60, 64, 76, 255), width=2 * s)
        d.line([(15 * s, 15 * s), (21 * s, 21 * s)], fill=(60, 64, 76, 255), width=2 * s)
    elif kind == "files":
        d.rounded_rectangle([2 * s, 5 * s, 12 * s, 11 * s], 2 * s, fill=(226, 168, 38, 255))
        d.rounded_rectangle([2 * s, 8 * s, 22 * s, 20 * s], 2 * s, fill=(252, 207, 86, 255))
    elif kind == "web":
        d.ellipse([2 * s, 2 * s, 22 * s, 22 * s], fill=(40, 160, 220, 255))
        d.ellipse([7 * s, 7 * s, 17 * s, 17 * s], fill=(255, 255, 255, 255))
        d.ellipse([9 * s, 9 * s, 15 * s, 15 * s], fill=(40, 120, 220, 255))
    elif kind == "chat":
        d.rounded_rectangle([2 * s, 3 * s, 22 * s, 18 * s], 6 * s, fill=(110, 98, 230, 255))
        d.polygon([(6 * s, 16 * s), (6 * s, 22 * s), (12 * s, 17 * s)], fill=(110, 98, 230, 255))
        for i in range(3):
            d.ellipse([(7 + i * 4.2) * s, 9.5 * s, (9.4 + i * 4.2) * s, 11.9 * s], fill="white")
    elif kind == "music":
        d.rounded_rectangle([2 * s, 2 * s, 22 * s, 22 * s], 6 * s, fill=(236, 72, 110, 255))
        d.ellipse([7 * s, 13 * s, 12 * s, 18 * s], fill="white")
        d.rectangle([10.5 * s, 6 * s, 12 * s, 16 * s], fill="white")
        d.polygon([(12 * s, 6 * s), (17 * s, 8 * s), (12 * s, 10 * s)], fill="white")
    return im.resize((24, 24), Image.LANCZOS)


@lru_cache(None)
def desktop(w, h, bar):
    img = wallpaper(w, h)
    d = ImageDraw.Draw(img)
    # Taskbar: a frosted light bar.
    strip = img.crop((0, h - bar, w, h)).filter(ImageFilter.GaussianBlur(12))
    strip = Image.blend(strip, Image.new("RGBA", strip.size, (246, 247, 251, 255)), 0.78)
    img.paste(strip, (0, h - bar))
    d.line([(0, h - bar), (w, h - bar)], fill=(214, 218, 228, 255))
    icons = ["grid", "search", "files", "web", "chat", "music"]
    x0 = w // 2 - len(icons) * 40 // 2
    for i, k in enumerate(icons):
        img.alpha_composite(taskbar_icon(k), (x0 + i * 40 + 8, h - bar + 12))
    # Tray: chevron, Kompacter's icon, wifi/volume glyphs, clock.
    tx = w - 210
    d.text((tx, h - bar / 2), "^", font=font(13), fill=(70, 74, 86), anchor="lm")
    tray = Image.open(os.path.join(UI, "tray.png")).convert("RGBA").resize((18, 18), Image.LANCZOS)
    img.alpha_composite(tray, (tx + 22, h - bar // 2 - 9))
    cx, cy = tx + 68, h - bar / 2
    for r in (9, 6, 3):
        d.arc([cx - r, cy - r + 4, cx + r, cy + r + 4], 225, 315, fill=(60, 64, 76), width=2)
    vx = tx + 90
    d.polygon([(vx, cy - 3), (vx + 4, cy - 3), (vx + 9, cy - 7), (vx + 9, cy + 7), (vx + 4, cy + 3), (vx, cy + 3)],
              fill=(60, 64, 76))
    d.arc([vx + 8, cy - 6, vx + 16, cy + 6], 300, 60, fill=(60, 64, 76), width=2)
    d.text((w - 16, h - bar / 2 - 8), "4:20 PM", font=font(12), fill=(30, 32, 40), anchor="rm")
    d.text((w - 16, h - bar / 2 + 9), "11/17/2026", font=font(12), fill=(30, 32, 40), anchor="rm")
    return img


def place_icons(img, icons):
    d = ImageDraw.Draw(img)
    for (x, y), icon, label in icons:
        img.alpha_composite(icon, (int(x - 28), int(y - 34)))
        for line_i, line in enumerate(label.split("\n")):
            ty = y + 30 + line_i * 15
            d.text((x, ty + 1), line, font=font(12, "Medium"), fill=(255, 255, 255, 200), anchor="mm")
            d.text((x, ty), line, font=font(12, "Medium"), fill=(28, 32, 44), anchor="mm")


# ---------------------------------------------------------------- cursor

def cursor_img():
    s = 4
    pts = [(0, 0), (0, 17), (4.2, 13.2), (7, 19.5), (9.6, 18.4), (6.9, 12.3), (12.2, 12.2)]
    im = Image.new("RGBA", (16 * s, 22 * s), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    p = [(1.5 * s + x * s, 1.5 * s + y * s) for x, y in pts]
    d.polygon(p, fill=(255, 255, 255, 255))
    d.line(p + [p[0]], fill=(10, 10, 14, 255), width=int(1.2 * s), joint="curve")
    sh = im.split()[3].filter(ImageFilter.GaussianBlur(1.5 * s)).point(lambda a: a * 0.35)
    out = Image.new("RGBA", im.size, (0, 0, 0, 0))
    out.paste((0, 0, 0, 255), (s, 2 * s), sh)
    out.alpha_composite(im)
    return out.resize((16, 22), Image.LANCZOS)


CURSOR = cursor_img()


# ---------------------------------------------------------------- frames

def unpremultiply(rgba):
    a = rgba[..., 3:4].astype(float) / 255
    rgb = np.where(a > 0, rgba[..., :3] / np.maximum(a, 1e-6), 0)
    return np.dstack([np.clip(rgb, 0, 255), rgba[..., 3:4]]).astype(np.uint8)


def compose_frame(base, win_png, f, icons_layer, ghost_icons, clicks_t):
    frame = base.copy()
    win = Image.fromarray(unpremultiply(np.array(Image.open(win_png))), "RGBA")
    wx, wy = f["win"]
    # Frosted glass: blur what's under the window where it's see-through.
    region = frame.crop((wx, wy, wx + win.width, wy + win.height)).filter(ImageFilter.GaussianBlur(18))
    mask = win.split()[3].point(lambda a: 255 if a > 200 else 0)
    frame.paste(region, (wx, wy), mask)
    frame.alpha_composite(win, (wx, wy))
    x, y = f["cursor"]
    if f.get("ghost"):
        g = ghost_icons[f["ghost"]].copy()
        g.putalpha(g.split()[3].point(lambda a: a * 0.75))
        frame.alpha_composite(g, (int(x - 20), int(y - 16)))
        d = ImageDraw.Draw(frame)
        tag = "+ Copy"
        tw = d.textlength(tag, font=font(11, "Medium"))
        d.rounded_rectangle([x + 14, y + 26, x + 26 + tw, y + 44], 4, fill=(255, 255, 255, 240),
                            outline=(200, 205, 216, 255))
        d.text((x + 20, y + 35), tag, font=font(11, "Medium"), fill=(30, 34, 44), anchor="lm")
    for age in clicks_t:
        if 0 <= age < 0.35:
            u = age / 0.35
            r = 6 + 16 * u
            ring = Image.new("RGBA", frame.size, (0, 0, 0, 0))
            ImageDraw.Draw(ring).ellipse([x - r, y - r, x + r, y + r], outline=(0, 113, 227, int(170 * (1 - u))),
                                         width=3)
            frame.alpha_composite(ring)
    frame.alpha_composite(CURSOR, (int(x) - 1, int(y) - 1))
    return frame.convert("RGB")


def timeline(frames):
    """Output time for each recorded frame: real time, except a time-lapse plays fast."""
    out, t_out, prev, lapse_start = [], 0.0, None, None
    lapse_n = sum(1 for f in frames if f["lapse"])
    for f in frames:
        if prev is not None:
            if f["lapse"]:
                t_out += LAPSE_SECONDS / max(lapse_n, 1)
            else:
                t_out += f["t"] - prev["t"]
        out.append(t_out)
        prev = f
    return out


def build(name, icons=()):
    src = os.path.join(TMP, "frames", name)
    log = json.load(open(os.path.join(src, "log.json")))
    frames = log["frames"]
    w, h, bar = log["desk"]
    base = desktop(w, h, bar).copy()
    place_icons(base, icons)
    ghost_icons = {k: ic for (_, ic, _), k in zip(icons, ("clip", "folder"))}
    times = timeline(frames)
    # Click moments: when `down` turns on.
    clicks = [times[i] for i in range(1, len(frames)) if frames[i]["down"] and not frames[i - 1]["down"]
              and not frames[i].get("ghost") and not frames[i - 1].get("ghost")]
    total = times[-1]
    n_out = int(total * FPS)
    with tempfile.TemporaryDirectory() as tmp:
        j = 0
        for k in range(n_out):
            t = k / FPS
            while j + 1 < len(frames) and times[j + 1] <= t:
                j += 1
            f = frames[j]
            img = compose_frame(base, os.path.join(src, f"{j:05d}.png"), f, None, ghost_icons,
                                [t - c for c in clicks])
            img.save(os.path.join(tmp, f"o{k:05d}.png"))
        out = os.path.join(OUT, f"{name}.gif")
        vf = (f"split[a][b];[a]palettegen=max_colors=200:stats_mode=diff[p];"
              f"[b][p]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle")
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-framerate", str(FPS), "-i",
                        os.path.join(tmp, "o%05d.png"), "-vf", vf, "-loop", "0", out], check=True)
        if len(sys.argv) > 2:
            for k in [k for k in sys.argv[2:] if int(k) < n_out]:
                Image.open(os.path.join(tmp, f"o{int(k):05d}.png")).save(os.path.join(TMP, f"peek-{name}-{k}.png"))
    print(out, f"{total:.1f}s", os.path.getsize(out) // 1024, "KB")


def clip_thumb():
    clip = os.path.join(TMP, "samples", "Kompacter intro.mp4")
    p = os.path.join(TMP, "thumb.png")
    if not os.path.exists(p):
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-ss", "20", "-i", clip, "-frames:v", "1",
                        "-vf", "scale=320:-1", p], check=True)
    return Image.open(p).convert("RGBA")


def main():
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    sys.path.insert(0, os.path.dirname(__file__))
    import record
    icons = [(record.ICON_CLIP, file_icon("clip", clip_thumb()), "Kompacter\nintro.mp4"),
             (record.ICON_FOLDER, file_icon("folder"), "Trip")]
    names = ["compress", "trim", "folder", "settings"] if which == "all" else [which]
    for n in names:
        build(n, icons)


if __name__ == "__main__":
    main()
