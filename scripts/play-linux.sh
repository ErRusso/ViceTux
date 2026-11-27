#!/usr/bin/env bash
# Starts the title on Linux under Proton (vkd3d-proton), the Linux counterpart
# of scripts/play.ps1. Build first (cross-compiled with cargo-xwin):
#   cargo xwin build --locked --profile diag -p victory --features diagnostics --target x86_64-pc-windows-msvc
#
#   scripts/play-linux.sh [--machine 4060|4070|4090] [--console series-s|series-x]
#                         [--windowed] [--no-panel] [--safe] [--raytracing [--rt-shadows] | --rt-low] [--dlss quality|balanced|performance] [--breadcrumbs] [--cmd run|shaders|warm]
#                         [KEY=VALUE ...]   extra VICTORY_* overrides
#
# Paths come from the environment (defaults in brackets):
#   VICTORY_GAME_DIR  [the first line of $VICTORY_HOME/game-dir]  your dump
#   VICTORY_HOME      [~/.local/share/vice-tay]  cache, saves and the Proton prefix
#   VICTORY_PREFIX    [$VICTORY_HOME/prefix]  the Proton prefix (one per Proton build)
#   PROTON            [~/.steam/steam/steamapps/common/Proton - Experimental/proton]
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home_default="$HOME/.local/share/vice-tay"
home="${VICTORY_HOME:-$home_default}"
# Your dump: VICTORY_GAME_DIR, else the first line of $VICTORY_HOME/game-dir.
# Keep it on an SSD: from a hard disk the title's streaming left textures
# waiting a mean 23 s for their data (scripts/fast-copy.sh puts the
# archives it streams most on fast storage and links the rest).
game_dir="${VICTORY_GAME_DIR:-}"
if [[ -z $game_dir && -f "$home/game-dir" ]]; then
    game_dir="$(head -n1 "$home/game-dir")"
fi
if [[ -z $game_dir ]]; then
    echo "Set VICTORY_GAME_DIR to your dump's folder, or put that path in $home/game-dir." >&2
    exit 1
fi
# A Proton Experimental whose wineserver keeps mapped views in a tree
# (kroeeee-eeee/VIctory-Linux-Proton-Wine-7900XT-XTX, linux/wineserver):
# links to the installed one, its own files/bin. With the stock server every
# map and unmap walks all of the title's tens of thousands of views, and the
# write watch took 25-90 ms a frame here. Built for experimental-11.0-20261112
# (wine 5b557605); after a Proton update rebuild it, or PROTON= the stock one.
viewtree="$home_default/proton-experimental-viewtree/proton"
if [[ -z ${PROTON:-} && -x "$viewtree" ]] \
    && cmp -s "$(dirname "$viewtree")/built-against-version" "$HOME/.steam/steam/steamapps/common/Proton - Experimental/version"; then
    proton="$viewtree"
else
    [[ -z ${PROTON:-} && -x "$viewtree" ]] && echo "Proton Experimental was updated: the view-tree wineserver is for another build; using the stock one."
    proton="${PROTON:-$HOME/.steam/steam/steamapps/common/Proton - Experimental/proton}"
fi
build="${VICTORY_BUILD:-diag}"
victory="$repo/target/x86_64-pc-windows-msvc/$build/victory.exe"

machine=auto console=series-s windowed=0 panel=1 safe=0 rt=0 rt_shadows=0 rt_low=0 dlss= crumbs=0 cmd=run
extra=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --machine) machine="$2"; shift 2 ;;
        --console) console="$2"; shift 2 ;;
        --windowed) windowed=1; shift ;;
        --no-panel) panel=0; shift ;;
        --safe) safe=1; shift ;;
        --raytracing) rt=1; shift ;;
        --rt-shadows) rt_shadows=1; shift ;;
        --rt-low) rt=1; rt_low=1; shift ;;
        --dlss) dlss="$2"; shift 2 ;;
        --breadcrumbs) crumbs=1; shift ;;
        --cmd) cmd="$2"; shift 2 ;;
        VICTORY_*=*) extra+=("$1"); shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

[[ -f "$victory" ]] || { echo "No $victory. Build it first (see the top of this script)." >&2; exit 1; }
[[ -x "$proton" ]] || { echo "No Proton at $proton (set PROTON=)." >&2; exit 1; }
exe="$game_dir/game_scarlett_master.exe"
[[ -f "$exe" ]] || { echo "No $exe (set VICTORY_GAME_DIR=)." >&2; exit 1; }

