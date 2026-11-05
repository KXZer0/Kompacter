# Kompacter desktop app

Tauri 2 tray app on top of `kompacter-core`. First working cut: tray icon, flyout, drag & drop,
compress, result copied to the clipboard as a file (Windows).

## Run it (Windows)

Needs Rust, Node 20+, the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)
(WebView2 is already on Windows 10/11) and ffmpeg on your PATH (`winget install Gyan.FFmpeg`).

```bash
cargo install tauri-cli --version "^2" --locked
cd apps/desktop/ui && npm install
cd ../src-tauri && cargo tauri dev
```

Click the tray icon, drop a file on the flyout (or **Choose File** / **Paste**), press
**Compress**, then Ctrl+V in Discord. Video can be trimmed first, with live size estimates.

Build an installer (per-user, no admin): `cargo tauri build`.

## Layout

- `src-tauri/src/main.rs`: app setup, shared engine
- `src-tauri/src/tray.rs`: tray icon, show/hide the flyout above the taskbar
- `src-tauri/src/commands.rs`: `presets`, `inspect`, `compress` (+ `progress` events), `cancel`, `hide`
- `src-tauri/src/clipboard.rs`: put files on the clipboard like Explorer does
- `ui/`: the flyout, Svelte 5 + TypeScript + Vite
  - `src/App.svelte`: flyout shell and view logic (idle, too big, trim, compress, done, settings)
  - `src/lib/Stage.svelte`: dashed drop zone, blob with face, hydraulic press driven by real progress
  - `src/lib/*View.svelte`, `Trimmer`, `FileCard`, `SizeMeter`, `PresetPill`: one component per part
  - `src/lib/themes.css`: Kompacter and Discord (Light / Ash / Dark / Onyx) colour tokens
  - `src/lib/api.ts`: typed wrappers for the Rust commands
  - `src/lib/bindings/`: TypeScript types **generated from the Rust engine**. Don't edit by hand;
    run `npm run types` after changing engine structs.

UI checks: `npm run check` (types) and `npm run build`.

## Not yet

Paste guard, global hotkey and start with Windows (settings are saved, marked "coming soon");
Explorer right-click menu; clip-folder watcher; bundled ffmpeg; real acrylic blur
(`windowEffects`); macOS/Linux clipboard. The full design lives in `design/mockup/index.html`.
