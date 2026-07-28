# Sketchpad Architecture Notes

Status: architectural boundaries supported by current research. The first
product uses sparse canonical raster layers and a narrow Sketchpad-owned
`wgpu` paint/composite architecture. Later media and backends remain subject to
experiments. See
[first-usable-product.md](first-usable-product.md) for the governing product
scope,
[renderer-selection.md](renderer-selection.md) for the renderer decision and
[research-synthesis.md](research-synthesis.md) for evidence and next research.

## Goal

First build a responsive desktop painter whose saved document remains
independent of GPU caches and whose brush, compositing, undo, and color-mixing
semantics are explicit. Preserve boundaries that permit later platform and
media expansion.

The current technical direction is Rust with `wgpu`/WGSL. Sketchpad owns the
render plan, resource/cache policy, and specialized brush kernels. A library
such as Vello may implement a replaceable conventional-vector backend; its
scene and caches are not application or document boundaries. This choice does
not commit the document to vector, SDF/ADF, raster, or hybrid storage.

For the first product specifically, the working commitment is sparse canonical
raster tiles. The broader media independence applies to the architecture and
future layer types, not to delaying the initial painter.

## First-Product Path

The critical path is:

```text
real stylus input
      ↓
active stroke + brush reservoir
      ↓
bounded dirty tile set
      ↓
custom wgpu paint/mix work
      ↓
sparse canonical raster layer tiles
      ↓
visible-tile compositing and presentation
```

Required supporting systems:

- defined and resizeable canvas bounds;
- per-layer sparse tiles and bounds;
- per-gesture copy-on-write tile undo;
- explicit premultiplied linear compositing;
- brush-local pickup/deposit and pigment-like mixing candidates;
- GPU tile residency with clear CPU/GPU ownership;
- transactional native save/autosave/recovery;
- minimal layers, eraser, navigation, and canvas-first UI;
- the trace-driven performance/correctness laboratory.

Not on the first-product critical path:

- unbounded coordinates or extreme zoom;
- semantic scale and nested canvases;
- retained replay of all historic strokes;
- Vello integration;
- general vector/SVG/text rendering;
- persistent full-canvas wet simulation;
- mobile release engineering.

The complete milestones and measurable gates are in
[first-usable-product.md](first-usable-product.md).

## Stable Boundaries

The research supports these boundaries regardless of the final renderer:

```
┌─────────────────────────────────────────────────────────────┐
│ Application                                                 │
│ tools · layer UI · commands · save/recovery · preferences   │
├─────────────────────────────────────────────────────────────┤
│ Canonical document                                          │
│ ordered layer media · sparse tiles · recipes · undo/history │
├─────────────────────────────────────────────────────────────┤
│ Stroke/input domain                                         │
│ normalized samples · live stroke · prediction · finalizing  │
├─────────────────────────────────────────────────────────────┤
│ Derived scene state                                         │
│ visible tiles · paths/meshes · spatial index · damage · LOD │
├─────────────────────────────────────────────────────────────┤
│ Render backends                                             │
│ analytic │ vector strips │ density/stamps │ raster/simulation│
├─────────────────────────────────────────────────────────────┤
│ GPU/runtime                                                 │
│ resources · upload · passes · surface lifecycle · recovery  │
├─────────────────────────────────────────────────────────────┤
│ Platform adapters                                           │
│ window/display · tablet/stylus · touch · files · lifecycle  │
└─────────────────────────────────────────────────────────────┘
```

Dependencies should point downward. A saved stroke must not contain a `wgpu`
handle, and the GPU layer must not decide what an eraser means.

## Canonical Versus Derived State

### Canonical

Canonical state is sufficient to:

- save and reopen the artwork;
- apply undo/redo;
- rebuild after device loss;
- regenerate a missing LOD;
- migrate the file format;
- reproduce deterministic brush behavior.

Candidate canonical media:

- ordered logical strokes plus immutable, versioned brush definitions and
  deterministic seeds;
- sparse raster paint tiles;
- masks, transforms, layer properties, and semantic commands.

It is possible that different layer types have different canonical media.

### Derived

Derived state exists for performance:

- smoothed/fitted paths;
- expanded outlines or meshes;
- spatial acceleration structures;
- ADF cells;
- cached RGBA tiles and mips;
- active-stroke preview geometry;
- GPU buffers, textures, bind groups, and pipelines.

Every derived object needs:

- a source revision;
- conservative document-space bounds;
- a validity state;
- a rebuild path;
- an eviction policy if it consumes significant memory.

## Input and Stroke Lifetime

### Collection

Native platform backends collect the richest available input. The normalized
event should retain provenance instead of pretending all platforms are equal:

```
Sample
  position
  monotonic timestamp
  optional pressure
  optional tilt
  optional orientation/twist
  optional tangential pressure
  tool identity and buttons
  real | coalesced-real | predicted
```

`winit` can provide the event loop, window, pointer fallback, and some touch
data. It is not assumed to provide the complete tablet stream.

### Active stroke

One mutable active stroke accepts batches of real and predicted samples.
Prediction is replaced as new real data arrives.

The active stroke may maintain a transient mesh, brush simulation, or tile
overlay. That representation is optimized for latency and can be approximate.

When the modeler guarantees a stable committed prefix, that prefix may enter an
incremental cache. The unstable real tail is redrawn locally and the predicted
tail is always replaceable:

```
stable committed prefix | unstable real tail | predicted tail
```

The overlay remains visually above the retained document until authoritative
content is ready; finalization must not expose a blank or duplicate frame.

“Overlay” does not imply a separate `wgpu` presentation mode. Present mode is
surface-wide. The portable renderer may use retained final-composite regions
and transient content inside one surface frame; a true front-buffer active
layer is a later native platform capability. The active representation for
real input must produce the exact authoritative result. Only replaceable
prediction may be approximate. See
[Active-stroke presentation](active-stroke-presentation.md).

### Finalization

On pen-up:

1. discard unreconciled prediction;
2. finalize the canonical stroke or raster transaction;
3. create one semantic history command;
4. update spatial bounds and revisions;
5. invalidate or merge affected derived regions;
6. schedule any authoritative background refinement.

This makes “one gesture equals one undo” independent of how many dabs or tiles
were touched.

## Document and Layers

The minimum semantic model is:

```
Document
  metadata and working color space
  ordered layers
  history / recovery journal

Layer
  identity, name, visibility, opacity
  blend/composite mode
  transform and optional mask
  canonical medium
  derived-render revision
```

Possible canonical media:

- retained stroke operations;
- sparse premultiplied-RGBA tiles;
- a future procedural field representation;
- groups referencing child layers.

The exact variant model waits for the representative-mark product study.

## Compositing Contract

Default layer compositing should be defined before cache layout:

- named working color space and transfer function;
- premultiplied color representation;
- Porter-Duff operator, normally source-over;
- distinction between geometric coverage, brush opacity, pixel alpha, mask
  alpha, and layer opacity;
- group isolation and blend-space behavior.

Pigment interpolation is not the implicit default. It can be an explicit wet
brush process or blend mode after its semantics and licensing are resolved.

For normal premultiplied source-over, ordered groups can be reassociated. This
may permit a sparse reduction cache for dense tiles. That optimization stops at
semantic boundaries such as masks, filters, isolated groups, non-normal blend
modes, and destination-dependent paint. Cache grouping must never change
visible order.

## Damage, Revisions, and Scheduling

A single global `dirty` flag is insufficient for a large document.

Conceptually:

```
source edit
  ├─ increments document/layer/operation revision
  ├─ reports conservative affected bounds
  ├─ invalidates intersecting derived entries
  └─ requests a redraw only for visible affected regions
```

The scheduler may prioritize:

1. active-stroke feedback;
2. current viewport at display resolution;
3. authoritative replacement of approximations;
4. nearby/likely navigation regions;
5. background mips or persistence caches.

Idle state should use event-driven redraw rather than perpetual polling.

## Renderer Candidates and Later Media

The current hypothesis is a renderer dispatcher rather than one universal
backend. It compiles or classifies the canonical brush graph and chooses a
derived representation by mark semantics, device capability, view scale, and
measured cost. Switching backends must preserve a bounded visual-equivalence
contract.

This dispatcher is an extensibility boundary, not first-product scope. The
first implemented media path is sparse raster painting with custom bounded
`wgpu` work. Other backends enter only after the ordinary painter is usable and
measured.

