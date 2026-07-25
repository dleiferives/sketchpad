# Implementation Status

Status: live implementation ledger, 2026-07-24. This note records what the
current executable actually does. Product intent remains in
[first-usable-product.md](first-usable-product.md); research claims and future
possibilities belong in the subject notes.

## Current Executable

Run on Atlas:

```text
scripts/atlas run cargo run --release --bin sketchpad
```

Run the same build on the constrained Apollo target:

```text
scripts/apollo run cargo run --release --bin sketchpad
```

The application now uses the sparse raster path rather than the original SDF
prototype:

- 4096×4096 defined canvas;
- 128×128 sparse canonical CPU tiles;
- linear premultiplied `f32` RGBA reference pixels;
- hard round source-over brush;
- distance-based deterministic dab resampling;
- native Atlas/XInput2 pen and eraser device discovery;
- normalized pressure, tilt, physical source ID, tool type, and source
  timestamp delivery;
- mouse fallback at pressure 1;
- pressure-sized pen ink and destination-out erasing;
- GPU brush-footprint cursor that distinguishes pen and eraser and follows
  contact pressure;
- between-stroke keyboard controls for brush size, opacity, mouse
  pen/eraser mode, and six built-in colors;
- one persistent tile transaction from pointer contact to release;
- one before-image per touched tile per gesture;
- exact canvas and per-tile damage rectangles;
- additive nonempty-bound maintenance without commit-time tile rescans;
- subtractive nonempty-bound maintenance with boundary-only shrinking;
- incremental GPU uploads for resident dirty subrectangles;
- lazily paged `Rgba32Float` GPU tile cache, with 256 array layers per page
  and a 1,024-tile ceiling for the current 32×32-tile canvas;
- nearest-sampled instanced visible-tile rendering;
- visible paper boundary and dark pasteboard;
- pan and cursor-centered zoom;
- undo and redo;
- versioned/checksummed sparse-raster recovery checkpoints with atomic
  temporary-file replacement and directory synchronization;
- automatic recovery on startup and autosave two seconds after the last
  committed edit;
- cancellation on focus loss or Escape during an active stroke;
- one-second live mean/p95/max CPU input-handler and frame-submit timing,
  upload traffic, page/capacity/residency, deferred-visible, eviction, and CPU
  tile counters;
- event-loop sleep while no redraw is requested.

Controls:

- left drag: draw;
- Wacom pen contact: pressure-sensitive draw;
- Wacom eraser contact: pressure-sensitive coverage erase;
- middle drag: pan;
- wheel: zoom at cursor;
- `[` / `]`: decrease/increase the hovered tool size;
- Shift-`[` / Shift-`]`: decrease/increase the hovered tool opacity;
- E: toggle mouse pen/eraser mode;
- 1–6: select a built-in pen color;
- Control/Command-Z: undo;
- Control/Command-Shift-Z or Control/Command-Y: redo;
- Control/Command-S: force the recovery checkpoint;
- Control/Command-O: reload the last recovery checkpoint;
- Escape while drawing: cancel the active stroke;
- Escape while idle: exit.

## Ownership Path

```text
window samples
    ↓
HardRoundStroke resampler
    ↓
active RasterLayer gesture
    ├── one before-image on first tile write
    ├── canonical contiguous CPU tile mutation
    └── incremental per-tile damage
                    ↓
           resident dirty-subrect upload
                    ↓
          bounded GPU texture-array cache
                    ↓
             instanced tile draw
```

The CPU raster layer remains canonical. The GPU cache can be reconstructed
from it. The old SDF modules remain in the repository as historical prototype
code but no longer drive the application window.

Native tablet collection is described in
[tablet-input.md](tablet-input.md). It is a platform adapter feeding the same
brush transaction path as mouse input; XInput2 types do not enter the brush,
raster, document, or GPU layers.

## Automated Coverage

### Unit tests

The current test suite covers:

- sparse allocation and absent-tile transparency;
- clipped edge tiles;
- one snapshot per tile per gesture;
- persistent gestures across multiple update calls;
- incremental damage draining;
- exact per-tile damage union;
- cancellation and dropped-guard rollback;
- undo/redo state swapping;
- empty-tile reclamation;
- hard-brush parameter validation;
- immutable brush size/opacity/color adjustment with relative-spacing
  preservation;
