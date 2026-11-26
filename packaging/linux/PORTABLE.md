# Kompacter portable Linux test bundle

Extract the tarball into a writable directory and run `./Kompacter`. Run
`./install-launcher.sh` from the extracted bundle to add Kompacter and its
Choose files and Quit actions to your desktop launcher. Run the installer again
after moving the bundle.

If the bundle has FFmpeg and ffprobe alongside Kompacter, it uses that pair.
Otherwise, install FFmpeg through the distribution package manager so both
commands are on the desktop session's PATH. The binary also needs the host's
Fontconfig, OpenGL/EGL, xkbcommon, and X11 or Wayland libraries. Choose files
uses the session's XDG desktop portal, with Zenity as a fallback.

Closing the panel leaves Kompacter running. Open the desktop launcher again to
reopen it. Select Quit in the panel, tray, or launcher to stop the resident.
Use the file chooser on Wayland sessions where dragging files is unavailable.