### Direct retained vector

Encode ordered paths and styles, spatially cull them, and render through a
wrapped Vello Hybrid backend or a custom pipeline. This is the baseline for
resolution-independent hard strokes and transparency. Vello is leverage and a
benchmark, not the owner of the canvas scheduler or canonical scene.

The Levien/Uguray stroke-expansion paper solves path-to-outline expansion. It
does not solve document storage, brush simulation, ADF updates, or undo.

Vello's sparse-strip and Hybrid work should be benchmarked as a CPU-SIMD/GPU
composition baseline. Its internal CPU/GPU split changed substantially in July
2026, so its cache representation is not a durable document boundary.

### Analytic hard-edge coverage

Slug-style curve bands directly evaluate closed quadratic outlines and are now
practical to investigate under permissive reference licensing. This backend is
a candidate for crisp finalized strokes, vector fills, and text.

It still needs correct variable-width outline generation, cubic conversion
with error bounds, spatial band construction, and comparison with Vello and
expanded meshes on target GPUs.

### Continuous density and textured stamps

Ciallo-style renderers suggest two separate paths:

- integrate a continuous density kernel for dense airbrush accumulation;
- place textured stamps by cumulative path length and evaluate only bounded
  candidates.

These are candidates for soft and textured marks. Sharp corners, fast radius
change, anisotropic tips, self-overlap, and portable implementation remain
research.

### Procedural distance / ADF

Evaluate or store adaptive distance data for geometry. A practical design still
needs:

- a concrete cell/reconstruction format;
- an error metric;
- candidate-primitive lookup;
- sign/inside classification;
- cell border and LOD rules;
- update/reorder/erase costs;
- color and order outside the scalar field.

This remains a research candidate.

### Sparse tiled raster

Store premultiplied RGBA in allocated-on-demand document tiles. Use bounded
damage, copy-on-write/memento undo, mip levels, compression, and swap/eviction.

This is the strongest known architecture for soft/textured/smudging media, but
it has a fixed source resolution.

### Local simulation island

Wet paint may allocate height/pigment/velocity state only in bounded sparse
regions. Its canonical contract must say whether inputs plus a versioned
simulation and seed are replayable, or whether a result is intentionally baked
as raster state. Whole-canvas simulation is not assumed.

### Hybrid

Compose multiple explicit layer media. This avoids making one representation
solve incompatible semantics but increases complexity at selection, transform,
mask, file-format, and UI boundaries.

## Undo and Recovery

Two proven mechanisms can coexist:

- **Semantic commands** for retained operations, layer changes, transforms, and
  finalizing a logical stroke.
- **Copy-on-write tile snapshots or mementos** for raster paint transactions.

Derived caches are not the only undo record. They may be snapshotted to
accelerate undo, but cache eviction must not destroy history.

Recovery research should cover an append-only journal, autosave checkpoints,
partially written files, and deterministic cache rebuilding.

SQLite is now a concrete native-document candidate because it provides
transactions, partial loading, incremental updates, and schema migration in one
file. OpenRaster/SVG/PNG remain interchange candidates. A live WAL database is
not assumed safe for network/cloud synchronization; portable save/snapshot
semantics need a separate design.

## Coordinate Model

The current `f32` document coordinates cannot justify an “infinite” claim.
Candidates include:

- `f64` canonical coordinates with camera-relative `f32` GPU coordinates;
- integer document-tile coordinates plus local floating-point offsets;
- hierarchical coordinates and origin rebasing;
- nested local 2D coordinate frames for deliberately deep drawings.

View scale, derived LOD, painter's order, and optional semantic scale are
independent. Ordinary marks remain 2D; drawing while zoomed in already produces
small world-space geometry when a brush is screen-size controlled. A canonical
scale interval is justified only when content should deliberately appear,
disappear, change representation, or open a nested canvas.

Derived sparse/cache storage should prefer hierarchical `(level, x, y)` keys
over a dense 3D texture. A scale-aware `(x, y, log₂ scale)` index is a candidate
for explicitly semantic-scale content, not a replacement for layer/order
semantics. The complete comparison and proposed experiments are in
[scale-space-storage.md](scale-space-storage.md).

