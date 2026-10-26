#!/bin/sh
# Standalone install. NIRUX owns the normal suite installation.
set -eu
PREFIX=${NIRUDISPLAY_PREFIX:-${NIRUX_PREFIX:-$HOME/.local}}
BIN=$PREFIX/bin
APPS=$PREFIX/share/applications
ICONS=$PREFIX/share/icons/hicolor/256x256/apps
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
install -d "$BIN" "$APPS" "$ICONS"
install -m 755 "$here/target/release/nirudisplay" "$BIN/nirudisplay"
sed "s|^Exec=nirudisplay$|Exec=$BIN/nirudisplay|" "$here/nirudisplay.desktop" > "$APPS/nirudisplay.desktop"
install -m 644 "$here/assets/nirudisplay.png" "$ICONS/nirudisplay.png"
printf 'installed %s\n' "$BIN/nirudisplay"
