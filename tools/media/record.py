"""Records the README demos from the real app.

The debug build runs on a virtual X display (Xvfb + openbox). A scenario moves the real
pointer and clicks with xdotool, and every frame the app window is read with its alpha.
`compose.py` then puts the frames on a made-up desktop, draws the cursor, and writes a GIF.

Setup (Linux):  apt-get install xvfb openbox xdotool libxkbcommon-x11-0 fonts-inter
                pip install python-xlib numpy pillow
Run from the repo root:
    cargo build -p kompacter
    python3 tools/media/record.py compress   (or trim, folder, settings; `all` for every one)
Samples are made with ffmpeg in $KOMPACTER_MEDIA_TMP (default /tmp/kompacter-media).
"""
import json
import math
import os
import shutil
import subprocess
import sys
import time

import numpy as np
from PIL import Image
from Xlib import X, display

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
TMP = os.environ.get("KOMPACTER_MEDIA_TMP", "/tmp/kompacter-media")
EXE = os.path.join(ROOT, "target", "debug", "Kompacter")
OUTPUT_DIR = os.path.join(ROOT, "target", "debug", "Kompacted")
DISP = ":99"

# The made-up desktop (compose.py draws it); the window sits bottom-right like the tray
# flyout, its bottom edge just above the taskbar.
DESK_W, DESK_H, TASKBAR = 960, 720, 48
FPS = 25


def sh(*args, **kw):
    return subprocess.run(args, check=True, **kw)


# ---------------------------------------------------------------- samples

def samples():
    """The clip (Kompacter's intro video, re-encoded at a high bitrate so it's over the
    20 MB limit like a raw capture would be) and a folder of clips + photos. Made once.
    Point KOMPACTER_DEMO_CLIP at the intro video (or any good-looking 1080p clip)."""
    s = os.path.join(TMP, "samples")
    clip = os.path.join(s, "Kompacter intro.mp4")
    trip = os.path.join(s, "Trip")
    os.makedirs(os.path.join(trip, "clips"), exist_ok=True)
    os.makedirs(os.path.join(trip, "photos"), exist_ok=True)
    ff = ["ffmpeg", "-loglevel", "error", "-y"]
    jobs = []
    if not os.path.exists(clip):
        src = os.environ.get("KOMPACTER_DEMO_CLIP")
        if not src:
            raise SystemExit("set KOMPACTER_DEMO_CLIP to a 1080p clip (the intro video)")
        jobs.append(ff + ["-i", src, "-c:v", "libx264", "-preset", "veryfast", "-b:v", "9M",
                          "-maxrate", "9M", "-minrate", "9M", "-bufsize", "9M",
                          "-x264-params", "nal-hrd=cbr:filler=1", "-c:a", "aac", "-b:a", "192k", clip])
    if not os.path.exists(os.path.join(trip, "notes.txt")):
        lav = ff + ["-f", "lavfi"]
        jobs += [
            lav + ["-i", "mandelbrot=s=1280x720:r=30", "-t", "20", "-c:v", "libx264", "-preset",
                   "ultrafast", "-b:v", "12M", "-pix_fmt", "yuv420p",
                   os.path.join(trip, "clips", "sunset drive.mp4")],
            lav + ["-i", "life=s=1280x720:r=30:mold=10:ratio=0.3:life_color=#4a90e2:"
                   "death_color=#101820:mold_color=#e2a04a", "-t", "15", "-c:v", "libx264",
                   "-preset", "ultrafast", "-b:v", "10M", "-pix_fmt", "yuv420p",
                   os.path.join(trip, "clips", "beach.mp4")],
        ] + [
            lav + ["-i", f"gradients=s=4000x3000:seed={i},noise=alls=25", "-frames:v", "1",
                   os.path.join(trip, "photos", f"IMG_20{i}.png")] for i in (1, 2, 3)
        ]
    for p in [subprocess.Popen(j) for j in jobs]:
        p.wait()
    with open(os.path.join(trip, "notes.txt"), "w") as f:
        f.write("packing list\n")
    return clip, trip


# ---------------------------------------------------------------- display + app