The selection needs a written error budget for:

- maximum useful extent;
- maximum zoom ratio;
- stroke-width precision;
- transform composition;
- curve evaluation;
- screen antialiasing.

## Performance and Correctness Laboratory

Renderer evidence comes from deterministic state-transition traces, not static
demos. Each comparison declares:

- the canonical document before the action;
- real input, prediction, edit, and view traces;
- cold/warm/partially invalid/memory-pressure cache state;
- backend, adapter, shader variant, and quality contract;
- semantic and image oracles;
- raw CPU/GPU/frame, work, transfer, memory, and error records.

Drawing a new stroke over empty, sparse, dense, translucent, layered, and
adversarial existing content is a first-class benchmark family. Active update,
prediction replacement, finalization, old-content edit/replay, navigation,
recovery, and sustained mobile behavior are measured separately.

The detailed artifact format, scenario axes, timing vocabulary, hardware
cadence, and regression policy are in
[performance-laboratory.md](performance-laboratory.md).

## Current Prototype Mapping

| Current component | What it proves | Why it is not yet the boundary |
|---|---|---|
| `Document` with circle operations and embedded field | replayable operations can drive a derived field | canonical source and cache are coupled; dab is mistaken for stroke |
| fixed CPU `SdfCanvas` | localized hard-circle union | fixed extent/resolution; only silhouette semantics |
| full `R32Float` upload | simple display path | bandwidth scales with full canvas, not damage |
| display shader | camera transform and sampled edge display | no layers, color order, LOD, or cache selection |
| `main.rs` app/GPU/input loop | end-to-end spike | lifecycle, input domain, renderer, and app are conflated |

No immediate refactor is implied. The table defines what later experiments
should isolate.

## Technology Status

| Technology | Status |
|---|---|
| Rust | selected working language |
| `wgpu` + WGSL | selected working GPU abstraction |
| `winit` | selected working window/event loop; tablet adapter remains separate |
| `kurbo` | candidate path math |
| `peniko` | candidate color/style vocabulary |
| Vello Hybrid | replaceable conventional-vector backend and baseline; not the canvas foundation |
| Vello Classic | compute-centric benchmark and algorithm reference |
| Vello CPU | CPU fallback, export/reference, and comparison candidate |
| Slug-style analytic coverage | hard-edge research candidate; reference license/NOTICE review required |
| Ciallo-style density/stamps | soft/textured research candidate; source project is GPL |
| custom CPU/GPU stroke expansion | swappable research candidate; measure both sides |
| ADF/SDF cache | unproven research candidate |
| sparse tiled raster | selected canonical first-product paint medium |
| brush reservoir + bounded pickup/deposit | selected first-product mixing structure; equation open |
| local thin-film simulation | post-MVP wet-media research candidate |
| SQLite application file | working first-product native storage/recovery direction |
| OpenRaster/PNG | first layered/flattened interchange candidates |
| SVG | post-MVP retained/vector interchange candidate |
| custom binary/ZIP native file | still possible; deferred until canonical media are known |
| UI toolkit | open |

## Architectural Decision Gates

The first sparse-raster painter does not wait for vector, ADF, or semantic-scale
research. Its gates are:

1. hard ink, textured pencil/chalk, and painterly mixing brush contracts;
2. a default compositing specification;
3. a working pressure-input path on the first desktop environment;
4. sparse tile allocation, bounds, damage, residency, and ownership;
5. per-gesture copy-on-write undo;
6. measured active, navigation, memory, and recovery behavior on Atlas Intel;
7. reproducible correctness/performance traces with controlled cache and
   quality state;
8. transactional native save/autosave/recovery;
9. a minimal usable layers/eraser/color/navigation interface.

Before a post-MVP retained, ADF, semantic-scale, simulation, or alternative
renderer becomes production media, it additionally needs:

1. a canonical source and edit contract;
2. a coordinate/error budget;
3. a real baseline against the existing painter;
4. cache, undo, recovery, and migration behavior;
5. bounded visual equivalence across any backend/LOD switching;
6. a demonstrated artistic or performance benefit that pays for the added
   complexity.

The detailed work is prioritized in the
[research agenda](research-synthesis.md#research-agenda).
