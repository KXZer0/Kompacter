# Architecture

```
 ┌──────────────── desktop app (Tauri 2, planned) ────────────────┐
 │  UI (web: popup, trimmer, settings)                             │
 │        ▲ events (progress, estimates)    │ commands             │
 │        │                                 ▼                      │
 │  Rust shell: tray · triggers (clipboard, hotkey, paste guard,   │
 │              clip-folder watcher, per-app profiles) · settings  │
 └────────────────────────────┬────────────────────────────────────┘
                              │ calls
                     ┌────────▼─────────┐        ┌──────────────┐
                     │  kompacter-core  │──runs──▶ ffmpeg/ffprobe│
                     └────────▲─────────┘        └──────────────┘
                              │ calls
                     ┌────────┴─────────┐
                     │  kompacter-cli   │
                     └──────────────────┘
```

## kompacter-core

| Module | Job |
|---|---|
| `size` | Presets (Discord 20/50/500, email 25), parsing `"20mb"`, display |
| `probe` | ffprobe → `MediaInfo` (kind, duration, streams, alpha). Skips album art. |
| `plan` | **Pure math**, no ffmpeg: target bitrate, resolution ladder, quality badge, trim estimates. Safe to call on every UI frame. |
| `ffmpeg` | Spawns ffmpeg with `-progress pipe:1`, turns it into 0..1 progress, supports cancel. |
| `jobs::video` | Two-pass H.264 + AAC MP4, scaled/fps-capped per plan, one automatic retry if it overshoots. |
| `jobs::audio` | MP3 (≥ 64 kbps) or Opus (below), retry on overshoot. |
| `jobs::image` | JPEG, or WebP when transparent; steps quality then size down until it fits. Caps at 4096 px. |
| `jobs::folder` | Shares the size budget across media files by original size, shrinks them, zips (media stored, others deflated). |
| `jobs::cut` | Keep-quality trim: stream copy, no re-encode (snaps to keyframes). |

Public API: `Engine::probe`, `Engine::estimate` (live numbers), `Engine::compress` (does the work).

### Key rules

- ffmpeg runs only during a job, so idle cost is close to zero.
- Results go to `<name>.kompact.<ext>`. The app will write to its own cache folder and clean it after 7 days.
- Everything is local. The engine never touches the network.

## Known gaps

- Phone photos rotated via EXIF may come out sideways (ffmpeg doesn't apply EXIF orientation for stills).
- GPU encoders (NVENC / QSV / AMF) not wired yet; x264 on CPU for now.
- Folders over the limit after shrinking error out instead of splitting into parts.
- Keep-quality trim size is a linear estimate; exact packet-sum estimate planned.
