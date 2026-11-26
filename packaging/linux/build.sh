#!/bin/sh
# Build the amd64 Debian package and a relocatable test bundle.
set -eu
umask 022

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH='' cd -- "$here/../.." && pwd)
cd "$repo"

out=${1:-"$repo/dist/linux"}
mkdir -p "$out"
out=$(CDPATH='' cd -- "$out" && pwd)
# The release workflow passes the tag's version; local builds use Cargo.toml's.
version=${KOMPACTER_PACKAGE_VERSION:-${KOMPACTER_VERSION:-}}
if [ -z "$version" ]; then
    version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
fi
app_id=io.github.KXZer0.Kompacter
target=x86_64-unknown-linux-gnu

if [ -z "${KOMPACTER_LINUX_BINARY:-}" ]; then
    cargo build --locked --release --target "$target" -p kompacter --bin Kompacter
fi
binary=${KOMPACTER_LINUX_BINARY:-"${CARGO_TARGET_DIR:-target}/$target/release/Kompacter"}
binary=$(realpath "$binary")
readelf -h "$binary" | grep -q 'Machine:.*Advanced Micro Devices X86-64'
desktop-file-validate "$here/$app_id.desktop"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT HUP INT TERM
deb="$stage/deb"
bundle_name="Kompacter-$version-linux-x86_64"
bundle="$stage/$bundle_name"
icon="$repo/apps/kompacter/frontend/tray.png"

install -Dm755 "$binary" "$deb/usr/lib/kompacter/Kompacter"
install -d "$deb/usr/bin"
ln -s ../lib/kompacter/Kompacter "$deb/usr/bin/kompacter"
install -Dm644 "$here/$app_id.desktop" "$deb/usr/share/applications/$app_id.desktop"
install -Dm644 "$icon" "$deb/usr/share/icons/hicolor/32x32/apps/$app_id.png"
install -d "$deb/usr/share/icons/hicolor/256x256/apps"
python3 "$here/extract_icon.py" "$repo/apps/kompacter/icon.ico" \
    "$deb/usr/share/icons/hicolor/256x256/apps/$app_id.png"
chmod 644 "$deb/usr/share/icons/hicolor/256x256/apps/$app_id.png"
install -Dm644 "$here/$app_id.metainfo.xml" "$deb/usr/share/metainfo/$app_id.metainfo.xml"
install -Dm644 LICENSE "$deb/usr/share/doc/kompacter/copyright"

# Let dpkg-shlibdeps derive the actual libc floor of the build machine.
mkdir -p "$stage/debian" "$deb/DEBIAN"
printf 'Source: kompacter\n\nPackage: kompacter\nArchitecture: amd64\n' > "$stage/debian/control"
depends=$(cd "$stage" && dpkg-shlibdeps -O -e"$deb/usr/lib/kompacter/Kompacter")
depends=${depends#shlibs:Depends=}
installed_size=$(du -sk "$deb" | cut -f1)
cat > "$deb/DEBIAN/control" <<CONTROL
Package: kompacter
Version: $version
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Kompacter contributors <https://github.com/KXZer0/Kompacter/issues>
Installed-Size: $installed_size
Depends: $depends, ffmpeg, libegl1, libgl1, libxkbcommon0, libwayland-client0, xdg-utils
Recommends: xdg-desktop-portal, xdg-desktop-portal-gtk | xdg-desktop-portal-kde | xdg-desktop-portal-gnome, zenity
Homepage: https://github.com/KXZer0/Kompacter
Description: Shrink files to fit an upload limit
 Compress media and other files from a compact desktop panel.
CONTROL
dpkg-deb --root-owner-group --build "$deb" "$out/kompacter_${version}_amd64.deb"

install -Dm755 "$binary" "$bundle/Kompacter"
install -Dm644 LICENSE "$bundle/LICENSE"
install -Dm644 "$here/PORTABLE.md" "$bundle/README.md"
install -Dm755 "$here/install-launcher.sh" "$bundle/install-launcher.sh"
install -Dm644 "$here/$app_id.desktop" "$bundle/$app_id.desktop"
install -Dm644 "$icon" "$bundle/$app_id.png"
if [ -n "${KOMPACTER_FFMPEG_DIR:-}" ]; then
    install -m755 "$KOMPACTER_FFMPEG_DIR/ffmpeg" "$KOMPACTER_FFMPEG_DIR/ffprobe" "$bundle/"
    for name in LICENSE LICENSE.txt; do
        if [ -f "$KOMPACTER_FFMPEG_DIR/$name" ]; then
            install -Dm644 "$KOMPACTER_FFMPEG_DIR/$name" "$bundle/FFmpeg-LICENSE"
            break
        fi
    done
fi
tar -C "$stage" -czf "$out/$bundle_name.tar.gz" "$bundle_name"
(cd "$out" && sha256sum "kompacter_${version}_amd64.deb" "$bundle_name.tar.gz" > SHA256SUMS)
printf '%s\n' "$out/kompacter_${version}_amd64.deb" "$out/$bundle_name.tar.gz"
