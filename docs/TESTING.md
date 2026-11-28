# Testing on a real Windows PC

CI only builds and lints the app on Windows; these need a real machine. Try on Windows 11
and, if you can, Windows 10. Tick each one off; anything that fails, note what happened.

## Install and first start

- [ ] The installer runs without admin rights and opens Kompacter at the end.
- [ ] First start shows the available setup steps. Use the keyboard to change the compression preference, select Done, restart, and check the saved value in Settings.
- [ ] The tray icon appears; the one-time "keep the tray icon visible" tip shows on the idle
      screen, and its switch moves the icon out of the ^ menu (now, or after Explorer
      restarts: note which).
- [ ] Kompacter starts with Windows (sign out and in). Turning it off in Settings sticks.

## Opening files

- [ ] Drop a big video on the window: it shows "too big", Compress makes a file that fits,
      and it's on the clipboard (paste it into a chat or a folder).
- [ ] Right-click → **Kompact** on one file, several files, and a folder (Windows 11:
      under "Show more options").
- [ ] Ctrl+Alt+K with a big file copied opens the quick bar.
- [ ] Copying a big file in Explorer offers the quick bar (when that setting is on).

- [ ] Open a video in Trim and move both handles. Hover a second file from Explorer over
      the panel, press Escape, and verify the same video and both handle positions remain.
      Then drop the second file and verify it replaces the first selection.

## Paste guard (Settings → Automatic → "When I paste into a chat")

For each app you have: copy a file bigger than that app's limit, click in a chat, press
Ctrl+V. The paste should be held and the quick bar should show the app's logo and limit.

- [ ] Discord app, and Discord in a browser tab (limit follows your Discord tier: 20 MB,
      or 50 / 500 MB if you picked Nitro Basic / Nitro)
- [ ] Slack (1 GB)
- [ ] WhatsApp (2 GB)
- [ ] Telegram (2 GB)
- [ ] Signal (100 MB)
- [ ] **Compress & paste** pastes the small file into that same chat; **Paste anyway**
      pastes the original; a file under the limit pastes normally with no bar.

## Formats

- [ ] An iPhone HEIC photo becomes a JPEG that fits (needs the bundled ffmpeg).
- [ ] An HDR video (iPhone or a game capture with HDR on) keeps normal colours, not grey
      and washed out.
- [ ] A zip of videos and photos comes back as a zip, same folders, smaller files.
- [ ] An animated GIF stays a GIF.

## GPU encoding (Settings → General → GPU encoding)

- [ ] NVIDIA, AMD or Intel graphics: compressing a video is clearly faster with the switch
      on, and the result plays fine. If it fails, it should quietly fall back to the CPU.

## Updates (needs two published releases)

1. Publish a release with auto-updates in it and install it.
2. Publish the next release (tagged higher). Within a day (or on the next start after 90 s),
   the idle screen shows the update pill with a filling ring, then "Update … ready ·
   Restart".
- [ ] Restart installs it silently and reopens Kompacter, which shows "What's new" once.
- [ ] `%TEMP%\Kompacter-update` is gone after the new version starts.
- [ ] Turning off Settings → General → "Install updates" stops it.

## Uninstall

- [ ] Settings → Apps → Kompacter → Uninstall removes the app, the Start menu entry, the
      right-click menu, start-with-Windows, `%APPDATA%\app.kompacter.desktop` and the
      `Kompacted` folder.

## Start with the system on Linux and macOS

Use a release build (`cargo build --release`, a package or the AppImage): debug builds never
switch it on by themselves.

Linux (Settings → General → "Start with the system"):

- [ ] The first start of a release build creates
      `~/.config/autostart/io.github.KXZer0.Kompacter.desktop` (or under `$XDG_CONFIG_HOME`).
      Its `Exec=` is `kompacter --tray` for the .deb, the `.AppImage` file's path for the
      AppImage, and the `Kompacter` program's path for the tarball, each followed by `--tray`.
- [ ] Log out and in: Kompacter is running in the tray with no window open.
- [ ] Turning the switch off removes the file; it stays off after restarting Kompacter.
- [ ] Move the AppImage or tarball folder and start it again: `Exec=` follows the new path.
- [ ] Switching it off in the desktop's own startup settings (GNOME Tweaks, KDE
      Autostart) shows the switch off in Kompacter, and Kompacter doesn't switch it back on.

macOS (Settings → General → "Open at login"), with the `.app` from the DMG:

- [ ] The first start adds Kompacter to System Settings → General → Login Items (macOS 13
      and later; macOS 12 and earlier get `~/Library/LaunchAgents/io.github.KXZer0.Kompacter.plist`).
- [ ] Log out and in: Kompacter is running.
- [ ] Turning the switch off removes it from Login Items. Switching it off in Login Items
      shows the switch off in Kompacter; switching it on again opens Login Items.
- [ ] `cargo run` (not a bundle) shows no "Open at login" switch and registers nothing.

## macOS: shortcut and paste guard

- [ ] Settings → Automatic → Shortcut lists ⌃⌥K, ⌃⇧K and ⌃⇧Space. With a big file copied in
      Finder, ⌃⌥K opens the quick bar from any app (no permission prompt); pressing it
      again folds the bar away. Each of the other choices works, and "Off" stops it.
- [ ] Pick a shortcut another app already registered (for example one set in another
      hotkey utility): Settings says "Used by another app, pick another".
- [ ] Turn on "When I paste into a chat" without Accessibility allowed: macOS shows its
      Accessibility prompt, Settings shows "Allow Kompacter in System Settings → Privacy &
      Security → Accessibility", and Cmd+V everywhere still pastes normally.
- [ ] Allow Kompacter there (no restart): within a few seconds the note goes away and
      the guard works.
- [ ] For each app you have (Discord, PTB or Canary, Slack, WhatsApp, Telegram, Signal):
      copy a file over its limit in Finder, click in a chat, press Cmd+V. The paste is held
      and the quick bar shows the app's logo and limit. **Compress & paste** switches back
      to that app and pastes the small file; **Paste anyway** pastes the original.
- [ ] Discord, Slack, WhatsApp or Telegram in Safari, Chrome or Firefox: the same, found
      by the window title.
- [ ] A file under the limit, Cmd+V in a non-chat app (TextEdit, Finder), and holding
      Cmd+V all paste normally with no bar.
- [ ] Turn the guard off: Cmd+V is never held back again (no restart).

## Linux (Wayland): shortcut

- [ ] GNOME 48+ or KDE Plasma 6: on first start the desktop asks to confirm Kompacter's
      shortcut (Ctrl+Alt+K by default). With a big file copied, the shortcut opens the
      quick bar from any app; picking another shortcut in Settings asks again and works.
- [ ] Restart Kompacter: the shortcut still works (note whether the desktop asks again).
- [ ] Older GNOME / no global shortcuts portal (check with `busctl --user introspect
      org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop | grep
      GlobalShortcuts`): Settings → Shortcut shows "Set a shortcut in your system settings
      that runs: kompacter". A system shortcut running `kompacter` opens the panel.
- [ ] Settings has no paste guard row on Linux.

## Noticing copies on macOS and Linux (Settings → Automatic → "When I copy a big file")

Turn the setting on and copy a file bigger than your limit, then a small one, then some
text. Only the big file should bring up the bar, once per copy.

- [ ] macOS: copy in Finder (Cmd+C); the bar appears within about a second, and nothing
      happens while the setting is off.
- [ ] KDE Plasma (Wayland): copy in Dolphin.
- [ ] Sway or another wlroots desktop: copy in a file manager (Nautilus, Thunar, PCManFM),
      and with `wl-copy --type text/uri-list file:///path/to/big.mp4`.
- [ ] A path with spaces or accents (`My Clips/été.mp4`) is noticed.
- [ ] GNOME (Wayland) and any X11 session: the setting is greyed out with a short reason,
      and first start doesn't offer it.

## Automated checks (CI)

What CI catches without a person, on every pull request that touches the app:

- **Memory and idle CPU** (`memory-test.yml`, Linux, Windows with CPU and with GPU
  drawing, macOS): the debug app compresses a video, an audio file, a picture, a folder
  and a zip, twice each, hiding and reopening the panel in between, then stays hidden for
  20 s. It fails if memory grows between rounds (more than 15 MB), stays more than 40 MB
  over its starting point once hidden, or the hidden app uses more than 2% of a core.
  Locally: `cargo build -p kompacter && tools/selftest/run.sh` (under `xvfb-run` on a
  Linux machine without a screen; `tools/selftest/run.sh '{"gpuDrawing": true}'` for GPU
  drawing on Windows).
- **Updates** (`update-test.yml`, and the release workflow before publishing): see
  docs/RELEASING.md.
- **Commit authors** (`commit-hygiene.yml`): every commit must be authored by the PR's
  author, with no `Co-authored-by:` lines, so no one else shows up as a contributor.