- pressure-dependent footprint;
- input range and signed-tilt normalization;
- XInput fixed-point and timestamp-wrap conversion;
- sparse valuator-state merging;
- physical pen/eraser classification;
- duplicate XInput tip-packet rejection;
- versioned input-trace parsing, validation, atomic replacement, and stable
  content hashing;
- deterministic seeded replay transforms and pixel-identical repeated replay;
- deterministic checkpoint encoding and exact sparse-raster round trips;
- checksum, truncation, trailing-data, and atomic replacement rejection/tests;
- destination-out erasing and undo restoration;
- no tile allocation when erasing empty space;
- final stroke caps;
- one undo entry across many input updates;
- independence from collinear event batching;
- camera mapping and view bounds.

### CPU brush replay

`brush_bench` replays the real hard-round paint/erase and transaction paths over
empty and painted content:

```text
cargo run --release --bin brush_bench -- --runs 12
```

The current provisional Atlas result is recorded in
[performance-laboratory.md](performance-laboratory.md).

### Recorded tablet replay

`traces/canonical-wacom-v1.json` is a real pen-down-through-pen-up gesture from
Atlas's Wacom Intuos Pro S. It contains 68 samples over 385.124 ms of
application-arrival time, retains X11 source time, position, pressure, tilt,
distance, phase, viewport, and device identity, and has content hash
`46da823fd749864d`.

Record a replacement trace:

```text
scripts/atlas record traces/canonical-wacom-v1.json
```

Run and automatically fetch a release CPU replay with its machine context:

```text
scripts/atlas benchmark cpu
scripts/apollo benchmark cpu
```

`trace_replay` uses that trace for deterministic unpaced, 1×, 2×, and 4×
playback over empty, sparse, dense, and 1,000-seed-stroke scenes. It also runs
1,000 deterministic transformed target strokes per scene by default. Every
scheduled variant must produce the same exact `f32` raster checksum; every
undo must restore the scene checksum. The versioned JSON Lines record retains
raw event time and lateness arrays, percentiles, damage, allocation, snapshot,
pixel-visit, tile, and byte counters. Optional PPM references make mismatches
inspectable rather than reducing correctness to timing.

The CPU profiling companion prepares one selected scene, verifies the same
canonical stroke at every input boundary under 1/2/4/8/all-sample drain
batches, warms deterministic transactions, and then repeats precomputed
paint-plus-undo transactions with checksums and JSON outside the timed region:

```text
scripts/apollo benchmark profile --scene dense --hot-strokes 10000
```

It emits `profile-ready` with its process ID before the hot interval so Linux
hardware counters can attach after scene/oracle setup. The final checksum must
exactly match the prepared scene. This workload is for profiling the
transaction region; it does not replace the latency distributions from
`trace_replay`.

Pass `--shadow-strokes N` to run an untimed undo-granularity experiment before
the hot loop. It captures the painted damage, undoes the gesture, compares
exact before/after pixels, and reports unique 8×8, 16×16, and 32×32 blocks,
payload bytes, per-tile bitmap bytes, and reduction relative to current
whole-tile snapshots. `--brush-mode paint|erase` and `--brush-diameter PX`
make large-footprint and eraser cases explicit in the raw result rather than
changing hidden benchmark state. This is exact for the current monotonic
hard-round paint/erase kernels. A future arbitrary kernel that can change a
pixel and later restore it within one gesture must mark first writes directly
rather than relying on final pixel differences.

`--undo-storage blocks16` selects the real block-history prototype in the
profiler. It captures each conservative 16×16 region once per existing tile
and gesture into a flat per-tile payload, stores tile bounds metadata
separately, and swaps block rows for undo/redo. Newly allocated tiles use the
whole-state absence marker because they have no before-pixels. Existing,
newly allocated, reclaimed, cancelled, undone, and redone tiles pass the same
intermediate oracle. `whole` remained the application and profiler default
while those comparisons were collected.

The first `hybrid16` implementation was removed after measurement. Its
first-edit threshold never selected whole storage for the recorded 192 px
stroke and slowed the block path. Its replacement is `adaptive16`, which
resolves storage once per brush gesture from information the brush owns:
diameters through the 128 px tile width use blocks and larger brushes use
whole tiles. General raster gestures without a brush hint conservatively use
whole storage. `RasterLayer::new` and the application now default to this
policy; profiler modes `whole` and `blocks16` remain concrete controls.

Profiler result format version 2 paints bursts of 64 strokes before restoring
the initial raster. `--transaction-batch-strokes N` changes the burst, with
`1` retained as the adversarial one-paint/one-undo control. Paint, explicit
undo, history destruction, and harness overhead are separate result fields.

