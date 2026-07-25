# First Usable Product

Status: current product direction, 2026-07-24. This note defines the first
Sketchpad worth using. It supersedes any implication that deep zoom, infinite
extent, a universal retained renderer, or novel multiscale storage must be
solved before the application becomes useful.

## Product Decision

Build a **very fast, high-quality desktop drawing application** first.

The first product is:

- a defined, resizeable canvas;
- sparse tiled raster layers;
- excellent stylus response;
- a small set of excellent brushes;
- painterly pickup, deposition, and color mixing;
- fast layer compositing and per-gesture undo;
- ordinary pan, zoom, and rotation;
- reliable save, autosave, recovery, and export;
- a minimal canvas-first interface.

The first product is not:

- a proof of infinite zoom;
- a semantic-scale or nested-canvas system;
- a general vector graphics engine;
- an SVG/PDF authoring tool;
- a full wet-fluid simulator;
- a requirement to retain and replay every stroke forever;
- a mobile release;
- a complete clone of another painting application;
- a hundred-brush preset library.

Those remain possible later. They no longer block a usable painter.

## Product Promise

Sketchpad 0.1 should make this interaction feel exceptional:

1. Open or create a canvas.
2. Put pen to surface.
3. See a stable pressure-sensitive mark immediately.
4. Build and blend color naturally without visible stepping or lag.
5. Pan, zoom, rotate, undo, and change layers without breaking flow.
6. Save, close, reopen, and recover the work with confidence.

If that loop is not excellent, deeper rendering research does not rescue the
product.

## Product Priorities

In order:

1. **Input feel and active-stroke latency.**
2. **Brush quality and predictable color interaction.**
3. **Reliability of undo, save, and recovery.**
4. **Sustained performance on integrated graphics.**
5. **Canvas navigation and minimal UI friction.**
6. **Large-document behavior proportional to touched/visible content.**
7. **Additional brushes and editing tools.**
8. **Experimental deep zoom, retained media, and scale-aware features.**

When two features conflict, preserve the first six before expanding scope.

## Working Architecture

```text
stylus / mouse / touch
          │
          ▼
normalized real input ──► active stroke and brush reservoir
                                  │
                                  ▼
                   dirty tile set and brush commands
                                  │
                                  ▼
document ──► visible tile query ──► custom wgpu paint/composite passes
   │                              │
   │                              └──► surface
   │
   ├── sparse canonical raster tiles
   ├── layers, masks, order, and metadata
   ├── per-gesture tile snapshots
   └── transactional save/recovery
```

The canonical medium for the first painter is sparse raster tiles. Brush input
and recent stroke metadata may be retained for undo, diagnostics, or future
editing, but displaying a mature document must not require replaying every
historic stroke.

The `wgpu` runtime, GPU atlas/cache, pipelines, and staging resources remain
derived. Losing them must not lose the artwork.

### Implementation discipline

The drawing path follows
[performance-aware-code.md](performance-aware-code.md):

- sparse lookup, allocation, dispatch, and snapshotting occur in the control
  plane at sample/tile/gesture frequency;
- concrete brush kernels receive clipped contiguous tile memory and do not
  hash, allocate, lock, log, or dynamically dispatch per pixel;
- the first write to a tile in a gesture creates one explicit before-image;
- hot-path abstractions are extracted from multiple real uses rather than
  designed around hypothetical backends;
- convenient gesture/brush operations remain decomposable into lower-level
  tile operations;
- counters and deterministic release traces accompany the implementation.

This is a code-shape decision, not proof that a particular tile size, pixel
format, hasher, CPU/GPU split, or SIMD strategy is fastest. Those remain
measurements.

## Canvas Model

### Defined bounds

A document has explicit pixel dimensions. This provides:

- predictable export;
- clear resize/crop behavior;
- a useful overview and center;
- bounded file validation;
- simpler thumbnails and print intent;
- fewer coordinate and navigation edge cases.

Canvas bounds should be resizeable. Automatic expansion can be added later
without changing tile storage.