if [[ $machine == auto ]]; then
    gpu="$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1 || true)"
    case "$gpu" in
        *4060*|*3060*|*4050*) machine=4060 ;;
        *4070*|*5070*) machine=4070 ;;
        *NVIDIA*|*GeForce*) machine=4090 ;;
        *) machine=7900xtx ;;
    esac
fi

winpath() { echo "Z:${1//\//\\}"; }
mkdir -p "$home/cache" "$home/data"
cat > "$home/victory.toml" <<EOF
[paths]
game_dir = '$(winpath "$game_dir")'
cache_dir = '$(winpath "$home/cache")'
data_dir = '$(winpath "$home/data")'
test_titles = []
EOF

# Clear leftovers so only what is set here reaches the layer.
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep '^VICTORY_' || true)
export VICTORY_CONFIG="$(winpath "$home/victory.toml")"

# Shared with play.ps1.
export VICTORY_TEXTURE_NO_WAIT=1 VICTORY_RELAY=2 VICTORY_WIDE_BACK=1 VICTORY_CARD_REHASH_RUNS=1
export VICTORY_JOIN_COPIES=1 VICTORY_INDIRECT_WINDOW=1 VICTORY_RELAY_COPY=1
# Card steps at stage 1, not play.ps1's 2: with the title's buffer copies done
# on the card behind flights in the air (stage 2) the card faulted reading an
# address of the title's space through its constant cache (NVIDIA Xid 31 at
# 0x5_8d249000 and alike), 3-5 minutes into every run from the prison save.
# Bisected on an RTX 4060 under Proton: relay 1, card steps 0 and card steps
# 1 each ran 9 minutes clean (stage 1 twice); stage 2 faulted every time,
# with or without the relay's copy queue. Gameplay: 13.2 fps at stage 1,
# 16-17 at stage 2, 10.6 with none.
export VICTORY_CARD_STEPS="${CARD_STEPS:-1}"
export VICTORY_TITLE_PRIORITIES=0 VICTORY_SHOW_AFTER=1 VICTORY_AMBIENT=0.8 VICTORY_D3D12_DRED=light

if [[ $machine == 7900xtx ]]; then
    export VICTORY_MAP_THREADS=1 VICTORY_CARD_AWAKE=1
else
    export VICTORY_THIN_THREADS=12 VICTORY_UNDER_THREADS=8 VICTORY_LAND_THREADS=6
    if [[ $safe == 0 ]]; then export VICTORY_MAP_THREADS=auto VICTORY_PLACED_ORDER=1; fi
    export VICTORY_WAVE_WIDE_ONLY=6f225b641c5b0708,5dc3b25986622949,369cbe906ea72764,2cc4c197ee29e291
fi
case "$machine" in
    4070)
        export VICTORY_THIN_THREADS=8 VICTORY_UNDER_THREADS=6 VICTORY_LAND_THREADS=4
        export VICTORY_MAP_THREADS=1 VICTORY_PIPELINE_WORKERS=3
        if [[ $safe == 0 ]]; then
            export VICTORY_CARD_RESERVE_BUDGETS=1 VICTORY_CARD_BUFFERS_MIN_KB=64
            export VICTORY_CARD_BUFFERS_MB=1536 VICTORY_CARD_WINDOWS_MB=1024
        fi ;;
    4060)
        # RTX 4060 (8 GB) / Ryzen 7 7700X (8 cores, no V-Cache) / 32 GB, under Proton.
        # Worker counts of the 4070 profile (same core count), one fewer of the
        # recording workers to leave room for Wine's and vkd3d-proton's threads.
        export VICTORY_THIN_THREADS=7 VICTORY_UNDER_THREADS=6 VICTORY_LAND_THREADS=4
        export VICTORY_MAP_THREADS=1 VICTORY_PIPELINE_WORKERS=3
        if [[ $safe == 0 ]]; then
            # The card has a third of the 4070 profile's headroom: smaller buffer
            # and window budgets, still reserved ahead of textures.
            export VICTORY_CARD_RESERVE_BUDGETS=1 VICTORY_CARD_BUFFERS_MIN_KB=64
            export VICTORY_CARD_BUFFERS_MB=1024 VICTORY_CARD_WINDOWS_MB=512
        fi
        # 1080p out of the Series S profile (render 720p through its FidelityFX).
        export VICTORY_OUTPUT=1920x1080 ;;
