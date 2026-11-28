# Sparse windows: no memory for the empty parts of window copies

## Why

The xbox has 16 GB for everything; the layer holds about 28 GB. In gameplay
(RTX 4060, Proton, 2026-11-28) the copies of "windows outside buffers" hold
4.2 GB, and of the windows that could not be read in place, 3.35 GB in all,
1.9 GB lie over title memory that is only reserved, not committed: there is
nothing there, yet every window is a full-size host buffer (4 MiB on the
default grid) whose pages are pinned.

## Goal

A window whose title memory has uncommitted 64 KiB tiles takes memory only for
the tiles that hold something. Shaders see the same contiguous buffer at one GPU
address; the empty tiles read zeros (as the copy's zeros did) and drop writes.

Non-goals: view memory in place (sections opened as heaps: a later step),
windows on the card (`VICTORY_CARD_WINDOWS_MB`) and windows in BAR memory
(`VICTORY_WINDOWS_BAR_MB`), which keep their paths.

## Approaches

1. **Sparse copies (chosen).** The GPU side is a reserved buffer
   (`CreateReservedResource`); its tiles over committed title memory map to
   heaps opened on our own memory (`OpenExistingHeapFromAddress1`), the others
   to nothing. Measured first (spike, 2026-11-28, RTX 4060, vkd3d-proton):
   tiled resources tier 4; such mappings work; null tiles read zeros; CPU
   writes after the mapping are seen; a shader reads and writes the mapped
   tiles, and a write to a null tile is dropped without a fault.
2. **View memory in place** (sections opened as heaps): saves the 1.44 GB of
   view memory too, but changes how the title's physical memory is made; risk
   of crashes. Later.
3. **Commit the holes and open in place**: saves nothing (the zeros move to RAM).

## Design

- Switch `VICTORY_SPARSE_WINDOWS=1` (the launcher turns it on; off by default
  in the code until measured).
- At a window's making (`make_loose_backing`), with the switch on, no BAR or
  card side asked for, the title range is asked (`VirtualQuery`): a 64 KiB tile
  is *backed* if any of its pages is committed. A window with no unbacked tile
  is made as before.
- Otherwise: our CPU side is one `VirtualAlloc(MEM_RESERVE | MEM_COMMIT)` of the
  window's size (Linux backs its pages only when touched); each run of backed
  tiles gets a heap opened on our pages there (pinned: the memory used); a
  reserved buffer of the window's size gets those runs mapped, and every other
  tile mapped to nothing, on the host's queue at the making (before any list
  that reads it is submitted).
- The copy's refreshes write only backed tiles (`Shadow::copy` clips; the
  comparing paths write only what differs, and reading an untouched page
  costs nothing).
- When the title commits or maps memory over an unbacked tile
  (`title_memory_allocated`, `title_memory_remapped`), sparse windows there
  leave the table (as `in_place_let_go` does): the next ask makes a new one.
  Lists that hold the old one keep it until they are done. For commits we are
  not told of, a window's unbacked tiles are asked again when it is refreshed,
  at most once in 8 frames.
- Freed with the copy: the reserved buffer, then the heaps, then our pages.
- A line every 10 s (diagnostics): sparse windows alive, their title bytes and
  the bytes backed.

## Left to the builder

Names, the line's wording, where exactly the refresh re-check sits.
