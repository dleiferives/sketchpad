# Implementation Status

Status: live implementation ledger, 2026-07-28. This note records what the
current executable actually does. Product intent remains in
[first-usable-product.md](first-usable-product.md); research claims and future
possibilities belong in the subject notes.

Selected next architecture, 2026-08-01: replace the CPU
stamp/mutate/recompose/upload loop and pen-up GPU readback boundary with the
page-batched GPU-resident layer and continuous-mask design in
[GPU-resident document and continuous brush migration](gpu-resident-document-migration.md).
The first non-live groundwork now exists in `stroke` and `round_geometry`:
validated full-precision paint/erase material state, separate opacity/flow
accumulation, timestamped round contacts, a packet-batching-invariant command
stream, and a CPU reference oracle for continuous variable-radius round
sweeps. The oracle treats consecutive segment overlap as one traversal while
allowing nonconsecutive self-crossings and pressure-separated subpaths to add
flow. `DocumentRevision` now also provides a monotonic identity for every
successful committed raster/structural transition and undo/redo; initial,
failed, empty-history, selection-only, and no-op states do not advance it.
Ordered `DocumentLayer` records now contain metadata only; stable `LayerId`
keys address a separate CPU raster-payload store. Chronological history keeps
metadata and IDs rather than owning pixel-bearing layer objects; it now lives
in a raster-free module with its branching, capacity, and exact-eviction
contracts tested independently. Detached payloads remain live exactly while
undo/redo can reach them and are reclaimed when a new branch makes them
unreachable. Existing layered checkpoint bytes and observable
undo/composition behavior are unchanged, and release tests pass on Atlas. The
inventory below continues to describe the executable at commit `373b0e7`; the
new stroke contract is not wired into live drawing yet.

The first GPU-atlas foundation is also non-live: a deterministic sparse
planner maps `(LayerId, TileCoord)` into stable row-major slots on lazy 2D
pages, reclaims unreachable keys, fails multi-key capacity requests without a
partial allocation, and groups resident work by physical page. The selected
1,024 px page / 128 px tile layout proves 64 logical tiles per pass-sized page,
16 MiB per full-float RGBA page, 4 MiB per scalar full-float mask page, and a
1,024-tile initial capacity. A regression test explicitly proves that 48
touched logical tiles on one page produce one page batch, not 48 tile batches.
A stateful round-mask scheduler now validates incremental command continuity,
computes conservative clipped damage, allocates the affected layer/tile keys,
and translates each dot or variable-radius sweep into its physical page slot.
Its tests cover a nine-tile sweep in one page batch, the exact 64-slot page
boundary, edge-tile clipping, incremental begin/sweep/end, layout mismatch,
and failure without partial scheduler/atlas mutation. GPU textures and passes
are now connected for the full-flow mask stage: a real `R32Float` target packs
all instances into one upload, lazily clears newly used physical slots, and
encodes exactly one max-blended render pass per touched page. The target
requires each encoded batch to be explicitly marked submitted or discarded
before its shared instance buffers may be reused; this prevents two command
buffers submitted together from silently seeing only the last queue write.
Color-page commit and live presentation are not connected yet.

The first Atlas GPU correctness smoke ran on its Intel UHD Graphics 630. A
two-tile continuous sweep encoded three instances, two slot clears, 152 bytes,
and one page pass. A second stroke reused one tile, cleared only that slot,
encoded one round instance plus one clear in 56 bytes, and kept the other
slot's prior mask intact. Exact readback checks saw new coverage `1`, cleared
coverage `0`, and retained-other-slot coverage `1`. These are correctness and
traffic facts, not timing measurements. The first smoke attempt also caught
that `from` is reserved in WGSL; the shader endpoints were renamed and the
validation error is now necessarily exercised by the smoke path rather than
being hidden by Rust-only compilation.

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
- ordered sparse raster layers with stable IDs, names, visibility, opacity,
  active-layer editing, and an incremental premultiplied-linear composite;
- hard-round source-over brush with no destination-color pickup;
- a tilt-oriented flat rectangular brush with anti-aliased contact, plus a
  pressure/tilt graphite pencil whose deterministic multi-scale paper tooth is
  fixed in canvas coordinates;
- a shared tilt dead zone, motion-direction mouse/upright fallback, and
  interpolated tilt for oriented natural brushes;
- an opaque palette knife on a visible topmost active layer uses connected
  continuous-contact geometry rendered into sparse full-float GPU scratch
  tiles; unsupported layer/opacity/GPU cases retain the older twelve-lane CPU
  implementation as a correctness fallback;
