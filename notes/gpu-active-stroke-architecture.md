# GPU Active-Stroke Architecture

Status: selected architecture and incremental implementation contract,
2026-07-28. Offscreen geometry, sparse full-float GPU tiles, asynchronous
readback, and the CPU commit boundary exist; live presentation/input wiring
and layer-aware composition remain to be integrated.

## Decision

Use GPU-resident sparse scratch tiles as a transaction for the active stroke.
The CPU document remains canonical at gesture boundaries. During a gesture,
the GPU owns only the transient pixels produced by that gesture. A finish
performs one asynchronous, damage-bounded readback and commits all returned
tiles as one existing raster undo transaction. Cancel discards the scratch
tiles without touching the document.

Do not write live brush pixels into `RasterDisplayPipeline`'s flattened visible
composite. That cache has already lost active-layer identity and cannot place a
new mark beneath layers above the active layer, produce a valid layer-local
undo record, or save the active layer.

## Why This Boundary

The measured alternatives establish the boundary:

- the old synchronous CPU `512 px` knife takes approximately 260 ms;
- CPU continuous-contact prototypes took 562–1,074 ms and were rejected;
- one full-float GPU stroke takes approximately 8.35 ms on Apollo;
- plausible incremental batches take well below 1 ms median/p95 on Apollo;
- a full `4096 × 4096` `Rgba32Float` scratch image would reserve 256 MiB and is
  unacceptable for an otherwise sparse document;
- reading back the complete `2048 × 2048` benchmark target took tens of
  milliseconds, so readback cannot occur in the input path.

The transaction preserves the current durable model and moves only the hot,
repeated pixel work to the GPU.

## Ownership State Machine

```text
CPU_CANONICAL
    pen down
        │
        ▼
GPU_DRAWING
    CPU layer remains the exact pre-stroke state
    GPU sparse tiles accumulate new source-over pixels
    cancel ───────────────────────────────► CPU_CANONICAL
        │ pen up
        ▼
GPU_READBACK_PENDING
    scratch remains visible
    save/undo/new stroke do not cross the transaction boundary
        │ mapped tile readback validates
        ▼
CPU_COMMITTING
    one raster gesture source-overs every changed tile
    one document raster edit and one composite damage result
        │ composite upload submitted
        ▼
CPU_CANONICAL
    scratch can be retired
```

If validation, mapping, or device operation fails, leave the CPU layer
unchanged and discard the in-progress mark. Device loss may lose the active
uncommitted gesture, but must not corrupt the last committed document.

## Implemented Commit Contract

`gpu_stroke::commit_source_over_tiles` is the landing point for mapped
full-float scratch tiles:

- every tile must have the exact document tile pixel count;
- coordinates and local damage must fit the document's edge-tile geometry;
- duplicate coordinates are rejected;
- every channel must be finite, RGB nonnegative, alpha in `[0, 1]`, and
  zero-alpha pixels must be exactly transparent;
- all inputs validate before a raster gesture opens;
- exact transparent/no-change input creates no allocation or history;
- changed pixels use premultiplied-linear source-over;
- all tiles commit as one adaptive raster undo entry;
- any mutation error cancels the opened gesture.

Tests prove opaque multi-tile commit/undo, translucent source-over, no-op
behavior, and rejection of malformed length, `NaN`, and duplicate tiles
without document mutation.

The GPU readback must preserve `Rgba32Float`; there is no `f16` conversion
inside this contract.

## Sparse Scratch Storage

Use `128 × 128 × N` `Rgba32Float` texture-array pages, matching the document
tile size. Start with a small page such as 16 layers:

```text
128 × 128 × 16 layers × 16 bytes = 4 MiB
```

Allocate another small page only when a stroke touches more tiles. Maintain a
`TileCoord -> (page, layer)` table and a unioned local-damage rectangle for
each touched tile.

On first contact with a tile:

1. allocate and clear one scratch layer to transparent;
2. create/reuse a one-layer render-attachment view;
3. record the tile coordinate and exact local damage;
4. add one presentation instance.

For the first source-over palette knife, the scratch layer stores only the
transient stroke overlay. It does not need the active layer's prior pixels.
Erasers, pickup, scraping, and true destination-dependent material operations
will require a later mode that seeds scratch tiles from the active layer and
makes those tiles authoritative during the transaction.

### Implemented sparse round trip

`gpu_stroke_target::SparseStrokeTarget` now implements the reusable offscreen
resource:

- retained small texture-array pages with lazy per-stroke tile allocation;
- explicit clear on first use of a reused layer;
- one dynamic, alignment-correct tile-origin/color uniform buffer;
- one growable vertex buffer reused across clipped tile passes;
- full-float fixed-function source-over;
- exact per-tile damage accumulation;
- row-aligned batched texture-to-buffer copies;
- callback-driven `map_async` completion through `PendingStrokeReadback`;
- deterministic tile ordering and production of validated `SourceOverTile`
  inputs;
