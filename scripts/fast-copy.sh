#!/usr/bin/env bash
# Puts the archives the title streams most on fast storage, links the rest.
#
#   scripts/fast-copy.sh <dump on a slow disk> <folder on an SSD>
#
# From a hard disk the title's own streaming (random reads of levels.rpf and
# the texture tiles of data/ttdb) left textures waiting a mean 23 s for their
# data; from an NVMe SSD, 3 s. The whole dump is about 225 GB; this copies
# about 125 GB and links everything else, so the folder works as the dump:
# point VICTORY_GAME_DIR (or $VICTORY_HOME/game-dir) at it.
#
# On btrfs the folder is made a subvolume of its own: snapshots of the
# volume around it (snapper, timeshift) then leave it out, instead of pinning
# 100 GB of game data every hour.
#
# Set COPY="<paths relative to the dump>" to choose what is copied.
set -euo pipefail

src="${1:?usage: fast-copy.sh <dump> <ssd folder>}"
dst="${2:?usage: fast-copy.sh <dump> <ssd folder>}"
src="$(cd "$src" && pwd)"
[[ -f "$src/game_scarlett_master.exe" ]] || { echo "$src does not look like the dump (no game_scarlett_master.exe)." >&2; exit 1; }

# data/ttdb/maps (14 GB) holds the map's texture tiles, three quarters of all
# streaming reads in the world: left on a hard disk they came at about 100 a
# second and textures stayed missing.
COPY="${COPY:-levels.rpf data/ttdb/maps data/ttdb/interiors data/ttdb/terrain data/ttdb/peds data/ttdb/props data/ttdb/ui data/ttdb/vfx data/ttdb/lighting data/ttdb/graphics}"

need=0
for rel in $COPY; do need=$((need + $(du -sb "$src/$rel" | cut -f1))); done
parent="$(dirname "$dst")"
mkdir -p "$parent"
free=$(( $(df --output=avail -B1 "$parent" | tail -1) ))
awk -v n="$need" -v f="$free" 'BEGIN { printf "To copy: %.1f GB; free there: %.1f GB.\n", n / 1e9, f / 1e9 }'
if (( need + 10 * 1024**3 > free )); then
    echo "Not enough room (keeping 10 GB free). Choose less with COPY=\"...\"." >&2
    exit 1
fi

if [[ ! -e "$dst" ]]; then
    if [[ "$(stat -f -c %T "$parent")" == btrfs ]] && btrfs subvolume create "$dst" >/dev/null 2>&1; then
        echo "$dst: a btrfs subvolume (left out of snapshots of the volume around it)."
    else
        mkdir -p "$dst"
    fi
fi

# Every entry a link to the dump; the folders on the way to a copied path
# become real folders of links, so one subfolder can be copied alone.
link_level() { # <relative folder>
    local rel="$1" f b
    mkdir -p "$dst/$rel"
    for f in "$src/$rel"/*; do
        b="$(basename "$f")"
        [[ -e "$dst/$rel/$b" || -L "$dst/$rel/$b" ]] || ln -s "$f" "$dst/$rel/$b"
    done
}
link_level .
for rel in $COPY; do
    parts=""
    IFS=/ read -ra segs <<< "$(dirname "$rel")"
    for seg in "${segs[@]}"; do
        [[ $seg == . ]] && continue
        parts="${parts:+$parts/}$seg"
        [[ -L "$dst/$parts" ]] && rm "$dst/$parts"
        link_level "$parts"
    done
done

for rel in $COPY; do
    if [[ -e "$dst/$rel" && ! -L "$dst/$rel" ]]; then
        echo "already copied: $rel"
        continue
    fi
    echo "copying $rel ..."
    rm -rf "$dst/$rel.part"
    cp -a "$src/$rel" "$dst/$rel.part"
    rm -f "$dst/$rel"
    mv "$dst/$rel.part" "$dst/$rel"
done
echo "Done. Use it with: VICTORY_GAME_DIR=\"$dst\" scripts/play-linux.sh"