- a twenty-four-strand bristle brush with visible strand gaps, fixed
  per-strand deposition strength, selected-color output, and deterministic
  low-amplitude bundle wobble;
- distance-based deterministic dab resampling;
- native Atlas/XInput2 pen and eraser device discovery;
- normalized pressure, tilt, physical source ID, tool type, and source
  timestamp delivery;
- mouse fallback at pressure 1;
- pressure-sized pen ink and destination-out erasing;
- GPU brush-footprint cursor with circular, tilt-oriented box, and
  tilt-oriented ellipse modes that follows pressure and the active preset's
  actual contact geometry;
- a cached custom-painted egui overlay sharing the existing `wgpu` 30 device,
  surface texture, command encoder, and render pass;
- a compact first toolbar and brush-preset popover for hard round, eraser,
  flat nib, graphite pencil, palette knife, and bristle brush, plus
  diameter/opacity adjustment, a continuous HSV color picker, preset/recent
  colors, undo/redo, and interface hiding;
- a custom file popover dispatching the existing Open, Save, Save As, PNG
  import, full-canvas export, and content-bounds export workflows;
- a collapsible custom layer panel for selection, per-row visibility, create,
  duplicate, delete, ordering, and stepped active-layer opacity;
- a custom keybinding editor covering 32 application commands with two slots,
  physical-key capture, deterministic conflict displacement, confirmed reset,
  and versioned user-config persistence;
- typed UI actions that invoke the same application command methods as
  keyboard controls rather than owning document state;
- explicit mouse and direct-XInput tablet UI capture, with ownership fixed
  from contact through release and canvas-origin strokes unable to migrate
  into controls;
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
- one chronological 256-entry undo/redo sequence spanning raster gestures,
  layer insertion/import/duplicate/delete, rename, visibility, opacity, and
  reorder;
- versioned/checksummed layered recovery checkpoints with exact sparse `f32`
  pixels, legacy flat-checkpoint migration, atomic temporary-file replacement,
  and directory synchronization;
- named `.sketchpad` document Open, Save, and Save As through parented native
  dialogs, using the same exact layered container as recovery;
- independent explicit-document modification and recovery-freshness state, so
  autosave never clears the user-visible unsaved marker;
- bounded static-PNG import into a new centered/clipped sparse layer, with sRGB
  to premultiplied-linear conversion and explicit rejection of unsupported
  color metadata;
- interactive file-drop PNG import as one undoable layer command;
- parented native PNG import and export dialogs through the Linux XDG Desktop
  Portal backend;
- mouse and Wacom Alt-contact visible-composite color sampling with correct
  linear-premultiplied unassociation;
- an eight-entry session-local recent-color history: presets and completed
  picker gestures move one exact linear RGB value to the front, while picker
  motion only previews;
- atomic streaming flattened PNG export of the visible composite as
  straight-alpha RGBA8 sRGB, for either the full canvas or exact content
  bounds;
- automatic recovery on startup and autosave two seconds after the last
  committed edit;
- cancellation on focus loss or Escape during an active stroke;
- one-second live mean/p95/max CPU input-handler and frame-submit timing,
  upload traffic, page/capacity/residency, deferred-visible, eviction, and CPU
  tile counters;
- separate live UI event, declaration/tessellation, texture-update,
  GPU-buffer-preparation, prepared-cache-hit, and overlay-encoding counters;
- event-loop sleep while no redraw is requested.

Controls:

- left drag: draw;
- Wacom pen contact: pressure-sensitive draw;
- Wacom eraser contact: pressure-sensitive coverage erase;
- middle drag: pan;
- wheel: zoom at cursor;
- Home: center the canvas and fit it entirely in the viewport;
- drop a PNG file on the window: import it as a new active layer;
- Control/Command-I: choose a PNG to import as a new active layer;
- Alt-left drag or Alt-Wacom contact: sample visible color without painting;
- `[` / `]`: decrease/increase the hovered tool size;
- Shift-`[` / Shift-`]`: decrease/increase the hovered tool opacity;
- E: toggle mouse pen/eraser mode;
- B: cycle hard round, flat nib, pencil, palette knife, and bristle presets;
- 1–6: select a built-in pen color;
- X / Shift-X: select the older/newer recent pen color;
- Control/Command-Z: undo;
- Control/Command-Shift-Z or Control/Command-Y: redo;
- Control/Command-S: save to the active `.sketchpad` path, or choose one for an
  untitled/recovered document;