The offscreen GPU companion is selected explicitly by adapter:

```text
scripts/atlas benchmark gpu intel
scripts/atlas benchmark gpu nvidia
scripts/apollo benchmark gpu intel
```

It submits one frame per recorded sample and reports separate raw distributions
for brush processing, damage/upload synchronization, visible-scene
preparation, encoding/submission, total app-to-submit CPU time, schedule
lateness, and hardware-timestamped GPU render-pass time. It also records exact
upload bytes/counts, residency, pages, evictions, visible/deferred instances,
adapter/driver identity, and output checksum. GPU render-pass timestamps do not
include `queue.write_texture` upload execution or display presentation; those
remain separate measurement boundaries.

The live presentation path now defers resident-tile damage uploads until frame
preparation and unions repeated damage to the same tile. This preserves every
document sample while avoiding redundant `Queue::write_texture` calls when
winit collapses several redraw requests into one frame. Reclaimed tiles remove
their pending work; newly visible nonresident tiles still receive one current
full-tile upload.

Presentation stats now distinguish incoming and coalesced damage regions,
full and partial uploads, logical dirty bytes, the address span of strided
source rows, a tightly packed 256-byte-aligned staging-byte estimate, and CPU
time spent inside `write_texture`. GPU replay result format version 2 exports
the same counters. The address span and API time are not GPU execution time.

GPU replay result format version 4 also supports `--display-hz 60,120`.
Display-paced rows drain all samples ready for a display deadline, preserve
their semantic order, and submit one frame. Input and damage timing remain
per sample; scene preparation, encoding, submission, and render-pass
timestamps are per displayed frame. `sample_ready_wait` reports deliberate
late-latching delay, while `schedule_lateness` reports missing the display
deadline itself. The final raster checksum must match per-sample scheduling.

Revision `e8f56c4` replaces unconditional one-rectangle-per-tile union with a
fixed-capacity cost-aware rectangle set. Four rectangles are stored inline;
pair merges compare their additional padded transfer bytes with a configurable
call-equivalent threshold. The fifth rectangle forces the cheapest pair merge.
The original `write_texture` path selected `rect4` with 64 KiB from clean
three-repeat Apollo Intel runs. Result format version 5 adds
`damage_coalescing`, `damage_merge_cost_bytes`, forced-merge, merge-byte, and
pending-region fields. Replay controls are `--damage-coalescing union|rect4`
and `--damage-merge-cost-kib N`.

Revisions `61b2cd4` and `4335258` add and refine a reusable three-slot mapped
staging ring. Frame damage is packed into aligned compact rows, unmapped,
encoded as buffer-to-texture copies, submitted before rendering, and
immediately requested for asynchronous remapping. Reuse polls first and blocks
only if a slot is genuinely unfinished. Frames above 8 MiB fall back to
`write_texture`, preventing cold residency from growing each ring slot to the
entire visible working set. Three slots can retain at most 24 MiB.

Result format version 6 adds `upload_mode`, CPU pack/encode/wait counters,
staging allocation/capacity/fallback counters, encoder-timestamp capability,
and GPU copy-batch distributions. `--texture-upload
write-texture|staging-ring` selects the path.

Revision `1f85a50` promotes `staging-ring`, `rect4`, and a 0 KiB merge
threshold to the current application and replay defaults. `write-texture`
remains selectable and defaults to its separately measured 64 KiB threshold.
This is an Apollo Intel decision and must be requalified on mobile hardware,
other formats, and different damage geometry.

Revision `609a92e` persists visible tile coordinates, the protected residency
set, and per-page instance buffers. `RasterLayer::allocation_generation`
changes when the sparse tile set changes but not for ordinary existing-tile
pixel edits. Visibility is invalidated by camera bounds or allocation
generation; instance data is additionally invalidated by residency slot
generation. Replay format version 7 records rebuilds, cache hits, scanned and
sorted tiles, instance bytes, and cached-visible count.

`--visibility cached|rebuild` preserves the same-revision rebuild control.
`--view-zoom Z` selects centered zoom cases for instance/texture isolation.
On stable dense Apollo frames, caching eliminates all visibility scans, sorts,
and instance writes and reduces application-to-submit p95 by 14%–21%.
Allocation-heavy strokes correctly rebuild rather than using stale state.