The application must not allocate a full image-sized texture merely because
the document has large dimensions.

### Sparse tile allocation

Each raster layer is divided into fixed-size document tiles. A tile exists only
after content touches it or another operation requires material state there.

Candidate tile sizes such as 128×128 and 256×256 must be benchmarked. Tile size
affects:

- brush dispatch overhead;
- border/halo overhead;
- copy-on-write undo cost;
- cache fragmentation;
- upload granularity;
- compression ratio;
- large-brush behavior;
- mobile tile/locality behavior.

Tile size is an implementation parameter, not a file-format identity unless
measurements show persistence is beneficial. The file schema should permit
migration.

### Bounds and damage

Track:

- conservative bounds per brush update;
- touched tile set per gesture;
- nonempty bounds per layer;
- visible bounds per layer/group;
- document content bounds;
- dirty rectangles within a tile where useful.

The renderer should scale primarily with damaged visible pixels and affected
tiles, not declared canvas area or total historical stroke count.

### Pixel coordinate model

Canonical raster content lives on an integer pixel grid within defined canvas
bounds. CPU view transforms may use wider floating-point precision, while GPU
geometry remains camera-relative or tile-local `f32`.

Tile keys should not assume the whole layer is one GPU texture. A future
unbounded canvas can extend the key range without replacing brush/compositor
semantics.

## Canonical Tile Content

### Color

Normal compositing is specified in a named linear working color space using
premultiplied alpha. The first implementation must not accidentally mix or
blend gamma-encoded RGB values.

Candidate working tile formats require measurement:

- normalized 8-bit storage with explicit linearization;
- higher-precision normalized storage;
- `RGBA16Float` working tiles;
- a compact persisted representation plus higher-precision active/GPU tiles.

`RGBA16Float` gives comfortable working precision but doubles or quadruples
traffic relative to compact formats. The performance laboratory must compare
visible error, memory, bandwidth, and mixing stability before the file format
is frozen.

### Optional material state

Persistent tiles initially contain final premultiplied color and alpha.
Additional channels are allowed only for a brush or layer mode that proves
their artistic value.

Temporary active-region state may include:

- wetness;
- picked-up color or pigment mixture;
- deposition capacity;
- local height/coverage;
- velocity or direction;
- drying age.

Temporary state is discarded or baked at a named boundary, normally stroke
finalization or a short settling interval. The first product does not require
an indefinitely running full-canvas simulation.

## Brush Engine

### Shared stroke lifecycle

Every brush follows:

```text
begin
  receive real samples
  update stable prefix and active tail
  replace predictions if enabled
  mutate only bounded tiles
finalize
  commit one undo transaction
  bake or release temporary brush state
```

The brush receives real timestamped input and a versioned recipe. GPU dabs,
meshes, coverage, tile commands, and reservoir state are derived.

### Brush spacing and sampling

Brush output must be stable under irregular input packet timing. Depending on
the brush, resampling may use:

- cumulative path distance;
- time/dwell;
- curvature and direction;
- pressure/tilt change;
- an error-bound adaptive rule.

No brush should place one canonical document operation per raw event simply
because that is how the windowing system delivered input.

### Brush reservoir

Painterly behavior uses a small state carried along the active stroke:

- current held color/mixture;
- pickup capacity;
- deposit load;
- dilution/wetness;
- optional tip zones or texture state.

For each bounded contact step:

1. sample destination color/material under the tip;
2. pick up a controlled amount;
3. mix it into the reservoir using the selected mixing rule;
4. deposit a controlled amount back to the canvas;
5. update the reservoir deterministically.

This can create convincing dirty-brush and blending behavior without a
full-canvas fluid simulation or persistent high-dimensional pigments in every
tile.

### Mixing modes

Keep separate:

1. **Normal layer compositing**: premultiplied source-over in the working color
   space.
2. **Brush pickup/deposit**: destination-dependent interaction within the
   active raster layer.
3. **Color interpolation**: how two reservoir colors mix—linear RGB,
   perceptual, or pigment-like.