- a sparse-tile presentation pipeline using the same camera contract as the
  retained canvas, with premultiplied source-over into the surface.

`gpu_sparse_stroke_smoke` renders one rectangle across six `128 × 128` tiles
with a forced four-layer page capacity. On Apollo it retained two pages,
encoded six clipped passes, copied 1,572,864 bytes, committed exactly 24,576
expected pixels, presented exactly the same 24,576 pixels through the sparse
display pipeline, and undid to zero allocated CPU tiles. The test deliberately
uses a small fixture; it proves resource growth, clipping, presentation,
mapping, row packing, commit, and undo correctness rather than final knife
performance.

## Drawing and Scheduling

Each presentation opportunity:

1. consume every semantic input sample currently ready;
2. derive connected `BladeSweep` commands;
3. triangulate only the newly derived sweeps;
4. union their bounds into touched document tiles;
5. upload one compact vertex batch;
6. render that batch once into each intersected scratch tile, clipped by the
   tile attachment;
7. submit one command buffer for the opportunity.

A dynamic uniform offset can supply each render pass's tile origin while the
same vertex batch is reused. This avoids rebuilding tile-local vertices.

Do not wait for a fixed number of samples. The benchmark's batch counts are
workload probes, not quality settings. Display pacing decides when to submit;
contact tolerances decide geometry fidelity.

`gpu_stroke::ContinuousBladeStroke` now owns this backend-neutral live
generation state. It preserves corrected tilt orientation and movement
fallback across input updates, breaks contact at zero pressure, performs
bounded angular subdivision, triangulates only new convex sweeps, and unions
their clipped bounds into deterministic `StrokeTileDamage` entries. Draining
after each input update or only at gesture end produces byte-identical vertex
order, equal sweep counts, and equal unioned tile damage in the regression
fixture. No per-update heap is allocated for subdivision.

## Layer Order and Staged Integration

For a topmost visible active layer, presenting

```text
transient stroke overlay OVER existing flattened composite
```

is mathematically correct for source-over. This is the narrow first live
integration and provides a safe path for the common one-layer document.

If any visible layer exists above the active layer, drawing the overlay above
the flattened composite is wrong. Until layer-aware GPU presentation ships,
that case must use the retained CPU brush rather than silently changing z
order.

The final compositor should render document layers bottom-to-top from their
sparse tile textures and substitute/inject active scratch tiles at the active
layer's exact position. This removes the need for permanent “below” and
“above” CPU composites and scales better than tripling sparse `f32` raster
memory. Layer opacity is applied at composition; scratch pixels remain
layer-local.

Refactor presentation ordering into explicit stages:

```text
canvas background
document layers or current flattened control
active scratch at its valid layer position
brush cursor
UI overlay
```

The existing all-in-one canvas `draw` entry point should remain as a
compatibility wrapper while exposing the split stages.

## Finish, Readback, and Responsiveness

At pen-up:

- submit one copy for each damaged scratch tile into a row-aligned staging
  buffer;
- call `map_async` and keep servicing redraw, hover, and window events;
- retain the scratch overlay until the mapped pixels are committed and the
  resulting CPU composite upload has been submitted;
- initially reject/queue a second stroke, undo, explicit save, and recovery
  snapshot while this short transaction is pending;
- log readback bytes, touched tiles, map latency, CPU commit time, composite
  time, and final upload time separately.

The initial implementation may serialize stroke commits. Double-buffered
scratch transactions are a later optimization only if physical testing shows
pen-up-to-next-pen-down blocking.

## Correctness Gates

- one CPU undo restores every touched active-layer tile exactly;
- cancel performs zero CPU document mutation and zero history mutation;
- empty scratch input creates no tile and no undo entry;
- source-over matches the CPU reference for transparent, translucent, and
  opaque fixtures;
- final pixels and damage are invariant across input batching;
- a layer above the active layer obscures the stroke correctly;
- hidden/zero-opacity active-layer behavior is explicit;
- save/recovery never captures a half-committed GPU transaction;
- device loss retains the last CPU-canonical document;
- no readback or mapping wait occurs while handling an input update.

## Next Implementation Slices

1. [Complete] Add the small sparse texture-array scratch allocator and
   tile-origin dynamic uniform contract.
2. [Complete] Add an offscreen multi-tile render/readback test using the exact
   `SourceOverTile` commit boundary.
3. [Foundation complete] Split canvas/cursor presentation stages and prove the
   sparse scratch display shader. Wire it between canvas and cursor in the app
   only when the active layer is topmost.
4. [Generator complete] Drive connected knife geometry from physical input
   into the scratch tiles. The packet-persistent generator and batching
   invariance tests are complete; app event/render wiring remains.
5. Make finish asynchronous and connect mapped tiles to one document commit.
6. Validate cancel, undo, save blocking, device loss, and exact final pixels.
7. Replace the top-layer restriction with bottom-to-top layer-aware GPU tile
   composition.
