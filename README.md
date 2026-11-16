# Kompacter

Compress any file, any time. With better quality and more for those apps. All local.

Copy a file that's too big for Discord (or email, or any app with an upload limit), and Kompacter shrinks it to fit, puts it back on your clipboard, and you paste. Videos, audio, images, whole folders. Nothing is uploaded anywhere: every byte stays on your PC.

> Status: early development. The Windows tray app uses Skia CPU rendering. Media jobs run in FFmpeg, with hardware encoding available on supported GPUs.

## Download

Grab the Windows installer from [Releases](https://github.com/KXZer0/Kompacter/releases). No admin rights needed; ffmpeg is included.

## Why

- **Web compressors** upload your files to someone else's server, and you still have to download and re-attach the result.
- **Existing desktop tools** handle one file type (usually video) or one OS.
- **Kompacter** handles everything, runs locally with your GPU, and is built for "share this clip right now".

## Features (planned for v1)

- Fits files under a target size: Discord 20 / 50 / 500 MB, email 25 MB, or custom.
- **Trim + compress** with live size and quality estimates while you drag the handles.
- Folders: shrinks the media inside, then zips.
- Short clips as **GIFs** that fit.
- Lives in the tray. Open it by dropping a file, right-clicking **Kompact** in Explorer, a hotkey (Ctrl+Alt+K opens a small quick bar for what you copied), or the optional paste guard that notices a too-big paste into Discord.
- Optional: shrink new clips in your clip folders (Medal, Game Bar, ShadowPlay, OBS) on their own.
- No admin rights needed. No account. No network access except update checks.

## Repository layout

```
crates/
  kompacter-core/   compression engine (Rust library): probe, plan, video/audio/image/folder jobs
  kompacter-cli/    `kompacter` command-line tool on top of the engine
apps/
  kompacter/        tray app: Slint UI (ui/*.slint) + Rust logic (src/), Windows first
docs/
  ARCHITECTURE.md   how the pieces fit together
  ROADMAP.md        phases, hooks/triggers plan, open decisions
design/             UI mockups
```

## Try the engine

Needs Rust and ffmpeg on your PATH (or set `KOMPACTER_FFMPEG` / `KOMPACTER_FFPROBE`).

```bash
cargo build --release
./target/release/kompacter compress clip.mp4 --target discord
./target/release/kompacter compress clip.mp4 --target 20mb --start 12 --end 40
./target/release/kompacter compress clip.mp4 --start 12 --end 40 --keep-quality
./target/release/kompacter estimate clip.mp4 --target 20mb --start 0 --end 30
./target/release/kompacter compress ./photos --target 25mb
```

`estimate` prints the resolution, bitrate and quality badge the trimmer UI will show, without encoding anything.

## Run the app from source

```bash
cargo run -p kompacter
```

## License

Copyright (C) 2026 LS. Kompacter is free software under the [GNU Affero General Public License, version 3](LICENSE) (AGPL-3.0). Kompacter calls ffmpeg as a separate program; ffmpeg has its own LGPL/GPL license.

The window is drawn with [Slint](https://slint.dev), used under its royalty-free license.

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-light.svg)](https://slint.dev)