- Control/Command-Shift-S: choose a new `.sketchpad` document path;
- Control/Command-O: choose and open a `.sketchpad` document;
- Control/Command-Shift-N: create and activate a raster layer;
- Control/Command-Shift-D: duplicate the active layer;
- Control/Command-Shift-H: toggle active-layer visibility;
- Control/Command-Shift-E: choose where to export the full visible composite;
- Control/Command-Alt-Shift-E: choose where to export exact visible content
  bounds;
- Control/Command-Shift-Delete: delete the active layer when it is not the
  document's last layer;
- Page Up / Page Down: select the layer above/below;
- Control/Command-Page Up / Page Down: move the active layer above/below;
- F1: show or hide the interface;
- UI `LAYERS`: show or hide the layer panel;
- UI `KEYS`: open or close the keybinding editor;
- UI current-brush button: open the brush preset selector;
- keybinding slot: capture the next physical key and exact modifiers;
- Escape while capturing: cancel without changing the slot;
- Backspace while capturing: clear the slot;
- UI color swatch: open or close the color picker;
- color saturation/value plane: preview continuously and commit on release;
- color hue strip: preview continuously and commit on release;
- UI `FILE`: open the native document/import/export command surface;
- layer-panel row: select that layer;
- layer-panel visibility mark: show or hide that row's layer;
- layer-panel `+` / `COPY` / `DEL`: create, duplicate, or delete;
- layer-panel `UP` / `DOWN`: reorder the active layer;
- layer-panel opacity `-` / `+`: change active-layer opacity by ten percent;
- Escape while drawing: cancel the active stroke;
- Escape while idle: exit.

Command-line image I/O:

```text
sketchpad --import-png reference.png
sketchpad --import-png bottom.png --import-png top.png
sketchpad --export-png flattened.png
sketchpad --import-png reference.png --export-png converted.png
```

`--import-png` may repeat; files are inserted in argument order above the
current active layer, and an interactive import enters normal autosave.
`--export-png` writes the recovered/imported visible full-canvas composite and
exits without opening a window. A failed decode, layer insertion, or export
exits nonzero.

## Ownership Path

```text
window samples
    ↓
ActiveStroke brush-family dispatcher
    ↓
shared distance resampler
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
- PNG straight-alpha/sRGB conversion, grayscale alpha, centered clipping,
  resource/metadata limits, malformed input, content-bounds export, and
  decode-after-export quantization;
- checksum, truncation, trailing-data, and atomic replacement rejection/tests;
- destination-out erasing and undo restoration;
- no tile allocation when erasing empty space;
- final stroke caps;
- one undo entry across many input updates;
- independence from collinear event batching;
- exact shared round-dab coverage and distance-resampling behavior;
- continuous variable-radius round-sweep distance and antialias coverage;
- stable flow at connected sweep joins, additive flow at nonconsecutive
  self-crossings, and distinct pressure-separated traversals;
- validated round-command continuity, bounds, and packet-batching-invariant
  command generation;
- globally ordered raster/layer undo, redo invalidation, imported-layer
  restoration, and matched bounded raster-memento eviction;
- redo cleanup for both present layers and rasters temporarily retained by
  delete/create history commands;
- fixed-capacity recent-color eviction, exact deduplication, validation, and
  non-mutating wraparound traversal;
- camera mapping and view bounds.
- logarithmic UI brush-diameter mapping and linear-to-sRGB swatch conversion;
- redraw/raw-axis UI invalidation filtering;
- UI-origin tablet capture through release, canvas-origin ownership exclusion,
  and hover-boundary forwarding.
- disjoint toolbar/color/layer hit-region ownership, including uncaptured gaps.

### CPU brush replay

`brush_bench` replays the real hard-round, erase, flat, pencil, palette-knife,
and bristle transaction paths over the relevant empty and painted cases:

```text
cargo run --release --bin brush_bench -- --runs 12
```

The original Atlas hard-round result and the first Apollo natural-brush result
are recorded in
[performance-laboratory.md](performance-laboratory.md).

### Shared brush-kernel equivalence

On 2026-07-27, the hard-round brush's round-dab coverage and distance
resampling were extracted into shared internal primitives during the now
removed mixing experiment. An Apollo release replay used the canonical Wacom trace, the empty scene,
the 1× scheduled rate, and one stress/corpus stroke. Before and after the
refactor it produced the exact same raster checksum (`5aa3f3b00ec18bec`), 450
dabs, 45 damaged tiles, 44 resident tiles, 621 write lookups/bulk edits, and
325,985 conservative pixels.

This is an exact behavioral oracle rather than a timing claim: the extraction
changed code ownership without changing coverage, spacing, damage, allocation,
or final pixels. The shared primitives remain useful brush infrastructure even
though the mixing experiment was removed.

### Serialized Zellij remote execution

On 2026-07-27, launching the all-target test and Clippy commands concurrently
through `scripts/apollo run` exposed a transport bug. Both helpers pasted into
the same interactive SSH pane before either command completed. Bash received
the concatenated payloads, reported a syntax error near `then`, and neither
helper could observe its unique end marker, so both waited indefinitely.

The runner now takes a process-owned local lock keyed by Zellij session and tab
across synchronization, paste, capture, and exit-status collection. Concurrent
callers serialize, dead owner PIDs are reclaimed, and cleanup releases the lock
on normal exit or interruption. Parallel commands remain valid only when they
target different panes. This preserves the single-writer invariant of an
interactive terminal and prevents a verification harness failure from being
mistaken for a project failure.

Later on 2026-07-27, a transient missing-pane lookup exposed a second runner
failure: the process-wide exit trap ran after the command function's local
variables had gone out of scope, and strict unset-variable handling replaced
the useful missing-pane error with `screen_file: unbound variable`. Cleanup now
treats an out-of-scope or never-created capture path as empty while still
releasing the pane lock. A forced nonexistent-tab invocation verifies the
original error is reported without the cleanup cascade.

### PNG codec benchmark and oracle

`png_bench` creates controlled transparent, sparse, opaque-gradient, and
translucent-noise sRGB sources. It reports first and warm import/export
distributions, source/output bytes, decoded bytes, placed pixels, and sparse
tile counts as versioned JSON Lines. Each repetition checks stable encoded and
canonical-raster checksums; decode→export→decode must reproduce exact internal
pixels.

```text
scripts/apollo run cargo run --locked --release --bin png_bench -- \
  --size 2048 --warm-runs 7