def start_display():
    if subprocess.run(["xdpyinfo", "-display", DISP], capture_output=True).returncode != 0:
        subprocess.Popen(["Xvfb", DISP, "-screen", "0", "1600x1000x24"],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1)
        subprocess.Popen(["openbox"], env={**os.environ, "DISPLAY": DISP},
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1)


def app_env(extra):
    """A throwaway home: light theme, the first-run screen answered, no auto-hide
    (the virtual display has no real focus), and Inter standing in for Segoe UI."""
    home = os.path.join(TMP, "home")
    cfg = os.path.join(home, ".config", "app.kompacter.desktop")
    os.makedirs(cfg, exist_ok=True)
    with open(os.path.join(cfg, "settings.json"), "w") as f:
        json.dump({"auto_hide": False, "welcomed": True, "theme": "light",
                   "tray_tip_seen": True, "start_default_done": True}, f)
    fc = os.path.join(home, ".config", "fontconfig")
    os.makedirs(fc, exist_ok=True)
    alias = "".join(
        f'<alias binding="same"><family>{n}</family><prefer><family>Inter</family></prefer></alias>'
        for n in ("Segoe UI Variable Text", "Segoe UI Variable Display", "Segoe UI"))
    with open(os.path.join(fc, "fonts.conf"), "w") as f:
        f.write(f'<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "fonts.dtd"><fontconfig>{alias}</fontconfig>')
    return {**os.environ, "DISPLAY": DISP, "HOME": home, "XDG_CONFIG_HOME": os.path.join(home, ".config"),
            "SLINT_BACKEND": "winit-software",
            # Linux has no file clipboard yet; show the Windows result ("Copied").
            "KOMPACTER_DEMO_COPY": "1", **extra}


