# Linux packaging

The release workflow builds three x86-64 downloads on Ubuntu 22.04 (so they run on glibc 2.35
and newer):

| File | Script | ffmpeg |
|---|---|---|
| `Kompacter-<version>-x86_64.AppImage` | `appimage.sh` | bundled (static BtbN linux64 GPL build) |
| `kompacter_<version>_amd64.deb` | `build.sh` | `Depends: ffmpeg` from the distribution |
| `Kompacter-<version>-linux-x86_64.tar.gz` | `build.sh` | bundled when `KOMPACTER_FFMPEG_DIR` is set |

Kompacter uses the `ffmpeg` and `ffprobe` next to its own binary when both are there, otherwise
the ones on `PATH`. In the AppImage they sit in `usr/bin/` beside `Kompacter`, and `AppRun` also
points `KOMPACTER_FFMPEG`/`KOMPACTER_FFPROBE` at them. Only Kompacter and the media tools are
bundled: Slint's OpenGL/EGL, xkbcommon, Wayland/X11 and fontconfig libraries come from the host,
as on any desktop.

## Build locally

```sh
sudo apt-get install -y libfontconfig-dev desktop-file-utils dpkg-dev appstream
cargo build --locked --release -p kompacter

# A folder with static ffmpeg, ffprobe and their LICENSE, for example from
# BtbN's ffmpeg-nX.Y-latest-linux64-gpl-X.Y.tar.xz.
export KOMPACTER_FFMPEG_DIR=/path/to/ffmpeg
export KOMPACTER_LINUX_BINARY=target/release/Kompacter

packaging/linux/build.sh dist/linux           # .deb and .tar.gz
APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
    packaging/linux/appimage.sh dist/linux    # .AppImage and .AppImage.sha256
```

`KOMPACTER_PACKAGE_VERSION` sets the version in the file names (default: `Cargo.toml`'s).
`KOMPACTER_APPDIR_ONLY=1 packaging/linux/appimage.sh` stops after assembling
`dist/linux/Kompacter.AppDir`, for checking the layout without `appimagetool`. appimagetool runs
with `APPIMAGE_EXTRACT_AND_RUN=1`, so FUSE is not needed to build.

## Check an AppImage

```sh
./Kompacter-<version>-x86_64.AppImage --appimage-extract
ls squashfs-root squashfs-root/usr/bin   # AppRun, Kompacter, ffmpeg, ffprobe
```

Users on systems without FUSE can run it with `--appimage-extract-and-run`.

## Updates

A copy started from an AppImage (`$APPIMAGE` set) downloads the newer release's
`Kompacter-<version>-x86_64.AppImage`, checks it against its `.sha256`, and on the next restart
renames it over the running file (keeping its name, so launchers keep working). The .deb and the
tarball only open the release page.
