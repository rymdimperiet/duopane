#!/usr/bin/env bash
# demo.sh: builds a made-up home folder full of plausible files and starts
# Duopane in it, for screenshots and screen recordings.
#
#   scripts/demo.sh               build it (if needed) and start Duopane
#   scripts/demo.sh --rebuild     start over with a fresh demo folder
#   scripts/demo.sh --no-launch   only build the folder
#
# Duopane runs with HOME pointing into the demo, so the path bar shows
# "~/Projects" rather than your real paths, and its settings (colour scheme,
# fonts…) are kept apart from your own. Your installed fonts are linked in so
# they can still be picked in Appearance (F12).
#
# DUOPANE_DEMO sets where it goes (default: ~/.cache/duopane-demo).
set -euo pipefail

DEMO="${DUOPANE_DEMO:-${XDG_CACHE_HOME:-$HOME/.cache}/duopane-demo}"
H="$DEMO/home"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
REBUILD=0
LAUNCH=1
for arg in "$@"; do
    case "$arg" in
        --rebuild) REBUILD=1 ;;
        --no-launch) LAUNCH=0 ;;
        -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
        *) echo "Unknown option: $arg" >&2; exit 1 ;;
    esac
done

if [ "$REBUILD" = 1 ] && [ -d "$DEMO" ]; then
    # Only ever delete a folder this script made.
    if [ ! -f "$DEMO/.duopane-demo" ]; then
        echo "$DEMO wasn't made by this script; not deleting it." >&2
        exit 1
    fi
    rm -rf "$DEMO"
fi

# ---- helpers -----------------------------------------------------------------

# file PATH SIZE DAYS_AGO [TEXT]: a file of SIZE bytes (or with TEXT), dated DAYS_AGO
# days back (fractions are fine: 0.1 is a couple of hours ago).
file() {
    local path="$H/$1" size="$2" days="$3"
    mkdir -p "$(dirname "$path")"
    if [ $# -ge 4 ]; then
        printf '%s\n' "$4" > "$path"
    else
        truncate -s "$size" "$path" # sparse: takes no real disk space
    fi
    stamp "$1" "$days"
}

# stamp PATH DAYS_AGO: sets the modification time (DAYS_AGO may be a fraction).
stamp() {
    touch -d "@$(awk -v d="$2" -v now="$(date +%s)" 'BEGIN { printf "%d", now - d * 86400 }')" "$H/$1"
}

# Colours for the pictures: sky top, sky bottom, far hills, near hills, sun.
PALETTES=(
    "#2a1e3f #ff9a5a #6b3a5b #2b1a2e #ffe2b0"   # sunset
    "#0b3a5c #6fc3df #2e5a6b #12303d #e8f6ff"   # ocean evening
    "#8fb8de #f2e6c9 #6f8f5f #2f4a30 #fff8e0"   # forest morning
    "#3b2c5a #f6c177 #7a4b5e #3a2438 #ffd9a0"   # golden dusk
    "#9fb3c2 #eef2f4 #8a9aa6 #52616b #ffffff"   # mist
    "#5d8bbf #f3d9b1 #c98b5a #8c5a3c #fff1d0"   # desert
)
PICS=0

# ridge W H BASE AMP: the outline of a range of hills, as polygon points.
ridge() {
    local w=$1 h=$2 base=$3 amp=$4 pts="0,$h" x
    for x in $(seq 0 $((w / 6)) "$w"); do
        pts="$pts $x,$((base - amp + RANDOM % (2 * amp + 1)))"
    done
    echo "$pts $w,$((base - amp + RANDOM % (2 * amp + 1))) $w,$h"
}

# picture PATH WIDTH HEIGHT DAYS_AGO: a real picture (a simple landscape), so the
# viewer has something to show. Drawn small and scaled up: soft and quick.
picture() {
    mkdir -p "$(dirname "$H/$1")"
    if command -v magick >/dev/null; then
        local top bot far near sun
        read -r top bot far near sun <<< "${PALETTES[$((PICS % ${#PALETTES[@]}))]}"
        PICS=$((PICS + 1))
        local w=$(( $2 / 4 )) h=$(( $3 / 4 ))
        local sx=$(( w / 5 + RANDOM % (w * 3 / 5) )) sy=$(( h * 2 / 5 + RANDOM % (h / 6) )) r=$(( h / 9 ))
        magick -size "${w}x${h}" "gradient:$top-$bot" \
            -fill "$sun" -draw "circle $sx,$sy $((sx + r)),$sy" -blur 0x2 \
            -fill "$far" -draw "polygon $(ridge "$w" "$h" $((h * 62 / 100)) $((h / 12)))" \
            -fill "$near" -draw "polygon $(ridge "$w" "$h" $((h * 80 / 100)) $((h / 14)))" \
            -blur 0x0.6 -resize "${2}x${3}!" -attenuate 0.12 +noise Gaussian -quality 88 "$H/$1"
    else
        truncate -s $(( $2 * $3 / 3 )) "$H/$1"
    fi
    stamp "$1" "$4"
}

if [ ! -f "$DEMO/.duopane-demo" ]; then
    echo "Building the demo folder in $DEMO …"
    mkdir -p "$H"
    touch "$DEMO/.duopane-demo"

    # ---- Projects ------------------------------------------------------------
    file "Projects/aurora/Cargo.toml" 0 3 '[package]
name = "aurora"
version = "0.4.1"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
toml = "0.8"'
    file "Projects/aurora/README.md" 0 3 '# Aurora

A tiny static-site generator: Markdown in, HTML out.

## Usage

```sh
aurora build ./site
aurora serve --port 8080
```

## Features

- Fast incremental builds
- Templates with partials
- RSS and sitemap out of the box'
    file "Projects/aurora/src/main.rs" 0 1 'use std::path::PathBuf;

mod build;
mod serve;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => build::run(&PathBuf::from(args.get(1).map_or(".", |s| s))),
        Some("serve") => serve::run(8080),
        _ => eprintln!("usage: aurora build <dir> | serve"),
    }
}'
    file "Projects/aurora/src/build.rs" 4200 1
    file "Projects/aurora/src/serve.rs" 2600 2
    file "Projects/aurora/src/templates.rs" 7900 5
    file "Projects/aurora/tests/build.rs" 3100 6
    file "Projects/aurora/.gitignore" 0 30 '/target'
    file "Projects/aurora/LICENSE" 1070 30
    file "Projects/aurora/Cargo.lock" 18400 3
    file "Projects/website/index.html" 6100 12
    file "Projects/website/about.html" 4300 12
    file "Projects/website/css/style.css" 9800 9
    file "Projects/website/js/menu.js" 1500 20
    picture "Projects/website/img/hero.jpg" 1600 900 12
    picture "Projects/website/img/logo.png" 256 256 40
    file "Projects/scripts/backup.sh" 0 45 '#!/bin/sh