esac
if [[ $safe == 1 ]]; then
    export VICTORY_UNDER_WIDE=0 VICTORY_LAND_ECHO=0
else
    export VICTORY_THIN=1 VICTORY_JOIN_PARTS=8 VICTORY_UPLOAD_AHEAD=1 VICTORY_INDIRECT_BATCH=2
    # As play.ps1 on NVIDIA: a constant-buffer fill that a small copy at its
    # place writes into is taken in list order instead of refusing the whole
    # submission to the card steps (there: submit thread 104.7 -> 50.9 ms a
    # frame), and a fill behind such a copy reads the copy's source on the card.
    export VICTORY_PLACED_ORDER=1 VICTORY_PLACED_FILL_REDIRECT=1
fi
# Without rays the bounced light is the ambient stand-in's, estimated from
# what is on screen: with its default (250 ms, changes of 3x at once) a
# room went dark the moment the lamps left the screen. Here: all of the
# estimate, a 4 s mean with no jumps, and the air (the haze) bounded by
# twice the light around. --raytracing builds the ray-tracing structures
# (the title's own bounced light and shadows; about a third slower).
if [[ $rt == 1 ]]; then
    export VICTORY_BVH_BUILDER=1
    # The ray-traced shadow passes left out (they trace every pixel; the
    # bounced light that lights rooms stays): --rt-shadows keeps them.
    [[ $rt_shadows == 1 ]] || export VICTORY_RT_SHADOWS=0
    # --rt-low: the bounced light's rays every second frame, the reflection
    # traces left out (an 8 GB card; docs/superpowers/plans/2026-11-27-rt-low.md).
    if [[ $rt_low == 1 ]]; then export VICTORY_RT_LOW=1; fi
else
    export VICTORY_AMBIENT=1.0 VICTORY_AMBIENT_SETTLE=4000,0 VICTORY_AMBIENT_AIR=2
    # The least bounced light anywhere: rooms kept some light with their lamps off screen.
    export VICTORY_AMBIENT_FLOOR="${AMBIENT_FLOOR:-0.35}"
fi
# The game's entries on a spinning disk (fast-copy.sh links them): the
# streaming reads at most a few of them at once (VICTORY_DSTORAGE_SLOW_MAX),
# so the reads from the SSD do not wait behind a seeking disk's.
slow=()
# Links at any depth: fast-copy.sh links folders inside data/ttdb too (the
# map texture tiles, read most of all, were missed one level down).
while IFS= read -r -d '' entry; do
    dev="$(findmnt -no SOURCE --target "$(readlink -f "$entry")" 2>/dev/null | sed 's/\[.*//')"
    [[ -n $dev && "$(lsblk -ndo ROTA "$dev" 2>/dev/null | tr -d ' ')" == 1 ]] && slow+=("${entry#"$game_dir"/}")
