#!/bin/sh
# install.sh
set -e
cargo install --path .
# mkdir -p ~/.local/share/applications ~/.local/share/icons/hicolor/256x256/apps
cp assets/duopane_icon.svg ~/.local/share/icons/duopane.svg
sed "s|@HOME@|$HOME|" assets/duopane.desktop > ~/.local/share/applications/duopane.desktop
update-desktop-database ~/.local/share/applications
