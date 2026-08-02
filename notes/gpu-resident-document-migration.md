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

A live submission must preflight every fallible owner before it reaches
`Queue::submit`. GPU history therefore previews the stable ID of the proposed
entry and the exact redo/oldest-undo IDs that branch and capacity policy would
evict. Atlas history pins have a matching read-only validation path. The live
owner can use that preview to check recovery-spill replacement, journal space,
and mirror capacity while the encoded commit is still discardable; successful
submission must produce the same ID and eviction sequence.

The encoded color-commit token exposes its memento only by immutable borrow.
That permits history preview and source/target readback planning without
releasing ownership before submission. Mirror dispatch likewise validates
reconciler source/target ordering, geometry, capture identity, snapshot budget,
and staging budget without registering the plan. Enqueue after submission
reuses that complete check.

The spill owner accepts that predicted eviction set as one replacement
operation. It first proves that every evicted ID is unique and still tracked,
that the proposed ID and consecutive revision pair are unused, and that the
post-eviction entry count fits. Only then does it release ready or pending
spills and insert the new pending association. Returned evicted entries retain
ownership for auditing, and byte accounting falls only for ready exact pairs.

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

Both halves of a mapped-history handoff are now separately preflightable. The
spill store can validate a borrowed two-sided transition, including identity
and its final retained-byte charge, without taking it. The timeline can preview
the exact record count and bytes a borrowed mirror snapshot would retire
without replacing the base. The live coordinator must pass both checks before
attaching the spill and advancing the base; after that point neither operation
has a remaining data-dependent failure.

Device-loss reconstruction is per logical layer: materialize that layer's
sparse mirror tiles, replay its round and exact-raster records while still
walking the global revision sequence, then discard the temporary CPU history.
This makes interleaved raster layers deterministic. CPU-canonical metadata is
captured immutably at the same target revision; structural-only revisions use
explicit no-raster boundaries, and recovery rebuilds layer order/properties
before recompositing an editable document.

Imports, duplicates, and restoring a deleted layer use one additional forward
transition: an exact sparse whole-layer snapshot. It retains canonical tile
coordinates and shared full-float copy-on-write pixels, replaces the preceding
layer state transactionally, and can seed later semantic strokes. A deletion
needs no raster transition while the layer is absent, but its structural
history must retain this exact snapshot so undo can reintroduce it even after
the mirror base has advanced past the deletion. Capture rejects an active
gesture and non-finite pixels, and journal accounting conservatively charges
the complete logical tile storage even when references are shared.

The pre-map GPU-only undo interval remains an explicit incomplete case rather
than being mistaken for a metadata or layer-snapshot problem. Its CPU spill
representation is an exact two-sided transition: copy the before-blocks from
the preceding immutable mirror and retain the mapped after-blocks under shared
ownership. Every readback plan names that precise source revision, and the
dispatcher constructs the pair before it lets reconciliation advance the
mirror. A separate bounded spill owner registers the stable GPU history ID and
its consecutive source/target revision before readback, then attaches the
returned pair by that exact identity. It deliberately does not maintain a
second undo stack: the GPU history remains authoritative for order, branching,
and eviction. Undo selects the pair's before-side, redo selects its after-side,
and a branch clear or budget eviction removes the same ID from both owners.
Pending, duplicate, stale, mismatched, and over-budget attachments leave both
the association and expensive transition ownership explicit. The live owner
must perform these calls atomically and must not advance the recovery anchor
past an ID whose transition is still pending.

The first live recovery coordinator now owns the timeline and spill index as
one state. A new raster history entry is prepared against the current target
revision using the GPU-history preview's ID and eviction sequence; preparation
changes nothing and returns the complete recovery command on failure. Commit
then applies the already-validated spill replacement and consecutive journal
record together. Undo/redo preparation is intentionally unavailable while the
referenced exact pair is pending. Once ready, undo clones the before patch and
redo clones the after patch into the next forward recovery record. The UI may
queue that discrete action, but it must not submit a GPU swap first and hope
the inverse becomes recoverable later.

Mapped completion uses the same two-phase rule. Preparation requires the
transition target to equal the snapshot revision and its source to equal the
current recovery base. A history completion must name the registered pending
ID; a reconcile-only undo/redo completion is rejected if it would discard a
still-owned history spill. Only after the spill-attachment and base-retirement
previews both succeed does commit attach the pair and advance the mirror base.
A genuinely reconcile-only transition is returned unused rather than retained
as duplicate CPU history.

