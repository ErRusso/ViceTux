# Lighting without rays, close enough to the title's own

2026-11-27. Small change.

## Intent

The title lights rooms with ray tracing. With rays on, an RTX 4060 8 GB runs
it at 7-8 fps; the owner wants at least 15 fps, steady, with lighting that
looks right. Without rays the layer's stand-in (`crates/victory-gpu/src/ambient.rs`)
estimates the bounced light from what is on screen, at about 15-16 fps.

Goal: the stand-in close enough to the ray-traced light that the owner does
not notice much, at the frame rate of the run without rays.

Non-goals: making ray tracing itself cheaper; changing what `--raytracing`
does.

Anti-goal: a scene that looks wrong in a way rays never would (rooms gone
black, haze glowing, light popping when the camera turns).

## Approaches

1. **The stand-in, made closer (picked).** No rays. The resolve now reads the
   real depth plane (it read the stencil plane before the merge and was
   skipped after it). Compare with the ray-traced image and tune.
2. **Rays for the bounced light only**, reflections from upstream's
   reflection hold. Correct room light, cheaper than all of ray tracing, but
   by the timing run still too slow for 15 fps. The fallback if 1 is visibly
   off.
3. **Ray tracing every 2nd-4th frame.** Estimated 10-13 fps, short of the
   goal; light lags. Not taken.

The owner chose 1, with 2 if the difference is noticeable.

## Design

- Keep the depth fix: `ambient::DEPTH` tries the depth plane at table
  offset 688 first, then 464 (the stencil, refused since fa8fc1d).
- Same scene, twice: once with `--raytracing`, once without. Frames saved
  with `VICTORY_CAPTURE` at fixed counts after world entry
  (`VICTORY_CAPTURE_WORLD`, `VICTORY_CAPTURE_WORLD_ALSO`).
- Tune only the stand-in's existing switches from the comparison, in
  `scripts/play-linux.sh` without rays: `VICTORY_AMBIENT` (level),
  `VICTORY_AMBIENT_FLOOR` (0.35 was set against the stencil-as-depth
  estimate), `VICTORY_AMBIENT_SETTLE`, `VICTORY_AMBIENT_AIR`.
- The owner looks at the pairs and decides: close enough, or go to 2.

## Left to the builder

Which frames to compare, and how many tuning rounds before showing the owner
(proposal: two, each a short run).

## Result (2026-11-27, afternoon)

Option 1 does not get close. In the opening cutscene's shaded ground (the
lower part of the frame, median luma of the saved frames):

| Run | Ground | Sky |
|---|---|---|
| `--raytracing` | 66 | 134 |
| stand-in, floor 0.35 (before) | 20 | 138 |
| floor 1.0, shade 0.5 | 23 | 138 |
| floor 1.0, no shade | (same look) | |
| `VICTORY_AMBIENT=constant:3` | 24 | 144 |

The stand-in runs (its resolve role reports each time), yet even a strong
constant light barely moves that ground: what lights it in the ray-traced
image is not the buffer the stand-in writes (most likely the title's probes
or irradiance cache). Next: approach 2.

## Approach 2, chosen: rays for the bounced light, every second frame

Owner's choice: rays for the bounced light only, and the ray-traced light
updated every second frame. Goal unchanged: 15 fps steady, light the owner
does not notice as wrong. Expected 12-15 fps; to be measured.

- **Rays on** as `--raytracing` does now (`VICTORY_BVH_BUILDER=1`), shadow
  traces still out (`VICTORY_RT_SHADOWS=0`).
- **Reflection traces left out**, like the shadow traces (by program name in
  `pipeline/make.rs` `left_out`); reflections from upstream's hold
  (`VICTORY_AMBIENT_REFLECT`). The exact names come from the title's
  containers (logged once by name in a diagnostics run).
- **Every second frame** a new switch skips the bounced light's ray chain at
  dispatch: the per-frame clears of ray counts and bins, the traces
  (`CS_ScreenProbes_TraceRays_*`, the irradiance cache's), and the hit
  shading (`CS_RaytracingResolve_*`). Kept every frame: the structure
  builder (skipping its refits would leave moved objects in old places and
  meets the structures' overlap bug), the temporal resolves, upscales and
  final gathers, which then work from the last frame's rays.
- A launcher option `--rt-low` sets all of the above.
- Check: the same comparison as before (saved frames, ground/sky luma)
  against full `--raytracing`, the frame rate from the log, and no light
  flickering frame to frame (luma of consecutive saved frames).

Risk: if skipping the chain leaves something the next pass reads empty, the
light flickers dark every other frame; then the cut moves (fewer passes
skipped) or the rate idea is dropped and 2 runs every frame.

## Result of approach 2 (`--rt-low`, 2026-11-27)

RTX 4060 8 GB, Proton, world frames only (500+ draws), diagnostics build:

| Run | World fps median | p10 |
|---|---|---|
| `--raytracing` | 3.4 | 2.5 |
| `--rt-low` | 14.9 | 3.5 |
| no rays (stand-in) | 24.7 | 22.8 |

- No flicker: ten consecutive frames in the prison yard, odd-frame mean luma
  94.14 against even 94.08; the light rises smoothly (93.7 to 94.6).
- The yard at sunset is lit like the ray-traced runs: shade filled, no black
  ground. The low p10 is loading stalls, not the rays.
- Reflections: no glass or water in the frames looked at; the reflection
  traces stay left out until a scene with them shows otherwise.