# Nightly backup of ~/Documents to the NAS
rsync -a --delete ~/Documents/ nas:/backup/documents/'
    file "Projects/scripts/resize-photos.py" 2400 60
    file "Projects/scripts/rename-episodes.sh" 900 75
    file "Projects/game-jam-2026/design.md" 0 18 '# Lanternfall: design notes

Theme: "Light in the dark". You play a lantern keeper walking a mountain path
at night. Every lantern you light pushes the fog back a little.

- 2D, top-down, pixel art (16x16 tiles)
- 5 levels, ~15 minutes in all
- Music: slow synth pads'
    file "Projects/game-jam-2026/lanternfall.godot" 3200 17
    picture "Projects/game-jam-2026/art/tileset.png" 512 512 17
    picture "Projects/game-jam-2026/art/player.png" 128 128 16

    # ---- Documents -----------------------------------------------------------
    file "Documents/Budget 2026.ods" 48300 4
    file "Documents/Taxes/2025 tax return.pdf" 412000 160
    file "Documents/Taxes/receipts-2025.zip" 2310000 158
    file "Documents/Taxes/2024 tax return.pdf" 398000 520
    file "Documents/Work/Q4 planning.odt" 31200 8
    file "Documents/Work/Quarterly report.pdf" 1840000 9
    file "Documents/Work/Team offsite agenda.md" 0 11 '# Team offsite: agenda

**Day 1**
- 09:00 Coffee and catch-up
- 10:00 Where we are: roadmap review
- 13:00 Lunch by the lake
- 14:30 Workshops: onboarding, release process

**Day 2**
- 09:30 Hack morning
- 12:00 Demos and wrap-up'
    file "Documents/Recipes/Cardamom buns.md" 0 25 '# Cardamom buns

Dough: 500 g flour, 75 g sugar, 25 g yeast, 300 ml milk, 100 g butter,
1 tbsp ground cardamom.

Filling: 100 g butter, 75 g sugar, 1 tbsp cardamom.

Knead, rest 45 min, roll out, fill, twist, rest 30 min, bake 8 min at 225 °C.'
    file "Documents/Recipes/Lentil soup.md" 1100 70
    file "Documents/Recipes/Sourdough starter.md" 2300 120
    file "Documents/Manuals/Dishwasher SN65.pdf" 3400000 400
    file "Documents/Manuals/Router setup guide.pdf" 1250000 300
    file "Documents/notes.txt" 0 0.1 'Call the plumber about the kitchen tap
