# Architecture

```
 ┌──────────────── tray app (apps/kompacter) ─────────────────────┐
 │  UI: Slint (frontend/*.slint)                                   │
 │        ▲ properties (progress, estimates)  │ callbacks          │
 │        │                                   ▼                    │
 │  Rust: src/app (state, jobs, settings)                          │
 │        src/frontend (Slint bindings, motion) · src/platform    │
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
  (`tools/art.py` makes `frontend/*.png`).
- UI files: `frontend/app.slint` (window, screens), `frontend/settings.slint` (Settings screen and the
  `Prefs` global it reads and calls), `frontend/stage.slint` (blob, press), `frontend/theme.slint`
  (colors, buttons, dropdown, switch), `frontend/demo.slint` (the small looping scenes that show
  the three automatic features, in the first-run steps and Settings' hover previews).
- App logos (Discord, Slack, WhatsApp, Telegram, Signal) are the official marks from Simple
  Icons, as SVG path data in `frontend/icons.slint`, drawn in each brand's colours by `AppLogo`
  (`frontend/theme.slint`): Slack in its four colours, Telegram as its circle with a white plane.
- `src/app/mod.rs` coordinates startup, application state, and event routing.
  `src/app/flow.rs` handles file ingestion and compression, `src/app/trim.rs` handles the
  trimmer and live estimates, and `src/app/quick.rs` and `src/app/watch.rs` handle the quick bar
  and background watches. `src/frontend/motion.rs` owns flyout and stage animation.
- Quick bar: a 136 px view on top of the normal flow. While it shows, `set_view` turns
  Reading / Big / Busy / Done / Error into its states, so the flow code is the same.
- Paste guard: a low-level keyboard hook (only while the setting is on) sees Ctrl+V in a
  chat app: Discord, Slack, WhatsApp, Telegram or Signal, found by the program in front
  or, in a browser, by the tab's title (`platform::chat_app`). If a copied file is over *that
  app's* limit (Discord follows the user's Discord tier), it holds the paste back and opens
  the quick bar with the app's logo; while the bar is open, `App::target_bytes` is that
  app's limit. After Compress & paste, Kompacter brings the chat back and presses Ctrl+V,
  only if it really is in front.
- Updates: `src/app/update.rs` checks GitHub's release list once a day (release builds on
  Windows only; the release workflow stamps `KOMPACTER_VERSION`), downloads the newest
  published installer with curl.exe, checks it against the `.sha256` the release publishes,
  and runs it silently (`/S`) when Kompacter quits or the user presses Restart; the
  installer reopens the app. Settings → General → "Install updates" turns it off.
- What's new: `src/app/news.rs` holds a few lines per version, shown once on the first start
  of a new version (`seen_version` in settings). A version with no entry shows nothing.
- `src/platform/` provides OS integration. `src/platform/windows/` holds the native Windows
  shell, window, audio, global shortcut, and paste guard code.
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

### Keeping it light

- Hidden in the tray, nothing redraws and no timer runs (the clipboard check for "copied
  files" is the only wake-up, twice a second, and only when that setting is on).
- Whatever is on screen stays alive: the blob breathes whenever the window is open (its
  timer moves it by the real time passed, so it's the same speed on every screen), and
  stops only when the window is hidden.
- The feature demos run on `animation-tick()`, the real clock, at the display's full
  refresh rate: same speed everywhere, smoother on faster screens. They're only on screen
  briefly (first run, a hover in Settings).
- The press while compressing steps at 30 Hz: it follows the progress, moving far less
  than a pixel a step, so it looks the same, and a long compress doesn't redraw at
  144–240 Hz the whole time (that CPU goes to the encode instead).
- Blob art is drawn at 2× (sharp up to 200% scaling); the trimmer drops its preview frame
  when you leave it, updates its detail rows in place, and lets a frame finish decoding
  while a handle is dragged (instead of restarting ffmpeg on every move).

## kompacter-core

| Module | Job |
|---|---|
| `size` | Presets (Discord 20/50/500 MB, Slack 1 GB, WhatsApp 2 GB, Telegram 2 GB, Signal 100 MB, email 25 MB), parsing `"20mb"`, display |
| `probe` | ffprobe → `MediaInfo` (kind, duration, streams, alpha). Skips album art. Video: pixel format, HDR (PQ / HLG transfer), rotation, and its own bitrate (the stream's, or the file's minus the audio for MKV/WebM). |
| `plan` | **Pure math**, no ffmpeg: target bitrate, resolution ladder, quality badge, trim estimates, mono under 64 kbps, audio bitrate next to a copied video. Safe to call on every UI frame. |
| `ffmpeg` | Spawns ffmpeg with `-progress pipe:1`, turns it into 0..1 progress, supports cancel. |
| `gpu` | Finds hardware encoders (H.264, H.265, AV1 on NVENC, AMF, Quick Sync, VideoToolbox) that really work, with a tiny test encode. Cached per codec. NVENC also gets spatial AQ + 20-frame lookahead (+ temporal AQ on H.264/H.265); the test encode leaves them out, and an encode that's refused with them retries once without, so older cards stay on the GPU. |
| `hdr` | HDR → SDR: `zscale` + `tonemap` (Hable) to BT.709, run after scaling, output tagged BT.709. Whether ffmpeg has the filters is asked once (`-filters`) and cached; without them, or when a file's tags make zscale refuse, the picture is encoded as before. |
| `formats` | User-chosen output formats: video codec (H.264 most compatible, H.265 smaller, AV1 smallest), audio (Auto/MP3/Opus), images (Auto lossless-first/JPEG/WebP), archives (Zip/7z). The planner judges quality per codec. |
| `jobs::video` | MP4 (H.264, H.265 or AV1 + AAC), scaled/fps-capped per plan. GPU single pass when available, else two-pass x264, or x265 / SVT-AV1; falls back to CPU if the GPU encode fails; one retry if it overshoots. HDR is tone-mapped. AAC goes mono under 64 kbps. When only the audio is too big (whole file, video already the chosen codec, 8-bit 4:2:0 SDR, upright, ≤ 60 fps, and it plus ≥ 48 kbps of audio fits), the video is copied as-is and only the audio re-encoded; if that still comes out too big, it's re-encoded the normal way. |
| `jobs::audio` | MP3 (≥ 64 kbps) or Opus (below), mono under 64 kbps, retry on overshoot. |
| `jobs::image` | JPEG, or WebP when transparent; steps quality then size down until it fits. Caps at 4096 px. |
| Archives everywhere | When a zip doesn't fit but there's text or documents in it, a 7z (LZMA2) is tried before giving up. |
| `jobs::folder` | Shares the size budget across media files (and documents, models) by original size, shrinks them, zips (media stored, others deflated). If it can't fit in one zip, packs it into several zips that each fit. |
| `jobs::split` | Too long for one good file: cuts video/audio into equal parts that each fit (up to 10). |
| `jobs::archive` | Zip and 7z files: opens them (zip-slip safe, 32 GB unpack cap), shrinks what's inside like a folder, packs it again with the same layout and format. RAR and password-protected archives are packed as-is. |
| `jobs::model` | 3D models (glTF binary `.glb`, `.gltf` with embedded data): shrinks the textures (opaque → JPEG, transparent → smaller PNG), rebuilds the binary buffer, meshes untouched. Other model formats (OBJ, STL, FBX) are packed into an archive. |
| `jobs::docs` | PDFs, Office files (docx, pptx, xlsx, OpenDocument), EPUB and CBZ: shrinks the JPEGs inside, resizes PNGs (256 colors past the first step), keeps names and formats so it stays a normal document; falls back to an archive. |
| `jobs::cut` | Keep-quality trim: stream copy, no re-encode (snaps to keyframes). |
| `jobs::image` (phone formats) | HEIC/HEIF/AVIF are decoded to a lossless PNG first (ffmpeg ≥ 7.1 assembles HEIC tiles; the installer bundles a current build), then go the JPEG way. |
| `jobs::gif` | Animated GIFs that are too big stay GIFs (MP4 only if no GIF fits). Short clips (up to 30 s) as GIFs: one test encode predicts the width and frame rate that fit, then at most a few more encodes walk down until it does. HDR clips are tone-mapped (after the scale). |

Public API: `Engine::probe`, `Engine::estimate` (live numbers), `Engine::compress` (does the work).

### Key rules

- ffmpeg runs only during a job, so idle cost is close to zero.
- Results go to `<name>.kompacter.<ext>`, by default in a `Kompacted` folder next to the app, cleaned after 7 days.
- Everything is local. The engine never touches the network.

## Known gaps

- GPU paths are untested on real hardware so far (CI and the dev box have no GPU); the CPU fallback is tested.
  That includes NVENC's AQ and lookahead options and the retry without them on cards that refuse them.
- HDR files that tag only the transfer curve (no primaries / matrix) aren't tone-mapped: zscale refuses them, so they're encoded as before.
- A single non-media file bigger than the limit (a huge installer) can't be split into parts that open on their own.
- Keep-quality trim size is a linear estimate; exact packet-sum estimate planned.