4. **Wet simulation**: optional local state evolution after deposition.

The first product prioritizes brush pickup/deposit with a pigment-like
interpolation option. It does not replace all layer compositing with pigment
math.

The existing [pigment research](pigment-mixing.md) remains relevant to the
mixing kernel. Licensing, gamut behavior, neutral mixtures, and comparison
against simpler interpolation must be resolved before selecting a specific
implementation.

## Initial Brush Set

Three excellent brushes are a stronger first product than many shallow
presets.

### 1. Hard pressure ink

Purpose:

- validate latency, pressure, smoothing, caps/joins, and crisp coverage;
- provide a useful ordinary drawing/inking tool.

Required behavior:

- stable width response;
- smooth active tail without delayed rubber-banding;
- no cracks between samples;
- predictable self-overlap;
- document-space width after input mapping;
- ordinary source-over against prior content;
- deterministic replay within the same brush version.

The first raster implementation may bake ink into tiles. A retained crisp-ink
layer can be added later if deep zoom or stroke editing earns its complexity.

### 2. Textured pencil/chalk

Purpose:

- validate deterministic grain, pressure-to-density behavior, and page-locked
  texture.

Required behavior:

- texture anchored to document space so it does not swim during pan/zoom;
- deterministic seed;
- pressure can affect coverage, size, or both according to the preset;
- repeated passes build density naturally;
- no obvious stamp rhythm at normal drawing speeds.

### 3. Painterly mixing brush

Purpose:

- define Sketchpad's differentiating color interaction.

Required behavior:

- configurable pickup and deposit;
- a visible reservoir that becomes contaminated by destination color;
- smooth pressure/tilt response;
- bounded tile interaction;
- deterministic result for a saved real input trace;
- no implicit mixing across hidden layers;
- finalize without a long blocking simulation.

### Fourth brush after the core is stable

A soft airbrush is the next useful stress case:

- continuous opacity/density accumulation;
- large soft footprints;
- heavy overdraw;
- sensitivity to tile borders and precision.

Wet flowing paint, smudge, clone, and complex bristle simulation are later
brush families.

## Input and Active Feedback

Input is a product subsystem, not a window-event afterthought.

The normalized model should preserve:

- monotonic timestamp;
- document position;
- pressure;
- tilt;
- twist/orientation where available;
- tool and eraser state;
- real/coalesced provenance;
- optional prediction in a separate lifetime.

For the first usable desktop build:

- mouse input remains a fallback;
- stylus pressure must work on the primary development setup;
- real input is canonical;
- predicted input, if used, is visually replaceable and never saved as real;
- one gesture becomes one undo transaction.

The active path may draw a transient overlay or directly update staged tile
state. The chosen path must not show blank, doubled, or stale ink at
finalization.

## Layers and Compositing

Required first-product layer behavior:

- ordered raster layers;
- visibility;
- opacity;
- normal source-over compositing;
- create, delete, duplicate, rename, and reorder;
- clear and merge/flatten with explicit confirmation;
- per-layer sparse tiles and bounds.

Masks, clipping groups, non-normal blend modes, adjustment layers, and complex
filter graphs can follow after the ordinary layer contract is solid.

The brush interacts with the active layer by default. Pickup from the
composited visible result is an optional explicit mode because depositing the
result back into one layer can otherwise create surprising cross-layer
semantics.

## Eraser

The first eraser is a raster coverage eraser on the active layer:

- pressure-sensitive;
- hard and soft presets;
- bounded tile damage;
- one gesture per undo transaction;
- deterministic alpha reduction in the named working space.

Whole-stroke and geometric split erasers require retained objects and are
post-MVP features. A transparent-color brush is not automatically equivalent to
erasing premultiplied content; the operation must explicitly reduce
destination coverage.

## Undo and Redo

Use copy-on-write or before-image tile snapshots per gesture.

For each tile touched by a gesture:

1. capture/share its prior state the first time it will change;
2. apply all subsequent updates in the active transaction;
3. commit one history entry on finalization;
4. undo by restoring the prior tile references/data;
5. redo by restoring the committed result or replaying a bounded deterministic
   transaction.

Layer commands remain semantic history entries.

Required policies:

- explicit memory budget;
- compression or spill for older snapshots;
- no snapshot per input packet;
- cancelling a stroke restores its before-images;
- undo must not depend on GPU cache survival;
- autosave must not observe a half-committed gesture as a finished operation.

## GPU and Cache Model

### GPU-visible tile cache

Keep only an explicit budget of active and likely visible tiles resident.
Possible physical layouts include:

- texture atlas;
- 2D texture array;
- individually allocated textures where practical;
- buffer-backed or storage-texture work surfaces for special brushes.

The document tile key maps to a transient GPU slot. Eviction discards or
downloads dirty state only according to a clear ownership protocol.

### Ownership

At every moment, each mutable tile needs one authoritative recoverable state:

- CPU/document state;
- GPU dirty state with a scheduled/readable commit path;
- or an active transaction that owns both before-image and working result.

Ambiguous “newest copy” rules will produce save and device-loss corruption.

### Damage path

A brush update should:

1. determine a conservative footprint;
2. enumerate intersected tiles;
3. make them resident;
4. run only required brush/mix work;
5. mark exact tile revisions/damage;
6. composite only affected visible regions when possible.

No full-canvas texture upload or full-document replay belongs in the normal
stroke path.

### Idle behavior

When the user is not drawing, navigating, refining, or animating UI, the
application should stop requesting continuous redraw. A painting application
should not burn GPU and battery while showing an unchanged canvas.

## Native Document and Recovery

The first usable product requires a native format early, not after brush
research.

Working direction:

- SQLite application file;
- versioned document/layer/brush metadata;
- compressed sparse tile blobs;
- atomic gesture/layer transactions;
- autosave checkpoints;
- optional thumbnail and recovery image;
- schema migrations;
- derived GPU/cache data excluded or explicitly disposable.

This remains a working choice until a small failure-oriented experiment verifies
process kill, disk full, partial save, migration, and large sparse documents.

Interchange:

- PNG for flattened export;
- OpenRaster as the likely first layered interchange candidate;
- ordinary image import.

SVG, PDF, PSD fidelity, cloud collaboration, and live WAL synchronization are
not first-product requirements.

## Minimal Interface

The permanent interface should expose:

- canvas;
- brush/preset choice;
- size/opacity controls;
- color picker and recent colors;
- active-layer control;
- undo/redo;
- save/open/export;
- pan/zoom/rotate/reset view.

Useful early affordances:

- press-and-hold or modifier color sampling;
- quick brush/eraser toggle;
- canvas-only mode;
- clear indication of save/recovery status;
- visible active layer and selected color.

A large preference system, plugin marketplace, animation timeline, vector tool
suite, and elaborate dock layout are outside the first-product boundary.

## Provisional Performance Gates

These are initial hypotheses for the laboratory, not promises independent of
hardware and display.

### Target hardware

Atlas's Intel UHD 630 is the primary desktop performance floor. The GTX 1650
Mobile is the discrete comparison. A result that only succeeds on NVIDIA does
not satisfy the first product.

### Active drawing

At a 1080p-class viewport on the Intel adapter:

- ordinary hard-ink active updates should keep application CPU plus GPU work
  comfortably inside a 60 Hz frame;
- p95 renderer work for the canonical active scenario should target under
  8 ms, leaving time for input, windowing, and presentation;
- p99 should avoid crossing 16.7 ms during sustained ordinary drawing;
- finalization should not introduce a visible blank/double frame or block the
  next input gesture;
- work should scale with damaged tiles rather than declared canvas dimensions.

The eventual 120 Hz target requires roughly half the total frame interval and
will motivate a more aggressive active overlay. It is not required to call the
first build useful.

### Navigation

For a warm visible working set:

- pan/zoom/rotate should sustain 60 Hz on the Intel adapter;
- crossing tile boundaries should not hitch;
- a cold region should show a coherent fallback before refinement rather than
  stale or unrelated pixels.

### Memory

- GPU tile/cache memory has an explicit configurable budget;
- changing canvas dimensions without painting does not allocate proportional
  pixel storage;
- document memory grows approximately with nonempty tiles, layers, undo, and
  active caches;
- undo and cache budgets report/evict rather than growing without bound.

### Reliability

- killing the process after committed gestures loses at most the documented
  autosave interval;
- device loss can reconstruct the visible canvas from document state;
- save/open round-trips the working-space pixels within the specified format
  error;
- cancel and undo restore exact prior tile content for deterministic modes.

## First Benchmark Suite

The first laboratory suite should reproduce:

> Draw a pressure-sensitive stroke through an already dense region on a large
> sparse canvas.

Document variants:

- empty active layer;
- sparse marks;
- dense opaque overpainting;
- dense translucent overpainting;
- 8–16 visible raster layers;
- warm visible tiles;
- cold visible tiles;
- memory budget forcing eviction.

Actions:

- hard-ink stroke;
- textured stroke;
- mixing stroke that picks up two destination colors;
- undo and redo;
- pan away and return;
- save checkpoint during idle after finalization.

Record:

- sample-to-submit application interval;
- CPU time by stage;
- GPU time by pass;
- damaged and resident tiles;
- candidate dabs/segments;
- bytes uploaded/copied;
- GPU/CPU/cache memory;
- p50/p95/p99 frames;
- finalization time;
- undo/save time;
- output and difference images;
- mixing result and deterministic replay.

This suite is more relevant to the first product than Paris, SVG Tiger, or
extreme semantic zoom.

## Milestones

### Milestone 0 — Contract and measurement

- freeze first-product scope and non-goals;
- define normal compositing and working-space candidates;
- save deterministic input/view traces;
- make Atlas adapter choice explicit;
- establish raw timing/work/image result records.

Exit: one existing prototype path can run through a reproducible offscreen
scenario on both Atlas GPUs.

### Milestone 1 — Sparse tile core

- tile addressing and allocation;
- layer tile store;
- bounds and damage;
- copy-on-write gesture transaction;
- CPU-visible correctness reference;
- explicit memory accounting.

Exit: large declared canvases allocate only touched tiles, and undo restores
exact prior content.

Implementation status, 2026-07-24: achieved for the first single-layer `f32`
reference path. Persistent multi-event gestures, exact damage, cancellation,
empty-tile reclamation, and swap-based undo/redo are covered by deterministic
tests. Pixel format and commit-bound maintenance remain performance
experiments.

### Milestone 2 — GPU canvas and hard ink

- GPU tile residency/atlas;
- dirty tile upload/commit protocol;
- visible tile compositing;
- hard pressure ink;
- pan/zoom/rotate;
- idle-on-no-damage.

Exit: the canonical hard-ink trace stays within the provisional Intel budget
without full-canvas upload or replay.

Implementation status, 2026-07-24: in progress. Bounded GPU tile residency,
dirty-subrectangle uploads, visible tile instancing, a deterministic hard
round brush, native Atlas pressure, pan/zoom, and idle-on-no-damage are
connected end to end. A sustained physical Wacom run grew to 911 visible tiles
across four GPU pages without deferral or eviction. Rotation, GPU stage
filtering, live presentation timing, and input-to-photon measurement remain.
The recorded offscreen runner now reports separate CPU stages and hardware
render-pass timestamps on both Atlas adapters and Apollo.

### Milestone 3 — Input and drawing feel

- primary stylus pressure path;
- smoothing/resampling;
- active tail and optional prediction;
- cancellation/finalization;
- latency instrumentation.

Exit: sustained drawing feels stable and produces one deterministic transaction
per gesture.

