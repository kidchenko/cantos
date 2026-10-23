#!/usr/bin/env bash
#
# Installs Cantos for the current user from the release tarball, no root:
#
#   ~/.local/bin/cantos
#   ~/.local/share/applications/cantos.desktop
#   ~/.local/share/icons/hicolor/256x256/apps/cantos.png
#
#   ./install.sh              # install
#   ./install.sh --uninstall  # remove those three files (settings are kept)
set -euo pipefail
cd "$(dirname "$0")"

bin="$HOME/.local/bin"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"

if [ "${1:-}" = "--uninstall" ]; then
  pkill -x cantos 2>/dev/null || true
  rm -f "$bin/cantos" "$apps/cantos.desktop" "$icons/cantos.png"
  rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/cantos.desktop"
  echo "Removed Cantos. Settings remain in ${XDG_CONFIG_HOME:-$HOME/.config}/cantos."
  exit 0
fi

install -Dm755 cantos "$bin/cantos"
# The menu entry runs `cantos` by name, so point it at the full path in case
# ~/.local/bin is not on the PATH the desktop session started with.
install -d "$apps"
sed "s#^Exec=cantos#Exec=$bin/cantos#" cantos.desktop > "$apps/cantos.desktop"
install -Dm644 cantos.png "$icons/cantos.png"
command -v update-desktop-database >/dev/null && update-desktop-database -q "$apps" || true

echo "Installed to $bin/cantos. Launching it; find it in your tray."
nohup "$bin/cantos" --settings >/dev/null 2>&1 &
