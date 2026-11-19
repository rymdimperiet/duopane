#!/bin/sh
# install.sh: builds Duopane and adds it to the desktop's application menu (for this user).
set -e
cargo install --path .
icons="$HOME/.local/share/icons/hicolor/scalable/apps"
apps="$HOME/.local/share/applications"
mkdir -p "$icons" "$apps"
cp packaging/duopane.svg "$icons/duopane.svg"
# The menu entry points at where cargo installed the program (it may not be on
# the desktop's PATH).
bin="${CARGO_HOME:-$HOME/.cargo}/bin/duopane"
sed "s|^Exec=duopane|Exec=$bin|" packaging/duopane.desktop > "$apps/duopane.desktop"
# Optional helpers: not every desktop has them.
command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" || true
