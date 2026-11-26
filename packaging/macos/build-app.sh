#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
    echo "Run this script on macOS." >&2
    exit 1
fi

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
cd "$repo"

target="${CARGO_TARGET_DIR:-target}"
destination="${1:-$target/macos}"
version="${KOMPACTER_VERSION:-}"
if [[ -z "$version" ]]; then
    version="$(cargo metadata --locked --no-deps --format-version 1 | python3 -c \
        'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "kompacter"))')"
fi
version="${version%%-*}"

cargo build --locked --release -p kompacter
mkdir -p "$destination"
stage="$(mktemp -d "$destination/.kompacter.XXXXXXXX")"
trap 'rm -rf "$stage"' EXIT
app="$stage/Kompacter.app"
contents="$app/Contents"
macos="$contents/MacOS"
resources="$contents/Resources"
mkdir -p "$macos" "$resources"

cp "$here/Info.plist" "$contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$contents/Info.plist"
plutil -lint "$contents/Info.plist"
install -m 755 "$target/release/Kompacter" "$macos/Kompacter"
cp LICENSE "$resources/Kompacter-LICENSE.txt"

# macOS accepts an ICNS containing the existing 256 px app artwork.
python3 "$here/make_icon.py" "$repo/apps/kompacter/icon.ico" "$resources/Kompacter.icns"

formula="$(brew --prefix ffmpeg)"
python3 "$here/bundle_ffmpeg.py" "$formula/bin" "$macos" "$contents/Frameworks"
brew info --json=v2 ffmpeg > "$resources/FFmpeg-Homebrew.json"

# Replacing install names invalidates the original signatures. Ad-hoc sign
# the Mach-O files, then the outer review bundle.
while IFS= read -r -d '' binary; do
    codesign --force --sign - "$binary"
done < <(find "$contents/Frameworks" "$macos" -type f -print0)
"$macos/ffmpeg" -L > "$resources/FFmpeg-LICENSE.txt"
"$macos/ffmpeg" -buildconf > "$resources/FFmpeg-buildconf.txt" 2>&1
codesign --force --sign - "$app"
codesign --verify --deep --strict "$app"

# Replace just the generated app and archive in the selected output directory.
rm -rf "$destination/Kompacter.app"
mv "$app" "$destination/Kompacter.app"
archive="Kompacter-$(uname -m).zip"
rm -f "$destination/$archive"
ditto -c -k --keepParent "$destination/Kompacter.app" "$destination/$archive"
(cd "$destination" && shasum -a 256 "$archive" > SHA256SUMS)