The first `GpuResidentDocument` owner now closes the new-stroke submission
boundary. It owns the sparse atlas planner, GPU history, mirror dispatcher,
and live recovery coordinator under one mutable authority. Preparation checks
the encoded color token, exact next revision, history ID and evictions, mirror
plan/capacity, recovery journal/spill replacement, and immutable snapshot
capture while the color commit can still be discarded. Only that complete
bundle can reach the owner's submission call. The owner rechecks it immediately
before `Queue::submit`, then acknowledges the target, records the predicted
history entry, advances recovery, and enqueues the exact mirror capture with no
remaining data-dependent failure. A rejected preparation returns both the
encoded color token and semantic recovery command; a rejected final preflight
also returns the command encoder and prepared bundle, whose tokens can be
split for explicit target rollback. This owner is not yet the application's
document path: undo/redo submission, mapped-readback driving, structural
history, presentation/compositing, and application state cutover still have to
join the same boundary.

Undo and redo now enter that owner through a second prepared transaction. GPU
history can name the next undo/redo ID without removing it, allowing recovery
to prove that the two-sided exact spill is ready before history or target state
changes. Only then does the owner move the entry to pending, encode the
buffer/texture exchange, derive the post-swap mirror capture, and validate its
capacity. Any failure before submission discards the command stream, swaps the
resident metadata back, and restores the entry to its original history side.
The prepared value owns the command encoder as well as the target token,
recovery command, and mirror capture, so the final submission cannot mix a
different command buffer into the atomic bundle. Successful submission finishes
the history move, records the exact forward recovery patch, and marks the
resulting mirror revision as reconcile-only; it does not create a second
history spill for the same command. Mirror-purpose ordering is now retained
beside the dispatcher queue.

The owner now drives that queue one bounded batch at a time. A prepared
readback owns its command encoder, submission immediately starts asynchronous
mapping, and polling never waits. On the last batch, the dispatcher first
constructs the exact two-sided transition and exact new CPU snapshot; the
owner then hands both to live recovery before permitting another readback.
Failed handoff retains the snapshot, transition, requested purpose, completion
accounting, and exact error inside the owner for retry, so mapped pixels cannot
fall through an error return. New drawing may continue until the immutable
snapshot budget backpressures it, but reconciliation cannot pass the failed
revision. A delayed `History(id)` completion is resolved dynamically: if that
ID is still tracked it attaches the exact spill, while an ID evicted before
mapping downgrades to reconcile-only and advances the recovery base without
resurrecting dead undo ownership.

GPU target identity is now explicit rather than implied by a per-target serial
number. Every color target receives one process-unique checked ID, and both
commit and undo-swap tokens carry it. Token validation rejects a different
target before considering its pending serial. `GpuResidentDocument`
construction binds the owner permanently to one target ID and layout; prepare,
submit, and discard paths all enforce that binding. Two freshly constructed
targets can therefore no longer accept each other's first token merely because
both local serial counters begin at one. The next API consolidation can move
target encoding behind the owner without carrying this ambiguity through the
cutover.

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

The worker materializes the immutable metadata/raster pair into an editable
CPU document, freezes its copy-on-write tiles as the existing layered
`DocumentSnapshot`, and performs encoding plus atomic replacement there. The
event/render thread does not replay strokes, composite the recovered document,
encode, or touch the filesystem. Mirror materialization installs the immutable
tile references directly into the worker raster; it does not copy every pixel
before the checkpoint encoder reads it.

## Ordered Implementation

1. Separate document metadata/history from the current CPU raster backend
   without changing observable behavior. The first part is complete:
   `DocumentMetadata` is now the backend-neutral immutable description of one
   exact revision, and `GpuResidentDocument` is constructed from and owns that
   complete geometry/layer snapshot. Recovery uses the same type instead of a
   parallel GPU-prefixed copy. Mutable structural commands and their unified
   chronological history still need to move behind the resident owner before
   this item is fully complete.
2. Define timestamped samples, versioned brush recipes, continuous primitives,
   opacity/flow algebra, and a deterministic CPU oracle.
3. Add the page-batched full-float GPU atlas, layer store, and ordered layer
   compositor. Exact bootstrap is complete: an existing sparse layered CPU
   document now seeds both the revision-matched CPU mirror and deterministic
   atlas residents without copying its shared CPU tile storage first. Atlas
   upload is full `Rgba32Float`, validates every key/slot/pixel count before
   issuing writes, and records target resident identity only after validation.
   Ordered GPU composition and live presentation remain.
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
