#!/bin/sh
# Builds a single-file Duopane AppImage:  sh packaging/build-appimage.sh
#
# The result is target/appimage/Duopane-<version>-<arch>.AppImage. It runs on Linux
# systems with the same or a newer C library (glibc) than the one it was built on,
# so for the widest reach build on an older distribution (GitHub does this for you:
# see .github/workflows/release.yml). Viewing PDFs and videos still needs poppler
# and ffmpeg on the computer that runs it.
#
# Needs appimagetool; if it isn't installed it is downloaded once from the official
# AppImage project into target/appimage-tools/.
set -eu
cd "$(dirname "$0")/.."

ARCH="$(uname -m)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
# Full paths: appimagetool may run from another folder when it runs without FUSE.
OUT="$(pwd)/target/appimage"
APPDIR="$OUT/Duopane.AppDir"

echo "Building Duopane $VERSION ($ARCH)…"
cargo build --release

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
cp target/release/duopane "$APPDIR/usr/bin/"
cp packaging/AppRun "$APPDIR/AppRun"
chmod +x "$APPDIR/AppRun"
cp packaging/duopane.desktop "$APPDIR/duopane.desktop"
cp packaging/duopane.desktop "$APPDIR/usr/share/applications/"
cp packaging/duopane.png "$APPDIR/duopane.png"
cp packaging/duopane.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/"

TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
if [ -z "$TOOL" ]; then
    mkdir -p target/appimage-tools
    TOOL="$(pwd)/target/appimage-tools/appimagetool-$ARCH.AppImage"
    if [ ! -x "$TOOL" ]; then
        echo "Downloading appimagetool…"
        curl -fL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
        chmod +x "$TOOL"
    fi
fi

RESULT="$OUT/Duopane-$VERSION-$ARCH.AppImage"
rm -f "$RESULT"
# APPIMAGE_EXTRACT_AND_RUN lets appimagetool (itself an AppImage) run without FUSE.
ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$RESULT"
echo
echo "Done: $RESULT"
