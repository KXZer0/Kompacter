#!/bin/sh
# Register this extracted bundle for the current desktop user.
set -eu
bundle=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
data=${XDG_DATA_HOME:-"$HOME/.local/share"}
app_id=io.github.KXZer0.Kompacter
desktop="$data/applications/$app_id.desktop"
install -d "$data/applications"
install -Dm644 "$bundle/$app_id.png" "$data/icons/hicolor/32x32/apps/$app_id.png"

# Exec's double-quoted argument is decoded twice by desktop-entry parsers.
# Escape percent field codes as well, since the bundle may live in any directory.
exec_path=$(printf '%s' "$bundle/Kompacter" | sed 's/[\\]/\\\\\\\\/g; s/["`$]/\\\\&/g; s/%/%%/g')
sed "s|^Exec=kompacter|Exec=\"$(printf '%s' "$exec_path" | sed 's/[\\&|]/\\&/g')\"|" \
    "$bundle/$app_id.desktop" > "$desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data/applications"
fi
printf 'Installed %s\n' "$desktop"
