# Architecture

```
 ┌──────────────── tray app (apps/kompacter) ─────────────────────┐
 │  UI: Slint (ui/*.slint), CPU-rendered, no web view              │
 │        ▲ properties (progress, estimates)  │ callbacks          │
 │        │                                   ▼                    │
 │  Rust: src/main.rs (flow) · sys.rs (clipboard, tray corner,     │
 │        Recycle Bin, start with Windows) · output.rs · settings  │
 └────────────────────────────┬────────────────────────────────────┘
                              │ calls (worker threads)
                     ┌────────▼─────────┐        ┌──────────────┐
                     │  kompacter-core  │──runs──▶ ffmpeg/ffprobe│
                     └────────▲─────────┘        └──────────────┘
                              │ calls
                     ┌────────┴─────────┐
                     │  kompacter-cli   │
                     └──────────────────┘
```

## apps/kompacter

- One tray process and one flyout, with FFmpeg as a child process for media jobs.
- The Windows UI uses Skia raster. Other platforms use FemtoVG/OpenGL. The flyout
  keeps its translucent fold and reuses a shadow image.
- The blob body, its gloss and the press heads use pre-drawn images
  (`tools/art.py` makes `ui/*.png`).
- UI files: `ui/app.slint` (window, screens), `ui/settings.slint` (Settings screen and the
  `Prefs` global it reads and calls), `ui/stage.slint` (blob, press), `ui/theme.slint`
  (colors, buttons, dropdown, switch), `ui/demo.slint` (the small looping scenes that show
  the three automatic features, in the first-run steps and Settings' hover previews).
- `src/flow.rs` (file → compress → clipboard), `src/trim.rs` (trimmer, live estimates,
  playhead, GIF), `src/motion.rs` (open/close, eyes, press, marching dashes),
  `src/quick.rs` (the quick bar, paste guard, clips that shrink on their own),
  `src/watch.rs` (background look at the clipboard and clip folders), `src/sys.rs`
  (Windows bits, including the global shortcut and the paste guard's keyboard hook).
- Quick bar: a 136 px view on top of the normal flow. While it shows, `set_view` turns
  Reading / Big / Busy / Done / Error into its states, so the flow code is the same.
- Paste guard: a low-level keyboard hook (only while the setting is on) sees Ctrl+V in
  Discord, and if a copied file is over the limit it holds that paste back and opens the
  quick bar. After Compress & paste, Kompacter brings Discord back and presses Ctrl+V,
  only if Discord really is in front.
- Updates: `src/update.rs` checks GitHub's release list once a day (release builds on
  Windows only; the release workflow stamps `KOMPACTER_VERSION`), downloads the newest
  published installer with curl.exe, checks it against the `.sha256` the release publishes,
  and runs it silently (`/S`) when Kompacter quits or the user presses Restart; the
  installer reopens the app. Settings → General → "Install updates" turns it off.
- What's new: `src/news.rs` holds a few lines per version, shown once on the first start
  of a new version (`seen_version` in settings). A version with no entry shows nothing.
- Platform code lives in `src/sys.rs` (Windows now; macOS/Linux get their own part later).
- ffmpeg runs as a hidden child process at below-normal priority, so games stay smooth.
- Dev builds: `KOMPACTER_DEMO=big|details|trim|trim-gif|busy|done|error|settings|auto|drag|discord|quick|quick-busy|quick-done|quick-empty|guard`
  shows a screen with sample data. `KOMPACTER_OPEN=<file>` opens a file at start;
  `KOMPACTER_QUICK=<file>` opens it in the quick bar (add `KOMPACTER_GUARD=1` for the
  paste guard's bar). `KOMPACTER_DROP=<file>@<seconds>` acts out a drop at that time, and
  `KOMPACTER_DEMO_COPY=1` makes the Linux build report the copy as done (Linux has no file
  clipboard yet); both are for recording the README demos.
- README media: `tools/media/banner.py` draws the banner from the app's art;
  `tools/media/record.py` drives the debug build on a virtual display (Xvfb) with real
  clicks and saves each frame with its alpha; `tools/media/compose.py` puts the frames on a
  made-up desktop with a cursor and writes `docs/media/*.gif`.

## kompacter-core

| Module | Job |
|---|---|
| `size` | Presets (Discord 20/50/500, email 25), parsing `"20mb"`, display |
| `probe` | ffprobe → `MediaInfo` (kind, duration, streams, alpha). Skips album art. |
| `plan` | **Pure math**, no ffmpeg: target bitrate, resolution ladder, quality badge, trim estimates. Safe to call on every UI frame. |
| `ffmpeg` | Spawns ffmpeg with `-progress pipe:1`, turns it into 0..1 progress, supports cancel. |
| `gpu` | Finds hardware encoders (H.264, H.265, AV1 on NVENC, AMF, Quick Sync, VideoToolbox) that really work, with a tiny test encode. Cached per codec. |
| `formats` | User-chosen output formats: video codec (H.264 most compatible, H.265 smaller, AV1 smallest), audio (Auto/MP3/Opus), images (Auto lossless-first/JPEG/WebP), archives (Zip/7z). The planner judges quality per codec. |
| `jobs::video` | MP4 (H.264, H.265 or AV1 + AAC), scaled/fps-capped per plan. GPU single pass when available, else two-pass x264, or x265 / SVT-AV1; falls back to CPU if the GPU encode fails; one retry if it overshoots. |
| `jobs::audio` | MP3 (≥ 64 kbps) or Opus (below), retry on overshoot. |
| `jobs::image` | JPEG, or WebP when transparent; steps quality then size down until it fits. Caps at 4096 px. |
| Archives everywhere | When a zip doesn't fit but there's text or documents in it, a 7z (LZMA2) is tried before giving up. |
| `jobs::folder` | Shares the size budget across media files (and documents, models) by original size, shrinks them, zips (media stored, others deflated). If it can't fit in one zip, packs it into several zips that each fit. |
| `jobs::split` | Too long for one good file: cuts video/audio into equal parts that each fit (up to 10). |
| `jobs::archive` | Zip and 7z files: opens them (zip-slip safe, 32 GB unpack cap), shrinks what's inside like a folder, packs it again with the same layout and format. RAR and password-protected archives are packed as-is. |
| `jobs::model` | 3D models (glTF binary `.glb`, `.gltf` with embedded data): shrinks the textures (opaque → JPEG, transparent → smaller PNG), rebuilds the binary buffer, meshes untouched. Other model formats (OBJ, STL, FBX) are packed into an archive. |
| `jobs::docs` | PDFs, Office files (docx, pptx, xlsx, OpenDocument), EPUB and CBZ: shrinks the JPEGs inside, resizes PNGs (256 colors past the first step), keeps names and formats so it stays a normal document; falls back to an archive. |
| `jobs::cut` | Keep-quality trim: stream copy, no re-encode (snaps to keyframes). |
| `jobs::image` (phone formats) | HEIC/HEIF/AVIF are decoded to a lossless PNG first (ffmpeg ≥ 7.1 assembles HEIC tiles; the installer bundles a current build), then go the JPEG way. |
| `jobs::gif` | Animated GIFs that are too big stay GIFs (MP4 only if no GIF fits). Short clips (up to 30 s) as GIFs: one test encode predicts the width and frame rate that fit, then at most a few more encodes walk down until it does. |

Public API: `Engine::probe`, `Engine::estimate` (live numbers), `Engine::compress` (does the work).

### Key rules

- ffmpeg runs only during a job, so idle cost is close to zero.
- Results go to `<name>.kompacter.<ext>`, by default in a `Kompacted` folder next to the app, cleaned after 7 days.
- Everything is local. The engine never touches the network.

## Known gaps

- GPU paths are untested on real hardware so far (CI and the dev box have no GPU); the CPU fallback is tested.
- A single non-media file bigger than the limit (a huge installer) can't be split into parts that open on their own.
- Keep-quality trim size is a linear estimate; exact packet-sum estimate planned.