```

The first controlled Apollo findings and their engineering consequence are
recorded in [png-io-contract.md](png-io-contract.md#controlled-apollo-codec-baseline-2026-07-27).

### Removed mixing-brush benchmark and oracle

The deleted `mixing_bench` compared hard-round and version-1 linear-mixing
strokes over transparent and opaque swatch scenes. Corpus geometry, input
count, pressure, colors, recipe, and initial/result checksums were fixed. First
and warm timings were separate; every run verified exact counters and undo
restoration.

The historical controlled Apollo baseline remains recorded in
[pigment-mixing.md](pigment-mixing.md#controlled-apollo-baseline-2026-07-27).
On the fixed corpus, linear mixing costs 1.38× the hard-round warm median over
transparency and 2.19× over painted swatches. The painted mixing snapshot
accounts for 14.5 MiB of the stroke's 18.875 MiB temporary pixel payload,
making lazy fixed-size pickup blocks the next evidence-backed representation
experiment.

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

Replay format version 8 adds `--presentation direct|cache-rgba32`. The
experimental cache is a canvas-sized `Rgba32Float` texture updated from queued
dirty regions and drawn once per frame. It deliberately preserves the
reference format: a tested `Rgba16Float` variant was removed after the
readback oracle detected nonzero output differences. Direct tiles remain the
application and replay default until the full-precision memory/performance
matrix is complete.

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
7. renders the identical state through the full-precision display cache;
8. requires exact byte equality between direct and cached output;
9. verifies that every resident tile is drawn with no deferred visible tiles;
10. fails if enough dark ink and cursor-colored pixels are not present.

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
- color mixing is deliberately absent; natural brushes deposit their selected
  color without destination sampling, and future mixing is a gated roadmap
  item;
- the first graphical toolbar, layer panel, file surface, and HSV picker are
  functional integration slices, alongside the first keybinding editor; there
  is not yet a brush editor, final responsive layout, rotation, selection, or
  transforms;
- keybindings currently cover keyboard physical keys and exact modifiers, not
  mouse buttons, wheel gestures, tablet buttons, key sequences, or
  layout-relative logical characters;
- the current named `.sketchpad` document is the exact layered snapshot
  container proven by recovery, not the eventual scalable schema: it has no
  embedded preview or serialized undo history, and the active named path is
  session-local rather than restored from recovery after restart;
- checkpoint encoding and disk I/O are synchronous after the idle delay and
  still need large-document timing and disk-full/kill testing;
- PNG I/O has explicit sRGB transfer behavior, but there is no general ICC,
  wide-gamut, or HDR color-management path;
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
- hard round, eraser, flat, pencil, bristle, and fallback palette-knife work
  remains CPU-only; the eligible palette knife has a GPU-owned live
  transaction and one asynchronous full-float commit at pen-up;
- repeated dab/tile intersections are not yet coalesced in the retained CPU
  brush paths;
- live physical-input diagnostics now separate hover and contact for relative
  X-source delivery excess, backend-to-event-loop queueing, and newest handled
  sample-to-submit delay; offscreen GPU render passes and explicit staged
  upload copies have timestamps, but `write_texture` transfer execution,
  display presentation, and input-to-photon latency are not yet instrumented.
  The live-window timings still end at the CPU presentation call; see
  [input-latency-investigation.md](input-latency-investigation.md).
- the first Apollo physical capture found approximately one 60 Hz interval in
  both hover frame construction/presentation pacing and p95 event-loop queue
  delay, while hover handling itself was effectively free; synchronous
  recovery also produced a measured 490 ms maximum input-queue stall.

## Immediate Engineering Order

The active delivery sequence is governed by
[feature-roadmap.md](feature-roadmap.md); performance experiments remain
defined in [performance-proof-plan.md](performance-proof-plan.md). The
immediate order is:

1. [Complete] Add an Apollo capability report for `Rgba32Float` render
   attachment, blending/storage, and GPU timestamps. Apollo's Intel JSL
   integrated GPU exposes full-float render attachment, fixed-function
   blending, read/write storage, and timestamp queries through Vulkan; see
   [Continuous brush contact and physical paint](continuous-brush-contact.md).
2. [Complete] Define deterministic transient contact poses and convex
   blade-sweep geometry; they remain derived renderer input rather than
   document operations.
3. [Rejected] The incremental CPU continuous-contact variants were slower than
   the old dab control at `512 px`; retain their evidence, not their product
   code. See the rejected-prototype table in
   [Continuous brush contact and physical paint](continuous-brush-contact.md).
4. [Complete] Benchmark a connected, fixed-color `512 px` palette-knife sweep
   offscreen on Apollo's GPU. The full-float opaque geometry proof measured
   8.366 ms empty and 8.353 ms painted versus 263.544 ms and 260.293 ms for
   the retained CPU control—approximately 31× faster. Readback is measured
   separately and remains forbidden from the live stroke path.
5. [Complete] Measure persistent-target incremental batches. Four new input
   samples cost 0.162 ms median / 0.417 ms p95 empty and 0.169 ms median /
   0.519 ms p95 painted on Apollo; all batch partitions produced identical
   final validation pixels for the opaque geometry proof.
6. [Complete] Define active-layer GPU/CPU ownership, below/active/above composition,
   cancel, undo, save, and recovery semantics before any live GPU mutation.
   The selected sparse scratch-tile transaction and tested source-over
   readback commit boundary are defined in
   [GPU active-stroke architecture](gpu-active-stroke-architecture.md).
7. [Live integration complete; physical validation next] Integrate a validated
   continuous-contact path, then
   extend it to flat, pencil, and bounded-strand marks. The design and quality
   gates are in
   [Continuous brush contact and physical paint](continuous-brush-contact.md).
   The reusable sparse `Rgba32Float` stroke target, asynchronous tile
   readback, exact six-tile GPU smoke round trip, and CPU commit/undo boundary
   are complete. Sparse presentation now reproduces the exact smoke fixture,
   the retained canvas exposes separate canvas/cursor stages, and the
   packet-persistent continuous-blade generator has batching-invariant
   geometry/damage tests. The executable now drives that generator from live
   input, presents scratch tiles between the canvas and cursor, retains them
   during asynchronous readback, and commits the result as one undoable CPU
   document edit. Apollo Wacom feel, visual continuity, cancel, and pen-up
   latency still require physical validation before removing the CPU fallback.
8. Validate the Wacom tilt mapping with a labeled calibration view.
9. Continue turning the proven toolbar, layer, file, color, and keybinding
   surfaces into a coherent usable drawing workflow.
10. Add frame-paced replay that processes every sample but coalesces GPU work to
   one submission per display opportunity.
11. Compare per-damage `write_texture`, per-frame dirty coalescing, and reusable
   staging-ring transfers with copy timing and exact GPU readback.
12. Measure the exact `Rgba32Float` display cache against direct tiles without
   changing image quality; keep reduced-precision formats out of the product
   path unless the quality policy explicitly changes.
13. Profile the remaining CPU kernel and only then test scanline
   specialization, SIMD dispatch, LTO, and PGO.
14. Capture a physical trace family covering light pressure, fast motion, long
   curves, eraser use, and distinct drawing styles.
15. Keep hardware runs on Apollo for the current constrained-device work; do
   not run the present brush campaign on Atlas.
16. Add Wayland tablet-v2, filtering/mip experiments, and a second textured
   brush after the proof-system and hard-ink latency work.
