#!/bin/sh
# install.sh: builds Duopane and adds it to the desktop's application menu (for this user).
set -e
cargo install --path .
share="$HOME/.local/share"
# The menu entry points at where cargo installed the program (it may not be on
# the desktop's PATH).
bin="${CARGO_HOME:-$HOME/.cargo}/bin/duopane"
install -Dm644 packaging/duopane.svg "$share/icons/hicolor/scalable/apps/duopane.svg"
sed "s|^Exec=duopane|Exec=$bin|" packaging/duopane.desktop > packaging/.duopane.desktop.tmp
install -Dm644 packaging/.duopane.desktop.tmp "$share/applications/duopane.desktop"
rm -f packaging/.duopane.desktop.tmp
# Lets the menu notice the new entry; not every desktop has it, and it's not needed.
# (No icon cache update: icons in a folder newer than the cache are found anyway.)
command -v update-desktop-database >/dev/null && update-desktop-database "$share/applications" || true