The convenience commands write ignored artifacts beneath `.artifacts/results`
and fetch both the JSON Lines result and a text snapshot of host, load, CPU,
frequency policy, memory/swap, sensors, Vulkan, Rust, and NVIDIA state where
available.

### GPU smoke replay

`gpu_smoke` runs without a window:

```text
cargo run --release --bin gpu_smoke
```

It:

1. paints a deterministic pressure-varying stroke into sparse CPU tiles;
2. uploads visible tiles through deliberately tiny two-layer GPU pages so the
   replay exercises multi-page binding and drawing;
3. paints a second dot into an already-resident tile;
4. verifies that update uses a dirty subrectangle smaller than a full tile;
5. draws the paper, tile instances, and a visible eraser cursor into an
   offscreen texture;
6. copies the image back to the CPU;
7. verifies that every resident tile is drawn with no deferred visible tiles;
8. fails if enough dark ink and cursor-colored pixels are not present.

On the Atlas Intel UHD 630 it currently finds 15,178 dark ink/outline pixels
and 312 cursor-colored pixels. The resident update transfers 23,104 bytes
rather than the 262,144 bytes required for a complete `f32` RGBA tile. Its
eight visible tiles span four forced two-layer pages.

## Known Limitations

This is an architectural integration checkpoint, not yet the usable painter:

- native tablet input currently supports Atlas/X11 only;
- the committed trace covers one pen gesture on one Wacom model; eraser,
  very light pressure, fast motion, long strokes, and multiple drawing styles
  still need separate physical traces;
- no explicit proximity, twist, side-button, pad, coalesced-history, or
  prediction support;
- tilt and source timestamps are preserved but not yet consumed by the round
  brush;
- no smoothing beyond constant-distance resampling;
- only one layer, one hard-round brush family, six preset colors, and one
  eraser mode;
- no graphical brush/color UI, rotation, selection, or transforms;
- the recovery checkpoint is a single-canvas interim raster snapshot, not the
  future native semantic document; there is no Save As, file dialog, export,
  embedded preview, or migration beyond strict version rejection;
- checkpoint encoding and disk I/O are synchronous after the idle delay and
  still need large-document timing and disk-full/kill testing;
- no color management beyond the stated linear working assumption;
- `f32` RGBA costs 16 bytes per pixel and is explicitly a reference format;
- each lazily allocated 256-tile GPU page reserves 64 MiB in the reference
  format, up to 256 MiB for all 1,024 tiles in the current canvas;
- a future document with more than 1,024 simultaneously visible allocated
  tiles would defer the farther candidates, although that count is now logged;
- nearest tile sampling has no mipmaps, gutters, or zoom-out filtering;
- hard-round brush undo is adaptive only at the coarse measured 128 px
  diameter boundary; more brush families and devices require their own
  footprint matrix before generalizing the policy;
- arbitrary general edits still use full-tile content-bound rescans;
- brush work is CPU-only;
- repeated dab/tile intersections are not yet coalesced;
- offscreen GPU render passes and explicit staged upload copies have
  timestamps, but `write_texture` transfer execution, display presentation,
  and input-to-photon latency are not yet instrumented; current live-window
  timings still end at CPU queue submission.

## Immediate Engineering Order

The measured bottlenecks, exact-quality gates, and experiment definitions are
now governed by
[performance-proof-plan.md](performance-proof-plan.md). The immediate order is:

1. Add exact intermediate replay checkpoints, batch-schedule invariance, and a
   region-dominated CPU profiling workload.
2. Measure 8×8, 16×16, and 32×32 first-write undo shadow blocks, then implement
   the best exact candidate.
3. Add frame-paced replay that processes every sample but coalesces GPU work to
   one submission per display opportunity.
4. Compare per-damage `write_texture`, per-frame dirty coalescing, and reusable
   staging-ring transfers with copy timing and exact GPU readback.
5. Compare a stable lower-bandwidth display cache and presentation formats
   after the visibility/instance matrix points away from instance rebuilding.
6. Profile the remaining CPU kernel and only then test scanline
   specialization, SIMD dispatch, LTO, and PGO.
7. Capture a physical trace family covering light pressure, fast motion, long
   curves, eraser use, and distinct drawing styles.
8. Resume stable Atlas Intel/NVIDIA baselines only when that machine is idle;
   use Apollo for the current constrained-hardware work.
9. Add Wayland tablet-v2, filtering/mip experiments, and a second textured
   brush after the proof-system and hard-ink latency work.
