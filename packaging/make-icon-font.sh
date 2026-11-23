#!/bin/sh
# Cuts assets/icons.ttf out of a Nerd Font: just the icons src/icons.rs uses.
# Needs fontTools (pyftsubset) and a Nerd Font; pass its path to use another.
set -e
cd "$(dirname "$0")/.."
src="${1:-/usr/share/fonts/TTF/MesloLGSNerdFont-Regular.ttf}"
codes=$(grep -o "'\\\\u{[0-9a-f]*}'" src/icons.rs | grep -o '[0-9a-f]\{4,\}' | sort -u | grep -v '^0*$' | sed 's/^/U+/' | paste -sd, -)
pyftsubset "$src" --unicodes="$codes" --output-file=assets/icons.ttf \
    --no-hinting --desubroutinize --drop-tables+=FFTM,GDEF,GPOS,GSUB --name-IDs=0,1,2,3,4,5,6
echo "assets/icons.ttf: $(echo "$codes" | tr ',' '\n' | wc -l) icons, $(wc -c < assets/icons.ttf) bytes"
