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
    # A fixed release, checked against its known checksum before it's run, so a
    # changed download can't end up building the AppImage.
    TOOL_VERSION=1.9.1
    case "$ARCH" in
        x86_64) TOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 ;;
        aarch64) TOOL_SHA256=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158 ;;
        *) echo "No known appimagetool checksum for $ARCH; set APPIMAGETOOL to one you trust." >&2; exit 1 ;;
    esac
    mkdir -p target/appimage-tools
    TOOL="$(pwd)/target/appimage-tools/appimagetool-$TOOL_VERSION-$ARCH.AppImage"
    if [ ! -x "$TOOL" ]; then
        echo "Downloading appimagetool $TOOL_VERSION…"
        curl -fL -o "$TOOL.part" "https://github.com/AppImage/appimagetool/releases/download/$TOOL_VERSION/appimagetool-$ARCH.AppImage"
        mv "$TOOL.part" "$TOOL"
        chmod +x "$TOOL"
    fi
    if ! echo "$TOOL_SHA256  $TOOL" | sha256sum -c --quiet -; then
        echo "appimagetool's checksum is wrong; removed it." >&2
        rm -f "$TOOL"
        exit 1
    fi
fi

RESULT="$OUT/Duopane-$VERSION-$ARCH.AppImage"
rm -f "$RESULT"
# APPIMAGE_EXTRACT_AND_RUN lets appimagetool (itself an AppImage) run without FUSE.
ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$RESULT"
echo
echo "Done: $RESULT"
