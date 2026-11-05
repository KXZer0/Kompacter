# Roadmap

## Phases

| Phase | Goal | Status |
|---|---|---|
| 0. UI mockups | Clickable mockup of popup, trimmer, settings; iterate on look | In progress |
| 1. Engine | Rust library + CLI: probe, fit video/audio/image/folder, trim, estimates | **Working**, needs GPU + polish |
| 2. App shell | Tauri 2 tray app: popup, drag & drop, result on clipboard, settings | Next |
| 3. Triggers | Right-click menu, hotkey, paste guard, heads-up on copy, per-app profiles | |
| 4. Trimmer + extras | Live trimmer, GPU encoding, pre-compress on copy, clip-folder watcher, queue | |
| 5. Release | Signed per-user installer, auto-update, macOS/Linux builds | |

## Design direction

Apple-inspired: simple and snappy. Frosted panels, large rounded corners, one accent colour, crisp short springs (~200 ms). No home screen: the drop box is the app. It pops up from the tray (or near the cursor), never as a big window.

## Hooks and triggers (no Discord modding, no admin)

| Trigger | How | Notes |
|---|---|---|
| Tray drop zone | Drop files onto the popup | Always available |
| Right-click **Kompact** | Per-user registry entry (HKCU) | Win 11 shows it under "Show more options" until we ship a signed package |
| Global hotkey | Compresses whatever's on the clipboard | |
| Paste guard | Low-level keyboard hook watches only Ctrl+V while a chat app is in front; if the clipboard file is over that app's limit, hold the paste and ask | Opt-in. Fails open: on any error, the original paste goes through. |
| Heads-up on copy | Clipboard listener; "too big for Discord, compress now?" | |
| Pre-compress on copy | Start encoding the moment a big file is copied, so paste is instant | Images/audio by default, video opt-in (battery) |
| Clip-folder watcher | Watch ShadowPlay / Game Bar / OBS / Medal folders for new clips | Detect folders, ask once at first run |
| Per-app profiles | Foreground process name → limit (Discord, Slack, email clients, custom) | Nitro tier is set by the user; never read Discord tokens |

Not doing: intercepting drag-and-drop into Discord, hooking its upload dialog, or reading its UI. All need injection or break on every Discord update.

## Open decisions

1. Big paste: **ask first** (proposed) vs. auto-compress, or auto for images/audio and ask for video.
2. Default guarded apps: Discord, WhatsApp Desktop, Slack?
3. Output cache cleanup: 7 days (proposed).
4. WhatsApp preset: verify current limits before adding.

## Engine to-do

- [ ] GPU encoders (NVENC / QSV / AMF) with speed test and CPU fallback
- [ ] Exact keep-quality estimate from packet sizes
- [ ] EXIF orientation for photos
- [ ] Split folders/long videos into parts when they can't fit
- [ ] Bundle ffmpeg as a Tauri sidecar