class Recorder:
    def __init__(self, name, env):
        self.name = name
        self.dir = os.path.join(TMP, "frames", name)
        shutil.rmtree(self.dir, ignore_errors=True)
        os.makedirs(self.dir)
        shutil.rmtree(OUTPUT_DIR, ignore_errors=True)
        subprocess.run(["pkill", "-x", "Kompacter"])
        time.sleep(0.3)
        self.proc = subprocess.Popen([EXE], env=app_env(env), stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL)
        self.t0 = time.time()
        self.d = display.Display(DISP)
        self.win = None
        while self.win is None and time.time() - self.t0 < 15:
            self.win = self._find(self.d.screen().root)
            time.sleep(0.05)
        if self.win is None:
            raise SystemExit("the app window never showed up")
        self.cursor = (DESK_W * 0.45, DESK_H * 0.45)
        self.down = False
        self.ghost = None        # what's being dragged ("clip" / "folder"), drawn by compose
        self.clicks = []         # desktop-space click times + places
        self.frames = []
        self.lapse = False
        self.place = (0, 0, 384, 496)
        self.frame()

    def _find(self, w):
        for c in w.query_tree().children:
            try:
                if c.get_wm_name() == "Kompacter" and c.get_attributes().map_state == X.IsViewable:
                    return c
            except Exception:
                pass
            r = self._find(c)
            if r is not None:
                return r
        return None

    def t(self):
        return time.time() - self.t0

    # Window placement on the desktop: right edge at the screen edge, bottom on the taskbar.
    def win_desk(self, w, h):
        return DESK_W - w, DESK_H - TASKBAR - h

    def frame(self):
        g = self.win.get_geometry()
        img = self.win.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xffffffff)
        a = np.frombuffer(img.data, np.uint8).reshape(g.height, g.width, 4)[..., [2, 1, 0, 3]]
        i = len(self.frames)
        Image.fromarray(a, "RGBA").save(os.path.join(self.dir, f"{i:05d}.png"), compress_level=1)
        wx, wy = self.win_desk(g.width, g.height)
        self.place = (wx, wy, g.width, g.height)
        self.frames.append({"t": self.t(), "win": [wx, wy], "cursor": list(self.cursor),
                            "down": self.down, "ghost": self.ghost, "lapse": self.lapse})

    # Pointer: desktop space. Over the window the real pointer goes there too (hover
    # effects are real); elsewhere it's parked off the window.
    def _real_pointer(self):
        wx, wy, w, h = self.place
        x, y = self.cursor
        rx, ry = x - wx, y - wy
        geo = self.win.translate_coords(self.d.screen().root, 0, 0)
        ox, oy = -geo.x, -geo.y
        if 0 <= rx < w and 0 <= ry < h:
            sh("xdotool", "mousemove", str(int(ox + rx)), str(int(oy + ry)), env={**os.environ, "DISPLAY": DISP})
        else:
            sh("xdotool", "mousemove", "1590", "990", env={**os.environ, "DISPLAY": DISP})

    def app(self, x, y, from_bottom=False):
        """A point in window coordinates → desktop (from_bottom: y counts up from the bottom)."""
        wx, wy, w, h = self.place
        return (wx + x, wy + (h - y if from_bottom else y))

    def wait(self, secs):
        end = self.t() + secs
        while self.t() < end:
            self.tick()

    def until(self, t_abs):
        while self.t() < t_abs:
            self.tick()

    def tick(self):
        start = self.t()
        self.frame()
        rest = 1 / FPS - (self.t() - start)
        if rest > 0:
            time.sleep(rest)

    def move(self, to, secs=0.6):
        x0, y0 = self.cursor
        x1, y1 = to
        start = self.t()
        while True:
            u = min((self.t() - start) / secs, 1)
            e = 0.5 - 0.5 * math.cos(math.pi * u)
            # A slight arc, like a hand.
            bow = math.sin(math.pi * u) * 0.08 * math.hypot(x1 - x0, y1 - y0)
            nx, ny = -(y1 - y0), (x1 - x0)
            n = math.hypot(nx, ny) or 1
            self.cursor = (x0 + (x1 - x0) * e + nx / n * bow, y0 + (y1 - y0) * e + ny / n * bow)
            self._real_pointer()
            self.tick()
            if u >= 1:
                break

    def click(self, to=None, secs=0.6):
        if to is not None:
            self.move(to, secs)
            self.wait(0.12)
        env = {**os.environ, "DISPLAY": DISP}
        self.down = True
        sh("xdotool", "mousedown", "1", env=env)
        self.wait(0.09)
        sh("xdotool", "mouseup", "1", env=env)
        self.down = False
        self.wait(0.1)

    def drag(self, to, secs=0.6, steps_wait=0.0):
        """Press, move, release on the window (real drag, e.g. a trim handle)."""
        env = {**os.environ, "DISPLAY": DISP}
        self.down = True
        sh("xdotool", "mousedown", "1", env=env)
        self.wait(0.08)
        self.move(to, secs)
        self.wait(steps_wait)
        sh("xdotool", "mouseup", "1", env=env)
        self.down = False
        self.wait(0.1)

    def wheel(self, n, gap=0.06):
        env = {**os.environ, "DISPLAY": DISP}
        for _ in range(abs(n)):
            sh("xdotool", "click", "5" if n > 0 else "4", env=env)
            self.wait(gap)

    def until_output(self, timeout=600, fps=4):
        """Time-lapse until the result file appears (compose speeds this part up)."""
        self.lapse = True
        start = self.t()
        while self.t() - start < timeout:
            self.frame()
            # Done: a result exists and ffmpeg has finished (it writes the file as it goes).
            made = os.path.isdir(OUTPUT_DIR) and any(".kompacter." in n for n in os.listdir(OUTPUT_DIR))
            if made and subprocess.run(["pgrep", "-x", "ffmpeg"], capture_output=True).returncode != 0:
                break
            time.sleep(1 / fps)
        self.lapse = False

    def finish(self):
        with open(os.path.join(self.dir, "log.json"), "w") as f:
            json.dump({"frames": self.frames, "desk": [DESK_W, DESK_H, TASKBAR]}, f)
        self.proc.terminate()
        print(self.name, len(self.frames), "frames")


# ---------------------------------------------------------------- scenarios
# Desktop icons (compose.py draws them at these places).
ICON_CLIP = (70, 70)
ICON_FOLDER = (70, 190)
IDLE_DROP = (192, 210)   # the drop zone, window coordinates


