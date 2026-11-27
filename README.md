# ViceTux

**Vice-Tay, made to run on Linux.** ViceTux runs the Xbox Series build of the
title on a Linux PC under Proton, through the
[VIctory](https://github.com/Marvisak/VIctory) compatibility layer: it loads
the console executable, answers its system calls and translates its GPU work
to Direct3D 12, which Proton's vkd3d-proton turns into Vulkan.

It is [TaynanGT/Vice-Tay](https://github.com/TaynanGT/Vice-Tay) (tuned for
Windows on an RTX 4090) with the fixes it takes to run under Wine and
vkd3d-proton, a launcher for Linux, and settings for a mid-range NVIDIA card.

> [!IMPORTANT]
> **No game files, code or assets are in this repository.** You need your own
> dump of the Xbox Series build. ViceTux is not affiliated with or endorsed by
> the game's developer or publisher.

## Where it stands

Measured on the PC it is tuned on:

| | |
|---|---|
| GPU | NVIDIA GeForce RTX 4060, 8 GB (driver 620.73) |
| CPU | AMD Ryzen 7 7700X, 8 cores / 16 threads |
| RAM | 30 GiB, 4 GiB zram swap |
| Storage | the hot archives on an NVMe SSD, the rest on a hard disk |
| Software | Linux (zen kernel), Proton Experimental 11.0, a patched wineserver |

| What | Result |
|---|---|
| Menus, videos | 30 fps (the title's own cap) |
| The prison, the heavy rooms (3,000-5,000 draws) | **15-16 fps** median |
| Lighter scenes | 19-24 fps |
| With ray tracing on | 7-8 fps: the right light, not playable on this card |
| Loading to the world | about 2 minutes; the first run longer (shaders, pipelines) |

It plays, but it is not smooth yet. See [Known problems](#known-problems).

## What ViceTux fixes for Linux

Each of these was found and measured in a session on the PC above.

| Problem under Proton | Fix |
|---|---|
| **The HUD, menus and subtitles were missing.** Wine's shader compiler refuses the `[RootSignature]` attribute, so the plane the interface is drawn on was never composed over the scene. | The composer's root signature is built in code ([`compose.rs`](crates/victory-gpu/src/compose.rs)). |
| **About 3,500 draws a frame were dropped** (missing geometry). Wine's compiler refuses `RWByteAddressBuffer.InterlockedOr` in the indirect-argument clamp. | The launcher installs Microsoft's `d3dcompiler_47` into its own prefix and forces it. |
| **2-4 fps forever, about 1,000 pipelines "being made".** vkd3d-proton's DXIL-to-SPIR-V translator recursed without end on seven compute programs (loops with two back edges); the pipeline workers died and their queue never drained. | Every loop gets a single latch before the DXIL is written (from [kroeeee-eeee's Linux fork](https://github.com/kroeeee-eeee/VIctory-Linux-Proton-Wine-7900XT-XTX)); a watchdog replaces a dead worker and leaves its pipeline out from then on (`lost-pipelines.txt` in the cache). Six of the seven now build. |
| **Water volumetrics never drawn** on 32-lane cards. `VICTORY_WAVE_WIDE_ONLY` had turned nothing on since upstream made it a filter of `VICTORY_WAVE_WIDE=1`. | The list alone turns the wave-wide form on for the programs it names (upstream came to the same fix; its `wave_wide_routes` is the one used now). |
| **Brown and dark blotches in lit haze.** The ray-traced shadow passes ran with no acceleration structure built. | Left out under Wine unless ray tracing is on (`VICTORY_RT_SHADOWS`). |
| **Rooms went black** when their lamps left the screen (the bounced-light stand-in only sees what is on screen). | A floor under the estimate (`VICTORY_AMBIENT_FLOOR`), a 4 s mean instead of 250 ms, and a bound on the haze. |
| **25-90 ms a frame in the write watch.** The stock wineserver walks all of the title's tens of thousands of mapped views on every map and unmap. | A wineserver with a view tree ([`linux/wineserver`](linux/wineserver)): 11.1 → 15.8 fps median. |
| **75,000-216,000 `VirtualQuery` a second** under Wine's process-wide lock. | Uncommitted regions are remembered ([`watch.rs`](crates/victory-gpu/src/watch.rs), from kroeeee-eeee's fork). |
| **Textures waited a mean 23 s** for their data with the dump on a hard disk. | [`scripts/fast-copy.sh`](scripts/fast-copy.sh) puts the archives the title streams most on an SSD: 3 s. |

The commits say more about each: `git log upstream/main..`.

## Getting started

### What you need

- Linux with Steam and **Proton - Experimental** installed (Library → Tools).
- A GPU with Vulkan 1.3 and mesh shaders: tested on NVIDIA (proprietary
  driver); AMD on Mesa should work (kroeeee-eeee's fork runs there).
- **RAM: 32 GB with 8 GB or more of swap.** The title holds 23-27 GB.
- **An SSD** for at least the hot archives (about 110 GB).
- Rust (the toolchain is pinned), [`cargo-xwin`](https://github.com/rust-cross/cargo-xwin),
  `clang`/`lld`, `winetricks`, and for the wineserver `gcc`, `make`, `flex`,
  `bison`, `autoconf`, `perl`.
- Your own dump of the title.

### 1. Build

```sh
git clone https://github.com/ErRusso/ViceTux
cd ViceTux
cargo install cargo-xwin
cargo xwin build --locked --profile diag -p victory --features diagnostics --target x86_64-pc-windows-msvc
```

The first build downloads the Windows SDK headers and libraries (accept the
licence prompt).

### 2. Put the game where it streams fast

If your dump is on a hard disk, copy the archives the title streams most to an
SSD and link the rest:

```sh
scripts/fast-copy.sh /path/to/your/dump ~/Games/ViceTux-game
echo ~/Games/ViceTux-game > ~/.local/share/vice-tay/game-dir
```

On btrfs the folder becomes a subvolume of its own, so snapshots of your home
(snapper, timeshift) do not pin 100 GB of game data. If the whole dump is on
an SSD already, just put its path in `~/.local/share/vice-tay/game-dir` (or
set `VICTORY_GAME_DIR`).

### 3. Build the faster wineserver (recommended)

```sh
linux/wineserver/build.sh
```

It fetches the Wine source of your installed Proton Experimental build,
applies the view-tree patch, builds only the server (about a minute) and makes
a 7 MB Proton of links next to it. Your Steam Proton is not touched. Run it
again when Proton Experimental updates; until then the launcher says so and
uses the stock server.

### 4. Translate the shaders (once per build of the translator)

```sh
scripts/play-linux.sh --cmd shaders
```

### 5. Play

```sh
scripts/play-linux.sh --windowed
```

The first start installs `d3dcompiler_47` into its own Proton prefix
(`~/.local/share/vice-tay/prefix`). The first minutes in the world stutter
while pipelines are made; they are kept for the next runs.

| Option | What it does |
|---|---|
| (none) | Full screen, Series S profile, settings for your card, performance panel on |
| `--windowed` | In a window |
| `--no-panel` | Without the performance panel (**F3** hides it while playing) |
| `--raytracing` | The title's own ray-traced light: correct indoor lighting, about half the frame rate |
| `--rt-shadows` | With `--raytracing`: the ray-traced shadows too (slower still) |
| `--rt-low` | The title's ray-traced bounced light, updated every second frame, reflections without rays: for 8 GB cards |
| `--dlss quality\|balanced\|performance` | NVIDIA DLSS upscales the Series S render (`nvngx_dlss.dll` in `$DLSS_DIR`, default `~/.local/share/vice-tay/dlss`) |
| `--machine 4060\|4070\|4090\|7900xtx` | Force a profile (default: picked from your card) |
| `--console series-x` | Run as a Series X (heavier) |
| `--safe` | Leave out the settings still on trial |
| `--breadcrumbs` | vkd3d-proton's breadcrumbs and log, for chasing a GPU fault |
| `--cmd shaders\|warm` | Translate shaders / make pipelines ahead |
| `VICTORY_...=value` | Any layer switch, after the options |

Environment: `VICTORY_GAME_DIR` (your dump), `VICTORY_HOME` (cache, saves,
prefix, logs; default `~/.local/share/vice-tay`), `PROTON` (another Proton),
`AMBIENT_FLOOR` (the bounced-light floor without rays, default `0.35`).

Every run writes the layer's log to `~/.local/share/vice-tay/logs/`.

## Known problems

- **A GPU crash a few minutes into play (worked around).** The card faulted
  reading an address of the title's space through its constant cache
  (NVIDIA `Xid 31`, the same address every run) and the title's watchdog
  ended it ("GPU Loc Sig"). Bisected: it comes from card steps at stage 2
  (the title's buffer copies done on the card behind submissions in the
  air). `play-linux.sh` now runs stage 1 (`CARD_STEPS=2` brings stage 2
  back): no crash in two 9-minute runs, at 13 fps in gameplay instead of
  16-17. The fault in stage 2 itself is not found yet.
- **Xwayland grew by about 2 GB a run** in one afternoon's sessions (about
  120 KB a frame, never given back when the game exited) until the title was
  killed for memory; later the same day two 8-minute runs and two menu runs
  left it flat, with and without `VICTORY_PRESENT_INTERVAL=1` (presents
  with vsync). Not from the layer's own window work. If `Xwayland` grows in
  `top`, restart it (or log out) between sessions, and try a run with
  `VICTORY_PRESENT_INTERVAL=1` to see whether it stops.
- **Frame rate.** 15-16 fps in heavy rooms: about 40 ms a frame preparing the
  GPU's work on the CPU, 15-25 ms copying data back, and the 4060 near full.
- **Lighting without ray tracing** is an estimate: rooms are lit, but flatter
  than the title's own light. With `--raytracing` it is right, at 7-8 fps on
  an RTX 4060 (the title traces its rays in compute shaders that the layer
  translates; there is no hardware ray tracing to lean on).
- **White patches on glossy floors and tables**: the ray-traced reflection
  resolve is left out without ray tracing, and those surfaces show stale data.
- **One compute program still kills vkd3d-proton's compiler**
  (`InstanceCulling:CS_Prop_BatchNoCull`): left out, so some props can be
  missing.
- **VRAM.** An 8 GB card is full: about 7 GB for the title's process (the
  panel counts only the layer's share), the rest for the desktop. Close the
  browser while playing.
- **The render resolution is fixed** at 1920×1080 output on the Series S
  profile: the title's FSR4 build refuses others and crashes.

## Troubleshooting

| | |
|---|---|
| The HUD is missing, or the world is mostly empty | `d3dcompiler_47` is not the native one: delete `~/.local/share/vice-tay/prefix/.native-d3dcompiler` and start again |
| "Proton Experimental was updated" at start | Run `linux/wineserver/build.sh` again |
| Stuck at a few fps with pipelines "being made" | A translator change: run `scripts/play-linux.sh --cmd shaders` |
| The game closes during a cutscene | `journalctl -k \| grep Xid`: a GPU fault (above) |
| Textures and models arrive late | The dump is on a hard disk: `scripts/fast-copy.sh` |
| Your disk fills up | Snapshots of your home holding deleted files; keep the game in its own subvolume |

## How this tree is built

ViceTux carries the whole history of the trees it comes from. Upstream work
from [TaynanGT/Vice-Tay](https://github.com/TaynanGT/Vice-Tay) is merged as
it lands, and the Linux changes are commits on top. The upstream README, with
its Windows instructions and measurements, is
[docs/README-vice-tay.md](docs/README-vice-tay.md); how the layer works:
[docs/architecture.md](docs/architecture.md).

## Credits

This is other people's work with Linux fixes on top:

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its
  contributors: the loader, the runtime services, the Direct3D 12 back end and
  the shader recompiler.
- **[TaynanGT/Vice-Tay](https://github.com/TaynanGT/Vice-Tay)** and everyone
  it credits in [docs/credits.md](docs/credits.md) and
  [docs/README-vice-tay.md](docs/README-vice-tay.md#credits).
- **[kroeeee-eeee/VIctory-Linux-Proton-Wine-7900XT-XTX](https://github.com/kroeeee-eeee/VIctory-Linux-Proton-Wine-7900XT-XTX)**
  (with datingesp-hash): the single-latch translation, remembered
  uncommitted regions, and the wineserver view-tree patch.
- [Proton](https://github.com/ValveSoftware/Proton),
  [Wine](https://www.winehq.org/) and
  [vkd3d-proton](https://github.com/HansKristian-Work/vkd3d-proton).

## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file it does not
cover is named in [docs/asset-policy.md](docs/asset-policy.md). The wineserver
patch in `linux/wineserver/` applies to Wine (LGPL-2.1-or-later).
