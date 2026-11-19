#!/bin/sh
# install.sh: builds Duopane and adds it to the desktop's application menu (for this user).
set -e
cargo install --path .
icons="$HOME/.local/share/icons/hicolor/scalable/apps"
apps="$HOME/.local/share/applications"
mkdir -p "$icons" "$apps"
cp assets/duopane_icon.svg "$icons/duopane.svg"
sed "s|@HOME@|$HOME|" assets/duopane.desktop > "$apps/duopane.desktop"
# Optional helpers: not every desktop has them.
command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" || true
