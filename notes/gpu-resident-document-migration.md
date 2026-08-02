# GPU-Resident Document and Continuous Brush Migration

Status: selected implementation direction, 2026-08-01. This decision
supersedes the CPU-canonical gesture-boundary target in
[GPU active-stroke architecture](gpu-active-stroke-architecture.md). The
existing sparse GPU palette-knife transaction remains a useful control while
this replacement is built, but it is not the destination architecture.

## Decision

The interactive renderer will own sparse raster-layer pixels on the GPU.
Fixed-color brushes will generate compact continuous contact primitives,
accumulate a scalar full-precision stroke mask, and commit that mask into the
active layer without a synchronous GPU-to-CPU round trip. Layers will be
composited bottom-to-top on the GPU at their actual document positions.

The CPU retains:

- a deterministic correctness and headless fallback;
- an exact, asynchronously reconciled raster mirror for save/export and
  eviction;
- a compact in-memory semantic journal for edits newer than that mirror;
- pure document metadata and chronological command history.

This is a focused raw-`wgpu` renderer. Vello remains a comparison and a
possible conventional-vector component, not the brush or document
foundation.

## Non-Negotiable Contracts

- Committed layer pixels and saved pixels remain premultiplied-linear
  `Rgba32Float`. No `f16` or normalized-integer substitution is allowed.
- Fixed-color active strokes use scalar `R32Float` coverage or optical
  density; color remains one full-precision gesture value.
- Logical document sparsity remains based on `128 x 128` tiles. A logical tile
  must not imply one GPU render pass.
- First-write undo remains exact and uses `16 x 16` blocks.
- Input handling and pen-up never wait for mapped GPU memory.
- Cancel changes no layer pixels and creates no history command.
- Save and recovery never observe a half-applied GPU transaction.
- Color mixing, pickup, wet media, LOD/deep zoom, and the brush editor remain
  outside this migration.

## Brush Semantics

The first migrated tools are hard round and eraser. One gesture has separate
opacity and flow controls:

- `opacity` is the maximum effect of the gesture;
- `flow = 1` uses the union of continuous contact coverage;
- `0 < flow < 1` accumulates optical density for each traversal;
- `flow = 0` produces no mark.

For a traversal with pixel coverage `c`, the density contribution is:

```text
-ln(1 - flow * c)
```

The accumulated response is:

```text
1 - exp(-sum(density))
```

Paint source-overs and eraser destination-outs once using
`opacity * response`. This makes a single traversal have the requested flow,
lets self-crossing build toward the opacity ceiling, and removes dependence on
input packet count or an arbitrary dab spacing.

Continuous round geometry has a stable prefix and a small replaceable tail.
The prefix and tail use separate sparse scalar masks so new samples can replace
the last join/cap without subtracting from an accumulated mask. Pressure keeps
the current radius response during the first migration; brush-dynamics changes
are a separate artistic decision.

## GPU Storage and Composition

The first physical layout uses lazy `1024 x 1024` two-dimensional atlas pages:

- 64 logical tiles per page;
- 16 MiB per `Rgba32Float` page;
- 4 MiB per `R32Float` page;
- one render pass per touched atlas page, with primitives translated into
  physical slots;
- one global layer-tile pool keyed by `(LayerId, TileCoord)`.

The initial resident color capacity remains 1,024 tiles, matching the current
display cache's maximum. GPU history/staging has a 64 MiB budget and exact CPU
spill; exact reconciliation readbacks are capped at 16 MiB in flight. These
are explicit configuration values and reported counters, not hidden allocator
behavior.

Presentation is one ordered surface pass:

```text
paper / pasteboard
    -> visible layers bottom-to-top
    -> active layer with its transient paint/erase mask applied locally
    -> cursor
    -> UI
```

An eraser therefore changes only the active layer before that layer is
composited. It never erases the already flattened surface or layers below.

## Revision, Undo, and Persistence Model

Every committed raster or structural command advances `DocumentRevision`.
The implementation tracks three related revisions:

- interactive: latest GPU-visible document state;
- mirrored: latest exact CPU raster mirror;
- saved/recovery: latest atomically persisted snapshot.

GPU raster history stores exact before-blocks plus tile allocation/occupancy
metadata. Undo and redo exchange exact GPU blocks and then schedule ordinary
asynchronous mirror reconciliation.

Until the CPU mirror catches up, an in-memory journal retains complete
semantic commands, including structural commands and undo/redo direction. On
device loss, the application reconstructs from the exact mirror plus that
journal. The journal is not synchronously written per stroke; the existing
approximately two-second autosave crash window remains.

