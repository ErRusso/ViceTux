#!/usr/bin/env bash
# Builds a wineserver that keeps mapped views in a tree, for the installed
# Proton Experimental, and puts a Proton next to it that uses it.
#
#   linux/wineserver/build.sh [work folder, ~2 GB; default ~/.cache/vicetux-wine]
#
# Why: the stock wineserver walks all of a process's mapped views on every
# map and unmap. The title has tens of thousands of them, and the layer's
# write watch took 25-90 ms a frame (RTX 4060, Ryzen 7 7700X): 11.1 fps
# median in the prison. With the tree, 3-6 ms and 15.8 fps. The patch is
# kroeeee-eeee's (VIctory-Linux-Proton-Wine-7900XT-XTX, linux/wineserver),
# against Wine's server/mapping.c.
#
# What it makes: $VICTORY_HOME/proton-experimental-viewtree (default
# ~/.local/share/vice-tay/...), a Proton of links to the installed one with
# its own copies of the server, the loader and ntdll.so (Wine looks for its
# server beside the real path of its loader, so links alone are not enough).
# About 7 MB; the installed Proton is not touched. scripts/play-linux.sh uses
# it while the installed Proton is still the build it was made for, and says
# so when it is not: run this again after Proton Experimental updates.
#
# Needs: git, gcc, make, flex, bison, autoconf, perl, curl; no root.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="${1:-$HOME/.cache/vicetux-wine}"
proton_dir="${PROTON_DIR:-$HOME/.steam/steam/steamapps/common/Proton - Experimental}"
home="${VICTORY_HOME:-$HOME/.local/share/vice-tay}"
out="$home/proton-experimental-viewtree"

for tool in git gcc make flex bison autoreconf perl curl; do
    command -v "$tool" >/dev/null || { echo "Missing: $tool" >&2; exit 1; }
done
[[ -f "$proton_dir/version" ]] || { echo "No Proton at $proton_dir (set PROTON_DIR=)." >&2; exit 1; }

# The installed build, e.g. "1794497090 experimental-11.0-20261112-x86_64".
tag="$(awk '{print $2}' "$proton_dir/version")"
tag="${tag%-x86_64}"
echo "Proton build: $tag"

# Its Wine commit: the wine submodule of ValveSoftware/Proton at that tag.
wine_commit="$(curl -fsSL "https://api.github.com/repos/ValveSoftware/Proton/contents/wine?ref=$tag" \
    | sed -n 's/.*"sha": *"\([0-9a-f]\{40\}\)".*/\1/p' | head -1)"
[[ -n $wine_commit ]] || { echo "Could not find the Wine commit of $tag on GitHub." >&2; exit 1; }
echo "Wine commit: $wine_commit"

mkdir -p "$work"
src="$work/wine-$wine_commit"
if [[ ! -f "$src/.prepared" ]]; then
    rm -rf "$src"
    git init -q "$src"
    git -C "$src" fetch -q --depth 1 https://github.com/ValveSoftware/wine "$wine_commit"
    git -C "$src" checkout -q FETCH_HEAD
    patch -d "$src" -p1 -s < "$here/wineserver-view-tree.diff"
    # What Proton's own build generates and the tree does not ship.
    (cd "$src" && perl tools/make_specfiles >/dev/null && perl tools/make_requests >/dev/null && autoreconf -f)
    # Placeholders for generated Vulkan files: configure scans every
    # Makefile's sources, the server does not use them.
    for f in include/wine/vulkan.h dlls/winevulkan/loader_thunks.c dlls/winevulkan/vulkan_thunks.c \
             dlls/winevulkan/vulkan_thunks.h dlls/winevulkan/loader_thunks.h; do
        [[ -e "$src/$f" ]] || echo "/* placeholder: generated in Proton's build */" > "$src/$f"
    done
    [[ -e "$src/dlls/winevulkan/winevulkan.json" ]] || echo "{}" > "$src/dlls/winevulkan/winevulkan.json"
    touch "$src/.prepared"
fi

build="$work/build-$wine_commit"
mkdir -p "$build"
if [[ ! -x "$build/server/wineserver" ]]; then
    without=""
    for w in x freetype gstreamer vulkan wayland alsa pulse cups sane gphoto krb5 netapi pcap usb v4l2 sdl \
             capi dbus ffmpeg gnutls opencl osmesa pcsclite udev unwind; do
        without+=" --without-$w"
    done
    # shellcheck disable=SC2086
    (cd "$build" && "$src/configure" --enable-win64 --without-mingw --disable-tests $without >configure.log 2>&1) \
        || { echo "configure failed: $build/configure.log" >&2; exit 1; }
    make -C "$build" -j"$(nproc)" server/wineserver >"$build/make.log" 2>&1 \
        || { echo "build failed: $build/make.log" >&2; exit 1; }
fi

# The Proton of links.
rm -rf "$out"
mkdir -p "$out/files/bin" "$out/files/lib/wine"
for f in "$proton_dir"/*; do
    b="$(basename "$f")"
    case "$b" in
        files) ;;
        proton) cp -a "$f" "$out/proton" ;;
        *) ln -s "$f" "$out/$b" ;;
    esac
done
for f in "$proton_dir"/files/*; do
    b="$(basename "$f")"
    case "$b" in bin|lib) ;; *) ln -s "$f" "$out/files/$b" ;; esac
done
for f in "$proton_dir"/files/lib/*; do
    b="$(basename "$f")"
    [[ $b == wine ]] || ln -s "$f" "$out/files/lib/$b"
done
for f in "$proton_dir"/files/lib/wine/*; do
    b="$(basename "$f")"
    case "$b" in
        i386-unix|x86_64-unix)
            mkdir -p "$out/files/lib/wine/$b"
            for g in "$f"/*; do
                n="$(basename "$g")"
                case "$n" in
                    wine|wine64|wine-preloader|wine64-preloader|ntdll.so) cp -a "$g" "$out/files/lib/wine/$b/" ;;
                    *) ln -s "$g" "$out/files/lib/wine/$b/$n" ;;
                esac
            done ;;
        *) ln -s "$f" "$out/files/lib/wine/$b" ;;
    esac
done
cp -a "$proton_dir/files/bin/wine" "$out/files/bin/"
ln -s wine "$out/files/bin/msidb"
cp "$build/server/wineserver" "$out/files/bin/wineserver"
cp "$proton_dir/version" "$out/built-against-version"

# The server must answer this Proton's Wine (same protocol version).
test_prefix="$work/test-prefix"
rm -rf "$test_prefix"
if WINEPREFIX="$test_prefix" WINESERVER="$out/files/bin/wineserver" WINEDEBUG=-all \
    timeout 300 "$out/files/bin/wine" cmd /c echo viewtree-ok 2>/dev/null | grep -q viewtree-ok; then
    echo "Built and checked: $out"
else
    echo "The new server did not answer Proton's Wine; not used (removed)." >&2
    rm -rf "$out"
    exit 1
fi
WINEPREFIX="$test_prefix" "$out/files/bin/wineserver" -k 2>/dev/null || true
rm -rf "$test_prefix"