Implementation status, 2026-07-24: in progress. The Atlas/X11 adapter preserves
physical pen versus eraser identity, pressure, tilt, surface position, and
unwrapped source timestamps; mouse fallback remains. Constant-distance
resampling, cancellation, and one transaction per contact are connected. A
GPU footprint cursor and between-stroke size/opacity controls now expose the
actual brush state. One physical Wacom stroke is now a versioned,
content-hashed replay fixture exercised unpaced and at 1×/2×/4× over
deterministic existing content. A representative physical trace family,
drawing-feel evaluation, coalesced history, active-tail handling, and
input-to-presentation latency instrumentation remain.

### Milestone 4 — Painterly mixing brush

- reservoir;
- pickup/deposit;
- linear and pigment-like mixing candidates;
- textured/tilted contact where justified;
- bounded temporary material state.

Exit: the mixing corpus produces convincing, deterministic color interaction
without breaking the active-frame budget.

### Milestone 5 — Layers, UI, and eraser

- minimal layer panel and commands;
- active-layer raster eraser;
- brush/color controls;
- canvas-only workflow;
- ordinary shortcuts and feedback.

Exit: a complete small artwork can be created and edited without diagnostic
controls.

### Milestone 6 — Save, recovery, and export

- native transactional document;
- autosave/recovery;
- open/reopen;
- PNG and layered interchange experiment;
- corruption and kill testing.

Exit: the application can be trusted with real work.

Implementation status, 2026-07-24: started with an explicitly interim raster
recovery checkpoint. It stores canvas geometry and nontransparent
premultiplied pixels as ordered sparse row runs under a versioned, checksummed
header. Writes use a same-directory temporary file, file synchronization,
atomic replacement, and parent-directory synchronization on Unix. The
application recovers this checkpoint on startup and autosaves committed state
after a short idle delay. This does not yet satisfy the native-document exit:
it has no layers or semantic brush state, file chooser/Save As, migration,
preview/export, incremental journal, background I/O, or kill/disk-pressure
evidence.

### Milestone 7 — Usability and sustained performance

- long-session traces;
- cache/undo pressure;
- integrated-GPU profiling;
- hitch and p99 cleanup;
- brush tuning;
- install/run workflow.

Exit: Sketchpad is something the developer chooses to draw in, not merely a
renderer demonstration.

## Post-MVP Research Queue

After the first product is useful, evaluate:

1. more brushes and richer brush editor;
2. retained/editable hard-ink layers;
3. mip/LOD generation for enormous canvases;
4. automatically expanding or unbounded canvas extent;
5. mobile input, lifecycle, memory, and thermal work;
6. persistent wet simulation/material layers;
7. advanced masks, filters, and blend modes;
8. semantic scale and nested canvases;
9. extreme coordinate depth;
10. native Metal/Vulkan specializations proven by profiling;
11. broad vector/SVG/text integration;
12. collaboration and cloud storage.

The existing research notes remain valuable evidence for these items. They are
not the first-product checklist.

## Decisions Still Needed Soon

These are near-term decisions, not permission to expand scope:

1. Which working color space and tile precision give the best
   quality/bandwidth balance?
2. Which tile size wins the first brush and undo corpus?
3. Is GPU or CPU the authoritative mutable tile copy during an active gesture?
4. What is the exact pickup/deposit/reservoir equation?
5. Which pigment-like interpolation is acceptable in quality and licensing?
6. Which pressure curve and event-batching policy win on the first real Atlas
   Wacom traces?
7. Which native document layout survives the failure tests?
8. What GPU and undo memory budgets are appropriate for the development
   machine and later minimum device?

These can be answered by narrow experiments inside the first-product
architecture.

## Bottom Line

The first Sketchpad should be a painter, not a graphics research showcase.

Its architectural bet is deliberately practical:

> defined canvas bounds, sparse canonical raster tiles, custom bounded `wgpu`
> brush/composite work, a small excellent brush set, brush-local color mixing,
> per-gesture tile undo, and transactional recovery.

That is enough room to build something unusually fast and artistically
interesting. The more ambitious retained, infinite, multiscale, and simulation
features can grow on top after the basic drawing loop earns them.
