#!/usr/bin/env bash
# Pack Kompacter.app into a drag-to-Applications disk image.
#
#   packaging/macos/make-dmg.sh target/macos/Kompacter.app target/macos/Kompacter-0.2.0-macos-arm64.dmg
#
# KOMPACTER_SIGN_IDENTITY: when set (and not "-"), the image itself is signed too.
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
    echo "Run this script on macOS." >&2
    exit 1
fi
if [[ $# -ne 2 ]]; then
    echo "usage: $0 path/to/Kompacter.app path/to/output.dmg" >&2
    exit 2
fi

app="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
dmg="$2"
[[ -d "$app/Contents/MacOS" ]] || { echo "not an app bundle: $app" >&2; exit 1; }

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
# ditto keeps the bundle's signatures, symlinks and extended attributes intact.
ditto "$app" "$stage/Kompacter.app"
ln -s /Applications "$stage/Applications"
cp "$(dirname "$0")/../../LICENSE" "$stage/LICENSE.txt"

rm -f "$dmg"
hdiutil create -volname Kompacter -srcfolder "$stage" -fs HFS+ -format UDZO \
    -imagekey zlib-level=9 -ov "$dmg"

identity="${KOMPACTER_SIGN_IDENTITY:--}"
if [[ "$identity" != - ]]; then
    codesign --force --timestamp --sign "$identity" "$dmg"
fi
hdiutil verify "$dmg"