def drag_in(r, icon, kind, drop_at):
    """Pick up a desktop icon and drop it on the window at `drop_at` (seconds from start)."""
    r.move(icon, 0.8)
    r.wait(0.15)
    r.down = True
    r.wait(0.12)
    r.ghost = kind
    target = r.app(*IDLE_DROP)
    r.move(target, max(0.6, drop_at - 0.1 - r.t()))
    r.until(drop_at)
    r.ghost = None
    r.down = False


def compress(clip, trip):
    drop = 3.6
    r = Recorder("compress", {"KOMPACTER_DROP": f"{clip}@{drop}"})
    r.wait(0.5)
    drag_in(r, ICON_CLIP, "clip", drop)
    r.wait(1.3)
    r.move(r.app(200, 330), 0.5)
    r.wait(0.4)
    r.click(r.app(122, 372), 0.5)
    r.until_output()
    r.move((r.cursor[0] - 120, r.cursor[1] - 40), 0.8)
    r.wait(3.0)
    r.finish()


def trim(clip, trip):
    drop = 1.0
    r = Recorder("trim", {"KOMPACTER_DROP": f"{clip}@{drop}"})
    r.cursor = r.app(250, 330)
    r.until(drop + 1.2)
    r.click(r.app(288, 372), 0.6)       # Trim
    r.wait(1.4)
    r.move(r.app(352, 264), 0.7)        # end handle
    r.wait(0.2)
    r.drag(r.app(236, 264), 1.1, 0.3)
    r.wait(0.5)
    r.move(r.app(33, 264), 0.8)         # start handle
    r.wait(0.2)
    r.drag(r.app(118, 264), 1.0, 0.3)
    r.wait(1.0)
    r.click(r.app(192, 371), 0.6)       # Fit to 20 MB
    r.wait(1.2)
    r.click(r.app(192, 516), 0.6)       # Compress
    r.until_output()
    r.move((r.cursor[0] - 140, r.cursor[1] - 60), 0.8)
    r.wait(3.0)
    r.finish()


def folder(clip, trip):
    drop = 3.6
    r = Recorder("folder", {"KOMPACTER_DROP": f"{trip}@{drop}"})
    r.wait(0.5)
    drag_in(r, ICON_FOLDER, "folder", drop)
    r.wait(1.0)
    r.click(r.app(116, 259), 0.6)       # Show more
    r.wait(0.6)
    for y in (204, 228, 300, 324, 348):  # along the files
        r.move(r.app(250, y), 0.35)
        r.wait(0.25)
    r.wait(0.6)
    r.click(r.app(196, 397), 0.6)       # Compress & zip
    r.until_output()
    r.move((r.cursor[0] - 140, r.cursor[1] - 60), 0.8)
    r.wait(3.0)
    r.finish()


def settings(clip, trip):
    r = Recorder("settings", {})
    r.cursor = (DESK_W * 0.42, DESK_H * 0.5)
    r.wait(0.6)
    r.click(r.app(342, 41), 0.9)        # gear
    r.wait(1.0)
    for y in (153, 197):                # along the targets
        r.move(r.app(180, y), 0.35)
        r.wait(0.3)
    r.click(r.app(180, 241), 0.35)      # Email 25 MB
    r.wait(0.9)
    r.move(r.app(190, 420), 0.5)
    r.wheel(5)
    r.wait(0.8)
    for y in (397, 453, 509):           # the automatic features (hover previews)
        r.move(r.app(140, y), 0.45)
        r.wait(1.8)
    r.move(r.app(200, 560), 0.5)
    r.wheel(6)
    r.wait(1.2)
    r.move(r.app(36, 41), 0.8)          # back
    r.wait(0.2)
    r.click()
    r.wait(1.8)
    r.finish()


SCENARIOS = {"compress": compress, "trim": trim, "folder": folder, "settings": settings}

if __name__ == "__main__":
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    start_display()
    clip, trip = samples()
    for name in (SCENARIOS if which == "all" else [which]):
        SCENARIOS[name](clip, trip)