Renew passport before March
Book train tickets for the 14th
Return library books'
    file "Documents/Letter to the housing association.odt" 18700 33

    # ---- Pictures --------------------------------------------------------------
    for i in $(seq 1 12); do
        picture "Pictures/Holiday 2026/IMG_$((4410 + i)).jpg" 1200 800 $((95 - i))
    done
    for i in 1 2 3 4 5; do
        picture "Pictures/Wallpapers/wallpaper-0$i.jpg" 1920 1080 $((200 + i * 9))
    done
    picture "Pictures/Screenshots/Screenshot_20261102_142233.png" 1280 800 17
    picture "Pictures/Screenshots/Screenshot_20261110_091504.png" 1280 800 9
    picture "Pictures/avatar.png" 400 400 300

    # ---- Music (sizes only) ----------------------------------------------------
    track=1
    for t in "Night Train" "Glass Harbour" "Paper Satellites" "Slow Weather" "Afterglow" "Northbound"; do
        file "Music/The Lanterns/Low Tide (2024)/0$track - $t.flac" $(( 24000000 + track * 3100000 )) 600
        track=$((track + 1))
    done
    picture "Music/The Lanterns/Low Tide (2024)/cover.jpg" 600 600 600
    track=1
    for t in "Static Bloom" "Copper Sky" "Undertow" "Small Hours"; do
        file "Music/Mira Vale/Undertow EP/0$track - $t.mp3" $(( 7200000 + track * 900000 )) 240
        track=$((track + 1))
    done
    file "Music/Playlists/Running.m3u" 640 50
    file "Music/Playlists/Focus.m3u" 980 12

    # ---- Videos (sizes only) ---------------------------------------------------
    file "Videos/Holiday 2026/Boat trip.mp4" 412000000 88
    file "Videos/Holiday 2026/Sunset timelapse.mp4" 96000000 86
    file "Videos/Talks/Writing a file manager in Rust.mkv" 1320000000 40
    file "Videos/Screen recording 2026-11-10.webm" 38500000 9

    # ---- Downloads ---------------------------------------------------------------
    file "Downloads/ubuntu-26.04-desktop-amd64.iso" 6200000000 21
    file "Downloads/firmware-update-v2.3.bin" 16800000 50
    file "Downloads/Invoice INV-0042.pdf" 84000 2
    file "Downloads/Invoice INV-0043.pdf" 86500 0.3
    file "Downloads/concert-tickets.pdf" 212000 14
    file "Downloads/font-specimen.ttf" 312000 35
    picture "Downloads/IMG_20261117_183012.jpg" 1600 1200 2
    picture "Downloads/diagram.png" 1000 700 6
    # Real archives, so they can be browsed like folders (Enter) and unpacked (Alt+F9).
    (cd "$H/Projects" && zip -qr "$H/Downloads/website-backup.zip" website)
    stamp "Downloads/website-backup.zip" 3
    tar -czf "$H/Downloads/aurora-0.4.1.tar.gz" -C "$H/Projects" aurora
    stamp "Downloads/aurora-0.4.1.tar.gz" 1

    # ---- Desktop and the top level -------------------------------------------------
    file "Desktop/todo.md" 0 0.05 '# This week

- [x] Fix the leaking tap
- [ ] Send the quarterly report
- [ ] Pick up the bike from the shop
- [ ] Practise the talk once more'
    file "Desktop/Meeting notes 2026-11-18.txt" 1800 1
    mkdir -p "$H/Public" "$H/Templates"
    file ".bashrc" 3800 200
    file ".config/aurora/config.toml" 300 30
    file ".local/share/recently-used.xbel" 9100 0.2
    # Each folder dated by its newest file, deepest first, like a real disk.
    find "$H" -depth -mindepth 1 -type d -print0 | while IFS= read -r -d '' d; do
        newest="$(ls -At "$d" | head -n 1)"
        [ -n "$newest" ] && touch -r "$d/$newest" "$d"
    done
    stamp "Public" 300
    stamp "Templates" 300
    echo "Done."
fi

# Duopane looks for your fonts under HOME; link them into the demo home.
mkdir -p "$H/.local/share"
[ -d "$HOME/.local/share/fonts" ] && [ ! -e "$H/.local/share/fonts" ] && ln -s "$HOME/.local/share/fonts" "$H/.local/share/fonts"
[ -d "$HOME/.fonts" ] && [ ! -e "$H/.fonts" ] && ln -s "$HOME/.fonts" "$H/.fonts"

[ "$LAUNCH" = 1 ] || { echo "Demo folder: $H"; exit 0; }

BIN="$REPO/target/release/duopane"
if [ ! -x "$BIN" ]; then
    echo "Building Duopane first …"
    (cd "$REPO" && cargo build --release)
fi
echo "Starting Duopane in $H (its settings are kept in $H/.config)."
HOME="$H" XDG_CONFIG_HOME="$H/.config" XDG_DATA_HOME="$H/.local/share" XDG_CACHE_HOME="$H/.cache" \
    exec "$BIN" "$H/Projects" "$H/Pictures/Holiday 2026"
