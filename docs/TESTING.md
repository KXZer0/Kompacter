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
