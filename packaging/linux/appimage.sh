#!/bin/sh
# Build Kompacter-<version>-x86_64.AppImage with ffmpeg and ffprobe inside.
#
#   KOMPACTER_FFMPEG_DIR=/path/with/ffmpeg-ffprobe-LICENSE packaging/linux/appimage.sh [out]
#
# KOMPACTER_FFMPEG_DIR   (required) a folder with ffmpeg and ffprobe builds (static, or
#                        shared with their libraries in ffmpeg-libs/ next to them) and
#                        their LICENSE (or LICENSE.txt).
# KOMPACTER_LINUX_BINARY a built Kompacter binary (default: cargo build --release).
# KOMPACTER_PACKAGE_VERSION / KOMPACTER_VERSION  the version in the file name (default:
#                        the workspace version in Cargo.toml).
# APPIMAGETOOL           the appimagetool to run (default: appimagetool on PATH).
# KOMPACTER_APPDIR_ONLY=1 stop after assembling the AppDir in <out>/Kompacter.AppDir.
#
# Only Kompacter and the media tools are bundled. Slint's OpenGL/EGL, xkbcommon,
# Wayland/X11 and fontconfig libraries must come from the host, so build on the oldest
# distribution to support (the release uses Ubuntu 22.04) to keep the glibc floor low.
set -eu
umask 022

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH='' cd -- "$here/../.." && pwd)
cd "$repo"

out=${1:-"$repo/dist/linux"}
mkdir -p "$out"
out=$(CDPATH='' cd -- "$out" && pwd)
version=${KOMPACTER_PACKAGE_VERSION:-${KOMPACTER_VERSION:-}}
if [ -z "$version" ]; then
    version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
fi
app_id=io.github.KXZer0.Kompacter

ffmpeg_dir=${KOMPACTER_FFMPEG_DIR:?set KOMPACTER_FFMPEG_DIR to a folder with ffmpeg, ffprobe and LICENSE}
license=
for name in LICENSE LICENSE.txt LICENSE.md; do
    if [ -f "$ffmpeg_dir/$name" ]; then
        license="$ffmpeg_dir/$name"
        break
    fi
done
[ -n "$license" ] || { echo "no FFmpeg LICENSE in $ffmpeg_dir" >&2; exit 1; }

if [ -z "${KOMPACTER_LINUX_BINARY:-}" ]; then
    cargo build --locked --release -p kompacter --bin Kompacter
fi
binary=${KOMPACTER_LINUX_BINARY:-"${CARGO_TARGET_DIR:-target}/release/Kompacter"}
binary=$(realpath "$binary")
readelf -h "$binary" | grep -q 'Machine:.*Advanced Micro Devices X86-64'
for tool in ffmpeg ffprobe; do
    readelf -h "$ffmpeg_dir/$tool" | grep -q 'Machine:.*Advanced Micro Devices X86-64'
done
desktop-file-validate "$here/$app_id.desktop"
if command -v appstreamcli >/dev/null 2>&1; then
    appstreamcli validate --no-net --explain "$here/$app_id.metainfo.xml"
fi

appdir="$out/Kompacter.AppDir"
rm -rf "$appdir"
icon="$appdir/usr/share/icons/hicolor/256x256/apps/$app_id.png"

install -Dm755 "$here/AppRun" "$appdir/AppRun"
# platform::ffmpeg() picks up ffmpeg and ffprobe next to the Kompacter binary.
install -Dm755 "$binary" "$appdir/usr/bin/Kompacter"
install -m755 "$ffmpeg_dir/ffmpeg" "$ffmpeg_dir/ffprobe" "$appdir/usr/bin/"
# A shared build's libraries (see release.yml): next to the programs, which look there.
if [ -d "$ffmpeg_dir/ffmpeg-libs" ]; then
    cp -a "$ffmpeg_dir/ffmpeg-libs" "$appdir/usr/bin/"
fi
install -Dm644 "$here/$app_id.desktop" "$appdir/usr/share/applications/$app_id.desktop"
install -Dm644 "$here/$app_id.metainfo.xml" "$appdir/usr/share/metainfo/$app_id.appdata.xml"
install -d "$(dirname "$icon")"
python3 "$here/extract_icon.py" "$repo/apps/kompacter/icon.ico" "$icon"
chmod 644 "$icon"
install -Dm644 LICENSE "$appdir/usr/share/doc/kompacter/LICENSE"
install -Dm644 "$license" "$appdir/usr/share/doc/kompacter/FFmpeg-LICENSE"
if [ -f "$ffmpeg_dir/VERSION" ]; then
    install -Dm644 "$ffmpeg_dir/VERSION" "$appdir/usr/share/doc/kompacter/FFmpeg-VERSION"
fi

# appimagetool wants the desktop entry and its icon at the top, plus .DirIcon.
ln -s "usr/share/applications/$app_id.desktop" "$appdir/$app_id.desktop"
ln -s "usr/share/icons/hicolor/256x256/apps/$app_id.png" "$appdir/$app_id.png"
ln -s "$app_id.png" "$appdir/.DirIcon"

if [ "${KOMPACTER_APPDIR_ONLY:-}" = 1 ]; then
    printf '%s\n' "$appdir"
    exit 0
fi

image="$out/Kompacter-$version-x86_64.AppImage"
rm -f "$image"
# APPIMAGE_EXTRACT_AND_RUN: appimagetool is an AppImage too; CI machines lack FUSE.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "${APPIMAGETOOL:-appimagetool}" --no-appstream "$appdir" "$image"
rm -rf "$appdir"
(cd "$out" && sha256sum "$(basename "$image")" > "$(basename "$image").sha256")
printf '%s\n' "$image"
