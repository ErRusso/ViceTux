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