done < <(find "$game_dir" -type l -print0)
if ((${#slow[@]})); then export VICTORY_DSTORAGE_SLOW="$(IFS=,; echo "${slow[*]}")"; fi
# --dlss: NVIDIA DLSS upscales the Series S render (nvngx_dlss.dll in
# $DLSS_DIR; Proton's NVAPI on), with AMD FSR from $FFX_DIR standing by.
# No frame generation here: on an RTX 4060 with 30 GB its buffers on top of
# DLSS took the title past the PC's memory and the kernel killed it.
ffx_dir="${FFX_DIR:-$HOME/.local/share/FidelityFX-SDK/signedbin-2.4}"
if [[ -n $dlss ]]; then
    dlss_dir="${DLSS_DIR:-$home/dlss}"
    [[ -f "$dlss_dir/nvngx_dlss.dll" ]] || { echo "--dlss: no nvngx_dlss.dll in $dlss_dir (set DLSS_DIR)." >&2; exit 1; }
    export VICTORY_DLSS="$dlss" VICTORY_DLSS_DIR="$(winpath "$dlss_dir")" PROTON_ENABLE_NVAPI=1
    [[ -f "$ffx_dir/amd_fidelityfx_upscaler_dx12.dll" ]] && export VICTORY_FIDELITYFX_DIR="$(winpath "$ffx_dir")"
fi
[[ $windowed == 0 ]] && export VICTORY_FULLSCREEN=1
[[ $panel == 1 ]] && export VICTORY_PANEL=1
# Every run keeps the layer's log (a build for the windows subsystem has no
# stderr under Wine): $VICTORY_HOME/logs/<time>.log.
mkdir -p "$home/logs"
export VICTORY_LOG_FILE="$(winpath "$home/logs/$(date +%Y%m%d-%H%M%S)-$cmd.log")"
for kv in "${extra[@]}"; do export "$kv"; done

# Proton outside Steam; vkd3d-proton is Proton's D3D12.
proton_dir="$(dirname "$proton")"
prefix="${VICTORY_PREFIX:-$home/prefix}"
pfx="$prefix/pfx"
mkdir -p "$prefix"
export STEAM_COMPAT_DATA_PATH="$prefix"
# Wine finds its server beside its own binary; said outright for the tree one.
[[ -x "$proton_dir/files/bin/wineserver" ]] && export WINESERVER="$proton_dir/files/bin/wineserver"
export STEAM_COMPAT_CLIENT_INSTALL_PATH="${STEAM_COMPAT_CLIENT_INSTALL_PATH:-$HOME/.steam/steam}"
export PROTON_LOG="${PROTON_LOG:-0}"
# Larger shader cache for the thousands of translated pipelines; no frame cap.
export __GL_SHADER_DISK_CACHE_SIZE=10737418240 __GL_SYNC_TO_VBLANK=0
export VKD3D_CONFIG="${VKD3D_CONFIG:-}" WINEDEBUG="${WINEDEBUG:--all}"
# --breadcrumbs: vkd3d-proton marks the command lists it sends, and on a GPU
# fault (an NVIDIA Xid 31 in the kernel's log, then the title's "GPU Loc Sig"
# watchdog 30 s later) writes which command list and draw or dispatch was
# running to its log. Costs some speed: for finding a crash only.
if [[ $crumbs == 1 ]]; then
    export VKD3D_CONFIG="${VKD3D_CONFIG:+$VKD3D_CONFIG,}breadcrumbs"
    export VKD3D_DEBUG="${VKD3D_DEBUG:-err}" VKD3D_LOG_FILE="$(winpath "$home/logs/$(date +%Y%m%d-%H%M%S)-vkd3d.log")"
fi
# vkd3d-proton keeps its pipeline cache in the working directory otherwise (the repository).
export VKD3D_SHADER_CACHE_PATH="${VKD3D_SHADER_CACHE_PATH:-$(winpath "$home/vkd3d")}"
mkdir -p "$home/vkd3d"

# The layer compiles its own HLSL (the indirect argument clamp, the plane
# composer, in-place copies) with D3DCompile. Wine's built-in compiler
# refuses parts of it (RWByteAddressBuffer.InterlockedOr), which drops every
# indirect draw: Microsoft's own d3dcompiler_47 goes in the prefix instead.
if [[ ! -f "$prefix/.native-d3dcompiler" ]]; then
    if [[ ! -d "$pfx" ]]; then "$proton" run cmd /c exit >/dev/null 2>&1 || true; fi
    echo "Installing Microsoft's d3dcompiler_47 into $pfx (winetricks, once)."
    WINEPREFIX="$pfx" WINE="$proton_dir/files/bin/wine" WINESERVER="$proton_dir/files/bin/wineserver" \
        winetricks -q d3dcompiler_47 >/dev/null 2>&1 \
        && touch "$prefix/.native-d3dcompiler" \
        || echo "winetricks d3dcompiler_47 failed: indirect draws will be dropped." >&2
fi
export WINEDLLOVERRIDES="d3dcompiler_47=n,b${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"

echo "Settings for the $machine on $console ($cmd)."
mem_kb=$(awk '/MemTotal/ {print $2}' /proc/meminfo)
swap_kb=$(awk '/SwapTotal/ {print $2}' /proc/meminfo)
if (( mem_kb + swap_kb < 48 * 1024 * 1024 )); then
    echo "RAM + swap is $(( (mem_kb + swap_kb) / 1048576 )) GiB; the title holds 23-27 GB. Add swap if it is killed or stalls."
fi

cd "$repo"
case "$cmd" in
    run) exec "$proton" run "$victory" run "$(winpath "$exe")" --console "$console" --execute ;;
    shaders) exec "$proton" run "$victory" shaders ;;
    warm) exec "$proton" run "$victory" warm --used ;;
    *) echo "unknown --cmd $cmd" >&2; exit 2 ;;
esac
