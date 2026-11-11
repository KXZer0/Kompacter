# Roadmap

## Phases

| Phase | Goal | Status |
|---|---|---|
| 0. UI mockups | Clickable mockup of popup, trimmer, settings; iterate on look | In progress |
| 1. Engine | Rust library + CLI: probe, fit video/audio/image/folder, trim, estimates, GPU | **Working** |
| 2. App shell | Native tray app (Slint, no web view): popup, drag & drop, result on clipboard, settings | **Rewritten natively**, testing |
| 3. Triggers | Right-click menu, hotkey, paste guard, heads-up on copy, per-app profiles | Hotkey, paste guard (Discord), heads-up on copy **built**, testing |
| 4. Trimmer + extras | Live trimmer, GPU encoding, GIFs, pre-compress on copy, clip-folder watcher, queue | Trimmer, GPU, GIFs, clip-folder watcher **built** |
| 5. Release | Signed per-user installer, auto-update, macOS/Linux builds | |

## Living in the tray

Kompacter stays a tray app: no main window, no taskbar button. Windows hides new tray icons
in the "^" overflow and gives apps no official way to pin themselves, so:

- **Starts with Windows by default.** Switched on once at first run; after that the switch
  in Settings (or Task Manager) is the user's and is never turned back on.
- **Keep tray icon visible**: a one-time tip on the idle screen and a switch in Settings.
  On Windows 11 it flips the same per-user value as Settings → Taskbar → Other system tray
  icons (`HKCU\Control Panel\NotifyIconSettings\…\IsPromoted`). That value is not a
  documented API, so where it's missing the app opens that Settings page instead.
  **Needs a check on a real PC** (does the icon move at once, or only after Explorer
  re-reads it?).
- Already there: a second launch from the Start menu opens the flyout, and the global
  shortcut opens the quick bar, so the tray icon is never the only way in.

## Next up (suggested order)

1. Try the tray switch and default start-up on real Windows 10 and 11.
2. Explorer right-click **Kompact** (per-user registry entry; needs the app to accept a
   file path on launch and hand it to the running instance).
3. Engine quality that changes nothing for the person receiving the file (same codecs and
   containers): NVENC adaptive quantisation + lookahead once GPU paths are confirmed on
   hardware; tone-map HDR sources to SDR so colours don't wash out; fold audio to mono
   under ~64 kbps; skip the video re-encode when only the audio pushes a file over.
4. Pin the bundled ffmpeg to a fixed release, so two installers built a week apart behave
   the same.

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

## Decisions

- **Big paste: ask first.** The paste is held and the tray flyout opens with Compress & paste / Trim / Paste anyway. Never compress silently.
- **Paste guard UI:** the quick bar (a small card in the same corner) with Compress & paste / Trim / Paste anyway. Trim opens the full trimmer.
- **Global shortcut:** Ctrl+Alt+K by default (changeable or off in Settings). Opens the quick bar for what's on the clipboard.
- **Clip folders:** Medal, Game Bar captures and AMD ReLive folders are found on their own; ShadowPlay and OBS save into Videos, so those are added by hand. Only clips saved after it's turned on count; results never pop up over a full-screen game.
- **Flyout size:** one fixed base size for all quick states; only Settings, the folder queue and the trimmer extend.
- **Theme:** Kompacter blue by default, plus a Discord-style palette. Light/dark follows Windows.
  Auto-matching Discord's own theme is best effort: Discord keeps its theme in synced account settings, which we won't read (no tokens). Options to explore: Discord's "sync with computer" default (= follow Windows), or sampling the Discord window's background colour.

## Open decisions

1. Default guarded apps: Discord, WhatsApp Desktop, Slack?
2. Output cache cleanup: 7 days (proposed).
3. Verify Telegram / Slack / Gmail / WhatsApp limits before adding presets.

## Engine to-do

- [x] GPU encoders (NVENC / AMF / QSV / VideoToolbox) with detection and CPU fallback
- [ ] Verify GPU encodes on real NVIDIA / AMD / Intel hardware
- [ ] Exact keep-quality estimate from packet sizes
- [x] EXIF orientation for photos
- [x] Split folders/long videos into parts when they can't fit
- [x] Bundle ffmpeg with the installer