"Complete" is a stronger condition than naming an operation. In particular,
`Undo(command_id)` is not replayable from a mirror newer than that command's
pre-state: the base contains the painted result but does not contain the pixels
that undo must restore. Every post-base journal record must therefore be a
forward-applicable transition from the immediately preceding revision. Round
paint/erase can retain its versioned recipe and continuous path commands.
Undo/redo that reaches across the mirror base must instead retain either the
exact resulting blocks, or an older exact replay base plus the referenced
semantic command and inverse history. Structural deletion and imported raster
content have the same ownership requirement. The implementation must choose
and budget one of those representations before claiming device-loss recovery;
a direction-only journal is explicitly insufficient.

The selected raster representation is an exact forward patch for the state
*after* an undo or redo. It owns the mapped `Rgba32Float` block pixels and the
logical present/absent tile result, is canonicalized by layer and tile, and can
apply to the immediately preceding CPU recovery state without consulting the
GPU history stack. A complete multi-batch mirror plan is required before the
payload is accepted. The asynchronous interval still matters: until those
bytes have mapped and the recovery base has advanced atomically, recovery must
retain the older replay base and inverse-capable history. A GPU-only capture is
not device-loss protection.

The base/journal owner performs that advancement as one checked ownership
handoff. It rejects mismatched geometry, regression, unknown revisions, and a
mirror ahead of the journal before replacing its immutable base or retiring
any record. Snapshots retain both sides of the boundary with shared ownership,
so background save/recovery work cannot race a later handoff.

Device-loss reconstruction is per logical layer: materialize that layer's
sparse mirror tiles, replay its round and exact-raster records while still
walking the global revision sequence, then discard the temporary CPU history.
This makes interleaved raster layers deterministic. CPU-canonical metadata is
captured immutably at the same target revision; structural-only revisions use
explicit no-raster boundaries, and recovery rebuilds layer order/properties
before recompositing an editable document. Imported/deleted raster ownership
and the pre-map undo interval remain explicit incomplete cases rather than
being mistaken for metadata problems.

A save request captures metadata at revision `R`, asynchronously copies the
exact dirty GPU blocks needed for `R`, and writes the existing layered
`.sketchpad` format. Drawing may continue at `R + 1`. Saving `R` clears the
modified marker only if `R` is still the current interactive revision.

Background revision work is limited to one active payload plus one coalesced
newest pending payload. Completion is serial-token checked, and the immutable
payload returns to the caller on both success and failure. This bounds snapshot
retention while preserving a newer request that arrives during I/O. Duplicate
or older requests add no work; a successful stale write remains a valid file
but cannot mark the live document clean.

## Ordered Implementation

1. Separate document metadata/history from the current CPU raster backend
   without changing observable behavior.
2. Define timestamped samples, versioned brush recipes, continuous primitives,
   opacity/flow algebra, and a deterministic CPU oracle.
3. Add the page-batched full-float GPU atlas, layer store, and ordered layer
   compositor.
4. Add stable/tail round masks, live paint/erase presentation, GPU commit, and
   exact block undo.
5. Add asynchronous CPU reconciliation, revisioned snapshots, device-loss
   journal replay, and GPU color sampling.
6. Make the GPU document path the default. Temporarily hide the unmigrated
   natural brushes rather than adding CPU/GPU ownership stalls.
7. Restore flat and knife as connected oriented ribbons, pencil as a
   grain-modulated density ribbon, and bristle as bounded strand ribbons.
8. Remove each corresponding dab implementation after its replacement passes
   correctness, quality, and performance gates.

## Acceptance and Measurement

Automated coverage must include packet invariance, dots, sharp turns, pressure
tapers, self-crossing, opacity ceilings, repeated flow, partial erasing,
arbitrary layer order, cancel, exact undo/redo, empty-tile reclamation,
out-of-order readbacks, save-revision races, and simulated device loss.

The renderer must report primitive count, atlas instances, touched tiles and
blocks, render passes, full-float and mask bytes, GPU memento bytes, journal
depth, and reconciliation traffic. A regression test must prove that pass
count follows touched atlas pages rather than touched logical tiles.

No performance run is authorized while the development machines are in use.
During that window, only remote Atlas compilation, linting, and correctness
tests may run. At the next approved benchmark window, the migrated wide-brush
path must demonstrate at least a ten-times improvement over the legacy CPU
path without worsening the declared supersampled quality corpus. Failure of
the raster-atlas path or unavailable full-float blending triggers the already
isolated `16 x 16` full-`f32` compute-microtile fallback experiment; it does
not permit a precision reduction.
