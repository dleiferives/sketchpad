# Implementation Status

Status: live implementation ledger, 2026-08-02. This note records what the
current executable actually does. Product intent remains in
[first-usable-product.md](first-usable-product.md); research claims and future
possibilities belong in the subject notes.

Live cutover checkpoint, 2026-08-02: on hardware with full-float blending and
the selected 128 px tile geometry, the application now bootstraps its layered
document into `GpuResidentDocument` and makes that owner authoritative for
interactive pixels. Hard-round paint and erase feed continuous round commands
into the resident mask, preview in ordered layer composition, and commit
directly to full-float atlas pages without a pen-up readback. Surface frames
draw paper/pasteboard, the resident layer compositor, cursor, and UI in order.
Exact undo/redo uses the resident history transaction once its asynchronous
two-sided recovery spill is ready. The event loop advances one bounded mirror
readback batch at a time without waiting.

Resident autosave freezes the current metadata/recovery timeline and sends CPU
replay, composition, checkpoint encoding, and atomic replacement to the
bounded background worker. A later revision cannot be marked saved by an older
completion. Explicit save and close-time recovery use the same exact snapshot;
PNG export reconstructs that revision instead of reading the stale legacy
composite. Opening a document now bootstraps a complete replacement resident
owner before publishing either the new CPU metadata or GPU state, so failure
leaves the current canvas untouched. Active-layer selection is now resident
metadata too: panel clicks and relative-layer keybindings change the layer used
by the next GPU stroke without advancing revision or touching CPU pixels. The
layer panel and title read resident metadata, so legacy selection is not
shadow-mutated.

Visibility, opacity, and layer ordering now form the first live revisioned
metadata slice. Raster mementos and reversible metadata edits occupy one
bounded chronological GPU history, so a stroke followed by a visibility
change still undoes in user order. Metadata entries carry no atlas pins or
recovery spill; when their branch/capacity insertion evicts raster entries,
only those raster IDs are removed from the exact spill owner. Metadata
undo/redo applies the same checked edit in reverse/forward direction and emits
an explicit no-raster recovery boundary. The CPU mirror registers these
pixel-identical revisions without scheduling a GPU copy. If one is queued
behind a raster readback, dispatch retains the shared exact snapshot at the
raster boundary before advancing across the metadata revision, preventing a
later revision label from being paired with an earlier raster transition. The
existing panel and key commands now call this resident path.

Empty layer creation is live through the same presence-edit representation.
It inserts above the active layer, selects a monotonic stable ID, advances one
metadata/recovery revision, and allocates no raster tile or mirror capture.
Undo removes that metadata and redo restores the same ID; selection-only
changes between creation and undo do not invalidate the command. Branching
after the undo allocates a newer ID rather than reusing the unreachable one.

Layer deletion is live and uses the opposite side of that presence edit. The
layer disappears from composition immediately while its sparse atlas and CPU
mirror payload remain dormant for exact undo. History entries now expose the
layer identities they retain. When branching or bounded-history eviction
leaves an absent layer with no reachable raster or metadata command, the owner
releases its atlas allocations and schedules CPU-mirror tile retirement. The
mirror performs that retirement immediately when idle or after all previously
registered raster revisions reconcile, so cleanup cannot invalidate an
in-flight exact transition. Deleting the last layer fails without revision,
history, or storage mutation.

Layer duplication is live as the first payload-bearing structural resident
transaction. It prepares the inherited metadata, bounded history eviction,
exact whole-layer recovery command, ordered CPU-mirror clone, and sparse atlas
capacity before publishing the new identity. Initialized source tiles are
copied entirely on the GPU; empty/uninitialized atlas allocations are not
materialized in the duplicate. The CPU mirror applies the clone only after all
older raster revisions are exact, then shares immutable tile allocations with
the source until either layer changes. Recovery independently retains a
canonical full-float snapshot for the new identity, so device loss does not
depend on the GPU copy surviving.

WebGPU does not permit a texture-to-texture copy when source and destination
name the same texture, even for disjoint atlas slots. The first hardware smoke
caught this on Intel UHD Graphics 630. The live path therefore reuses one
tile-sized transient `Rgba32Float` scratch texture and encodes
`source slot -> scratch -> destination slot` for each initialized tile. This
doubles copy traffic but keeps it GPU-local, bounds temporary storage to one
tile, works within and across atlas pages, and avoids a CPU upload/readback.
The encoded token retains the scratch texture until submission and rolls back
logical resident identities if discarded. The hardware smoke compares every
row of the source and destination tiles, reconstructs the duplicate after
simulated device loss, and exercises metadata undo/redo.

PNG layer import is live in resident mode. Decoding still happens in the
established bounded CPU codec path, which produces centered premultiplied
linear `Rgba32Float` sparse tiles. The resident owner then preflights geometry,
history eviction, atlas capacity, exact whole-layer recovery, and an ordered
mirror snapshot before issuing any upload. Each allocated tile is uploaded
once with `Queue::write_texture`; the same immutable pixel allocation is
shared by the recovery command and is installed into the mirror when that
revision reaches the front of the reconciler. No GPU readback or legacy
`Document` mutation occurs. After the writes there are no remaining fallible
publication steps. The hardware smoke forces the import to grow a second atlas
page, verifies the exact GPU pixel, reconstructs it after simulated device
loss, and exercises presence undo/redo.

During this transition, natural brushes, layer rename, and color picking are
intentionally unavailable in
resident mode. The legacy `Document` remains immutable fallback data; the
application never treats it as a second writable pixel authority.

The reversible metadata edits are prepared without mutation, require the exact
expected side plus exact next revision when applied, reverse through the same
value, preserve stable active identity across moves, and reject invalid/stale
state transactionally.

Selected architecture, 2026-08-01: replace the CPU
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
historical inventory below records the incremental proofs that led to the live
boundary above.

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
It also retains the unioned conservative tile-local damage for every active
slot across incremental batches. Submission keeps that damage and discard
restores the exact prior union, providing the first-write block inventory
needed by GPU undo without expanding a touched region to its whole tile.
The first non-live color commit stage is connected as well. Lazy
`Rgba32Float` pages consume the ended full-flow mask once per affected slot;
fixed-function premultiplied source-over paints and destination-out erases in
one pass per touched page. A physical slot is cleared only on its first color
write or after its logical `(LayerId, TileCoord)` occupant changes. The target
rejects active masks, mask batches not yet acknowledged as submitted,
optical-density flow, and reuse of its own queue-written buffers before the
encoded commit is submitted or discarded. Live presentation, optical-density
accumulation, document-history sequencing, allocation/occupancy restoration,
and application cutover are not connected yet.

Exact GPU pixel undo now has a deterministic copy planner, backing buffers,
and a non-live bounded history owner. Each conservative tile-local damage
rectangle rounds outward to `16 x 16` full-float blocks. One block is exactly
4 KiB; its 16-pixel `Rgba32Float` row is exactly WebGPU's 256-byte copy-row
alignment.
Adjacent selected blocks for one tile coalesce into one rectangular copy
region: a full 128-pixel tile remains 64 accounting blocks / 256 KiB but needs
one texture-buffer copy command rather than 64. Plans are ordered by physical
page/slot, carry stable logical occupants, use checked size arithmetic, and
reject non-block-aligned tile layouts, out-of-tile damage, or conflicting
occupants before any GPU mutation.

An undo-enabled color commit captures those regions before the paint pass.
Existing residents copy exact `Rgba32Float` pixels into a private memento;
new residents record zero directly in that buffer rather than reading
undefined texture memory. The memento becomes usable only when its matching
encoded-commit token is acknowledged after submission. Undo copies current
pixels to one reusable scratch buffer, copies the memento into the document,
then replaces the memento with the prior current pixels. The same operation is
therefore exact redo on its next invocation. A pending commit or swap blocks
shared-resource reuse, and a swap fails before encoding if any physical slot
no longer names the recorded logical resident. GPU history budgeting, slot
pinning, and swap sequencing now exist below the application boundary;
revision commands, CPU spill, nonempty-content reclamation, and live history
integration remain.

Mementos also exchange initialized-resident metadata, not only pixels. The
first commit to a logical tile records an absent prior resident; undo restores
transparent blocks and removes that logical color occupant while its history
pin keeps the physical atlas slot reserved. Redo restores both pixels and the
same logical identity. Existing-resident edits exchange `present -> present`.
Metadata changes are staged with a serial-checked encoded-swap token: submit
keeps them, while discard restores both the document map and the memento's
direction before another operation may begin.

The sparse atlas now has checked reference-counted history pins as the first
part of that layer. A memento can pin its exact logical resident/physical-slot
pair; neither single-tile nor whole-layer release can recycle a pinned slot.
Whole-layer release validates every key before mutation, so encountering one
pin cannot partially release its siblings. The last unpin restores ordinary
slot reclamation. The non-live GPU history owner now creates one pin per
memento resident, keeps pins while an entry moves between undo and redo, and
releases them on branch clearing, budget eviction, or explicit history clear.

That owner defaults to the declared 256 entries / 64 MiB. A new command clears
redo and returns its evicted mementos for the future exact CPU-spill path; byte
or entry pressure evicts oldest undo commands deterministically. A memento
larger than the entire budget is returned unchanged rather than silently
dropping undo. Undo and redo move through an explicit pending state: successful
GPU submission finishes the move, while encoding/submission discard can put
the exact command back on its original side. Recording during a pending swap
is rejected. Pure tests cover branch clearing order, both budgets, oversize
rejection, and finish/cancel behavior.

GPU history now also exposes a non-mutating commit preview for the live-owner
cutover. It reports the next stable history ID and the exact IDs that a new
branch, entry limit, or byte limit would evict, without clearing redo or
changing accounting. The atlas separately validates every prospective
history pin without incrementing it. Actual recording reuses the same core
preview, asserts that its committed ID/eviction sequence agrees, and still
returns the memento unchanged on any rejected preflight. This lets the future
owner check history, spill replacement, recovery journal, and mirror capacity
before submitting GPU work rather than discovering a recoverability failure
after pixels are visible. Pure tests cover combined redo clearing, oldest-undo
eviction, stable ordering, pin mismatch, and absence of preflight mutation.

The encoded GPU color-commit token now lends its still-owned memento for that
preflight. History can derive its stable ID/eviction preview and mirror code can
derive exact source/target batches before target submission acknowledges the
token. Mirror reconciliation and dispatch now expose non-mutating full-plan
checks in addition to raw byte-capacity checks; they validate revision source,
target ordering, tile geometry, batch identity, capture identity, snapshot
budget, and staging budget without registering work. Enqueue reuses the same
check after capture submission. Tests prove the preview leaves reconciler and
dispatcher queues empty.

The recovery-spill owner now previews and commits the corresponding ID
replacement as one operation. It rejects repeated or untracked eviction IDs,
duplicate new identity, nonconsecutive revisions, and post-eviction entry
pressure before changing either index. A successful replacement removes the
exact pending/ready entries in GPU-history order, reports the bytes freed by
ready pairs, returns ownership of every removed spill, and installs the new ID
as pending. Tests prove that preview is non-mutating, ready-byte accounting is
released exactly once, and malformed eviction sets cannot partially branch.

The first non-live `GpuLiveRecovery` coordinator now combines that spill
index with the forward recovery timeline. New raster history is a two-phase
operation: preparation validates the consecutive journal record and predicted
GPU-history replacement without mutation, then commit rechecks the source and
updates both owners. A stale prepared token returns its complete recovery
command and cannot add a second revision. Undo/redo preparation requires a
ready two-sided spill, chooses before for undo or after for redo, and owns that
exact forward patch before any GPU swap needs to be submitted. A pending spill
therefore produces an explicit queueable error with no revision change. Pure
tests cover successful commit, branch/budget replacement, stale tokens,
returned command ownership, pending inverse rejection, and exact undo-side
selection.

Mapped mirror completion has now joined the same coordinator. Its prepared
handoff owns both the immutable CPU snapshot and exact two-sided transition,
requires matching targets and the current base as source, and previews journal
retirement before mutation. Original history mapping must attach to the exact
registered ID. Undo/redo or evicted-history mapping may be marked
reconcile-only, but that path refuses to discard any transition whose history
ID is still tracked. Commit rechecks the complete boundary, then attaches the
required spill and advances the base/journal together; a reconcile-only pair
is returned to the caller as unused ownership. Tests cover atomic history
attachment plus retirement, safe reconcile-only disposal, wrong-purpose
rejection, and preservation of the snapshot, transition, journal, base, and
pending spill on failure.

The first non-live `GpuResidentDocument` owner now coordinates an ordinary
GPU raster commit across the atlas, exact GPU history, asynchronous mirror,
and live recovery state. Before submission it derives one checked next
revision, previews the stable history ID and exact eviction sequence, validates
the mirror plan and budgets, prepares journal/spill replacement, and encodes an
immutable post-commit snapshot in the same command stream as the color write.
The final call repeats every mutable-state check immediately before its sole
`Queue::submit`; after submission, target acknowledgement, history insertion,
recovery advancement, and mirror enqueue reuse those proven identities. Both
failure stages preserve the opaque color/recovery ownership needed to discard
the encoded transaction. Constructor tests prove that every subsystem starts
at one revision with the configured budgets and that invalid history or mirror
limits fail at the owner boundary. The live application still owns the legacy
CPU document and does not call this owner; the remaining cutover work is to
coordinate undo/redo and mirror polling, add layer/presentation ownership, and
bind the completed transaction path to input and application state.

Exact GPU undo/redo has now entered the same non-live owner. History exposes a
read-only next-swap identity, and live recovery exposes a reusable final
preflight for its prepared forward patch. The owner refuses a swap before
history mutation when the matching two-sided spill is still mapping. Once
ready, it moves the entry to pending, encodes the GPU block exchange, plans and
captures the post-swap mirror revision, and retains the encoder inside one
opaque prepared value. Planning or capture failure restores both target
resident metadata and the history side while dropping all unsubmitted GPU
commands. Final submission rechecks target token, pending history identity and
direction, recovery revision, and mirror capacity before one queue submission;
it then finishes history/recovery and enqueues reconciliation as one boundary.
Ordinary commits and swaps also retain an ordered mirror purpose (`History(id)`
or `ReconcileOnly`) beside each capture.

That bounded mirror driver now exists inside `GpuResidentDocument` and is
driven by the application event loop. It owns the encoder for one oldest-
revision staging copy, acknowledges submission, starts
callback mapping without waiting, and exposes polling separately. Incomplete
batches report their exact bytes and applied revisions. A last batch creates
the exact transition/snapshot handoff and commits it to recovery before the
next readback can start. Any rejected handoff remains fully owned inside the
document, including mapped pixels and completion metadata, and can be retried;
the stored error is observable for policy/UI reporting. Purpose resolution
also handles history eviction during long mapping: a still-tracked ID receives
its two-sided spill, while an already-evicted ID becomes reconcile-only. A pure
regression test covers both sides of that lifetime change. The loop polls the
device without waiting and schedules another opportunity while work remains.

The remaining external color target is now identity-bound before it is moved
behind the owner API. Each `GpuDocumentTarget` receives a process-unique ID;
opaque color-commit and undo-swap tokens carry that ID and fail before serial
validation on another target. A `GpuResidentDocument` is constructed from one
specific target and checks the same identity on preparation, final submission,
and explicit discard. Pure tests cover unique allocation, cross-target token
rejection, and owner mismatch reporting. This closes the same-layout/same-
serial resource aliasing hole while keeping the physical target-ownership
refactor as a separate reviewable change.

Resident document metadata now has one backend-neutral immutable contract.
`DocumentMetadata` captures exact geometry, revision, active layer, ordered
layer identities, names, visibility, and opacity without retaining raster
pixels. Device-loss recovery and the resident owner both use this same type;
the old GPU-specific metadata structs are compatibility aliases only. A
resident owner can no longer be created from dimensions and revision alone,
and rejects metadata whose logical tile size differs from its atlas. Raster
commit and undo/redo submission advance the owned metadata revision at the
same post-submit boundary as history, recovery, and mirror ownership. This is
the first authority-consolidation slice; structural mutations and their
chronological history remain on the legacy `Document` path for now.

The resident owner can now bootstrap an existing exact layered document. It
captures one immutable CPU mirror at the document revision, deterministically
allocates every sparse `(LayerId, TileCoord)` in layer/row order, and uploads
the shared tile pixels into lazy `Rgba32Float` atlas pages. The target validates
empty ownership, unique keys and slots, layout bounds, and full tile payloads
before issuing any queue write; its logical resident map is installed only
after that preflight. Atlas-capacity failure discards the not-yet-published
owner and leaves the target uninitialized. The Atlas Intel UHD 630 hardware
smoke bootstrapped two layers/three tiles, copied the page back, and found the
premultiplied full-float pixels plus untouched transparency bit-exact. This is
a correctness result, not a timing measurement. Presentation still reads the
legacy flattened CPU cache.

An isolated ordered resident compositor now draws the atlas without flattening
the document. Its instance plan is layer-major and page-minor: page grouping
reduces state changes only within one layer, so a later layer can never be
moved below an earlier one. Hidden and zero-opacity layers emit no work;
partial document-edge tiles retain full physical slots but clip their logical
quad; each premultiplied sample is scaled by layer opacity and source-overed.
Preparation validates the bound target identity, atlas layout, and every
logical-key/physical-slot association before writing instance data. The Atlas
Intel UHD 630 hardware smoke composited overlapping pixels from two layers into
an `Rgba32Float` target and matched the CPU oracle exactly. The smoke now also
checks exact initial atlas upload and mirror ownership for three sparse tiles.
The same compositor now evaluates the active full-flow `R32Float` stroke mask
at the active layer's ordered position. Paint source-overs into that layer;
eraser destination-outs from that layer before lower and upper layers are
combined. Untouched layers still use the direct color-page path. A transient
tile may use an initialized committed base or transparent base, so the first
mark in an empty logical tile is visible without clearing or committing its
`Rgba32Float` slot. Preparation rejects a wrong active layer, stale atlas slot,
unsubmitted mask batch, mismatched target, and optical-density flow rather
than silently presenting different semantics. The Atlas Intel UHD 630 smoke
matches the CPU material oracle for paint into a newly allocated transparent
active-layer tile and for an eraser revealing the committed lower layer. It
also proves that preview does not initialize the color target or create a
history mutation.

That smoke exposed and now records an important cancel boundary: a newly
allocated preview tile must be released from the sparse planner when the
stroke is discarded. Leaving it allocated caused the next unrelated stroke
to fail resident validation because the key was neither committed nor active.
The reusable `GpuResidentRoundStrokeEngine` now owns that boundary. It creates
the mask pipelines once, acquires a serial resident-document guard at begin,
submits validated incremental command batches, retains only allocations that
those batches created, and is the sole path to transient composition. Failed
mask encoding restores the scheduler and releases that batch's allocations.
Cancel ends the mask and returns every provisional slot without touching the
color target, revision, or history. A mismatched token, layer, target, or
layout fails before atlas mutation, and ordinary commit/undo preparation is
blocked while the guard is held.

The terminal commit path requires a complete semantic round path, constructs
the versioned recovery command, ends the mask, captures exact first-write undo
blocks, and passes the color work through the existing resident preflight and
single-submit boundary. Success advances metadata, GPU history, live recovery,
and asynchronous mirror capture together; every pre-submit failure discards
target metadata and releases provisional allocations. There is no fallible
acknowledgement after `Queue::submit`. The Atlas Intel UHD 630 smoke now uses
this owner rather than a duplicate planner: it paints into a provisional tile,
proves preview-only residency, cancels and observes reclamation, starts a
second eraser stroke, previews the lower layer, commits it as history entry 1,
and reads back the committed result exactly. The application now uses this
path for hard-round paint and erase; the final dirty-updated exact stable
composite cache remains to be built. Command batches currently submit
immediately; display-
opportunity coalescing remains a later event-loop policy above this owner.

The resident owner can now freeze an exact application-facing snapshot at its
current interactive revision without waiting for the CPU mirror to catch up.
The immutable value pairs the same `DocumentMetadata` revision with the live
recovery timeline's exact mirror base and forward journal. Materializing layer
rasters, recomputing their visible composite, checkpoint encoding, and atomic
filesystem replacement can therefore happen away from the event/render
thread while later GPU strokes continue. The Atlas hardware smoke freezes the
state immediately after a resident eraser commit—before driving mirror
readback—and CPU recovery reproduces both its revision and revealed lower-layer
pixel exactly.

`GpuCheckpointWorker` is the first consumer of that boundary. It permits one
active background save plus one coalesced newest pending revision. A third
request replaces only the pending payload; it never interrupts or aliases the
active snapshot. Completion compares the task's retained live revision with
the then-current interactive revision, so only a successful save of that exact
revision may clear dirty state. Encoding, recovery replay, composite rebuild,
and disk I/O run on the worker. Save failure and worker panic are explicit
results; failure leaves dirty state claimed by no revision, and the worker can
accept later work. Dropping the owner joins any active write rather than
detaching filesystem mutation past application shutdown. Tests prove
`R1 active / R2 pending / R3 replaces R2`, stale-success behavior, exact final
file contents, and repeated failure against a non-directory path. The live
resident recovery path now uses this worker. The current
`.sketchpad` file format deliberately begins a new session at revision zero
when reopened, so live saved-revision identity remains task metadata rather
than a serialized file field.

Asynchronous CPU reconciliation now reaches an exact sparse CPU mirror.
Each revision reuses the exact undo-region identities and packs initialized
`Rgba32Float` regions into explicitly bounded readback batches. The default
staging-buffer cap is 16 MiB. Regions larger than that cap split only at
16-pixel block-row boundaries, so every buffer row remains naturally
256-byte aligned; a synthetic 2,048-pixel full-tile region splits into four
exact 16 MiB batches. Ordinary 128-pixel tiles coalesce until the cap.
Logically absent residents become zero-byte metadata records instead of
needless transparent pixel copies.

The GPU boundary validates atlas layout, logical resident identity, physical
slot, color-page existence, and device buffer limits before encoding any copy.
At the commit boundary it copies every batch into immutable GPU-only snapshot
buffers in one command stream. Subsequent drawing can therefore mutate the
live atlas without changing a pending revision. Snapshot batches later pass
through one bounded `MAP_READ` staging buffer at a time. Capture and staging
submission have explicit acknowledgement, and an unsubmitted staging copy can
be discarded back to the front of the queue without losing its snapshot.
Mapping is callback-driven and polled without imposing a wait in the API;
mapped full-float bytes become owned patch regions and the buffer is unmapped
on either successful decoding or a terminal decode error.

A revision-ordered reconciler accepts completed batches in any order but
applies only a complete oldest revision. Its CPU store remains sparse, removes
logically absent tiles, and uses `Arc` copy-on-write tiles so save/export can
hold an immutable exact revision while later reconciliation proceeds.

Pure checks cover packing, splitting, byte/block totals, absent residents, a
budget smaller than one block, out-of-order completion, malformed-patch retry,
sparse removal, and snapshot isolation. The Atlas Intel UHD 630 correctness
smoke additionally captured the eraser transaction as two 8,192-byte GPU
snapshots, then undid the eraser in the live atlas before either snapshot was
staged for the CPU. Sequential bounded mapping still reconstructed the exact
16,384-byte erased revision atomically and found the expected premultiplied
`[0.075, 0.15, 0.3, 0.375]` center in both the mapped patch and revisioned CPU
tile. This proves snapshot isolation across a later GPU mutation. It is a
correctness result, not a timing measurement.

A global reconciliation dispatcher now owns ordered captures and their CPU
reconciler. Its defaults are a 64 MiB immutable-snapshot budget and one 16 MiB
staging buffer. Capacity can be checked before allocating a revision capture;
enqueue repeats the check transactionally and returns ownership of a rejected
capture. Only the oldest revision may prepare a staging copy, so multiple
pending captures cannot multiply mapped-transfer memory. Completed batches
release their exact snapshot-byte charge, the final batch retires its capture,
and the dispatcher reports which revision became atomically visible. Plan
geometry is now rejected during registration, before mapped bytes can be
consumed, including edge-tile bounds and document extent.

The Atlas isolation smoke runs this dispatcher at exact 16,384-byte snapshot
and 8,192-byte staging limits. It rejects capacity for another capture while
the first is resident, never exposes more than one 8,192-byte staging batch,
and returns both counters to zero after reconciliation. The live application
still does not enqueue captures or bind mirror revisions to save/export.

The recovery journal now has a bounded, payload-generic revision core. It
requires exactly one record for every interactive revision, offers capacity
preflight before a GPU commit, never evicts an unreconciled record, and returns
the exact command if recording fails. Defaults are 4,096 entries and 64 MiB.
Mirror acknowledgement retires only records through an exact known revision
and releases their declared bytes. `Arc`-backed snapshots retain a stable base,
target, and command sequence while the live journal advances. Pure tests cover
gap rejection, transactional byte exhaustion, partial retirement, snapshot
isolation, and deterministic replay of a small state machine.

Full-flow round paint and erase now have the first typed journal payload. It
retains the layer, versioned recipe, canonical continuous path commands, and a
checked retained-byte charge. Construction returns command ownership on
failure and rejects empty or incomplete paths, timestamp regression, no-op
material, and optical-density flow that the current GPU commit path cannot yet
execute. Its deterministic CPU oracle evaluates the same variable-radius
capsule signed distance at pixel centers, applies the shared premultiplied
material algebra, rolls back on error, and does not allocate a tile when an
eraser crosses empty space. Two journaled strokes replay in revision order in
the pure tests.

The Atlas Intel UHD 630 smoke compared all 65,536 pixels after the same paint
sweep and eraser dot ran through the GPU and CPU recovery paths. Storage and
the GPU readback stayed exact `Rgba32Float`, but transcendental capsule math was
not bit-identical: 8 pixels / 32 channels differed, with maximum absolute
error `4.7683716e-7`; every channel passed the explicit `1e-6` oracle bound.
The CPU replay is therefore a deterministic semantic fallback, not a bit-exact
replacement for an asynchronously mirrored GPU revision.

Undo/redo now has its selected CPU payload representation below the
application boundary. A completed mapped mirror revision can be consumed as a
single-layer exact raster command only when every planned batch is present
exactly once and revision, byte count, logical region shape, resident state,
pixel count, finiteness, 16-pixel block alignment, and non-overlap all pass.
The command canonicalizes physical readback order into logical tile order,
owns the full-float pixels, has a checked journal charge, and returns expensive
input ownership on validation failure. Replay prepares every affected tile
before mutation, copies exact channel bits while preserving untouched pixels,
handles a resulting absent tile explicitly, reports actual changed-pixel
damage, and creates no accidental CPU undo history.

Mapped patch regions and the exact recovery command now share their immutable
pixel slices by `Arc`. Creating a journal snapshot, recovery owner, or future
CPU history spill therefore copies only region metadata; it does not deep-copy
the mapped full-float blocks. Replay still performs the one required copy into
a mutable destination tile.

The CPU spill value now pairs both exact sides of one reconciled raster
revision. Its before-side is captured from the immutable CPU mirror over the
same block-aligned region shape; its after-side takes ownership of the mapped
GPU result. It validates base ordering, tile geometry, complete batch identity,
finite pixels, and byte accounting while returning the mapped batches on
failure. First-paint absence is an explicit before-state. Pure replay tests
apply after then before and recover exact present pixels or sparse absence.

Readback plans now name both their exact source and target revisions. The
reconciler rejects gaps or a false source before registering work, and mapped
patches reject non-finite channels before changing the sparse CPU mirror. The
dispatcher retains each accepted plan plus shared mapped batches, constructs
the two-sided spill before advancing the mirror, and returns it only with the
completed revision. The GPU correctness smoke now contains an after/before
oracle for that returned spill, though this slice was compile-checked rather
than run against hardware.

The non-live recovery-spill owner now binds those transitions to the matching
stable GPU history IDs without duplicating undo/redo stack state. Registration
requires one exact consecutive source/target revision and rejects duplicate
IDs, duplicate revisions, and capacity pressure before mutation. Mapping can
complete out of registration order; attachment resolves by target revision,
rechecks the source, and returns the full-float transition unchanged on stale,
duplicate, mismatched, or over-budget input. A ready ID exposes its exact
before-side for undo and after-side for redo. GPU-history branch clearing and
eviction can remove that same ID, including while mapping is pending; a late
result then remains explicitly untracked. The default 256-entry / 160 MiB CPU
budget covers twice the default 64 MiB GPU pixel budget plus bounded
tile/region metadata, and accounting drops only when GPU history relinquishes
the ID. Pure tests cover out-of-order attachment, direction selection,
transactional failures, explicit eviction, and both limits.

That work also exposed and fixed a partial-edge invariant: a logical canvas
whose dimensions are not multiples of 128 may legitimately produce a padded
16-pixel GPU undo block. Reconciliation now accepts padding inside the
physical tile, copies only its logical canvas intersection, and leaves padded
CPU pixels transparent. Pure tests cover out-of-order mapped batches,
duplicate rejection with retained ownership, exact partial replacement,
untouched pixels, absent-tile removal, and a 250-pixel canvas edge.

The payload set is intentionally not yet declared complete for live drawing
recovery. The app-level owner must register GPU history, journal command,
readback, spill attachment, anchor advancement, undo/redo, branch clearing,
and eviction as one checked state transition. Until the original history
spill has mapped, an undo cannot use it as an inverse; the system must retain
the older exact replay anchor and complete inverse-capable history, or queue
that undo. A pending GPU-only capture would disappear with the device.

The first anchor handoff owner now makes the exact CPU base and its post-base
journal one state boundary. Recording delegates to the same transactional
entry/byte preflight. Advancing to a newer mirror snapshot first verifies
canvas and tile geometry and asks the journal to retire through that exact
revision; only then does it replace the base. Failure returns the candidate
snapshot unchanged and cannot retire a command. Its immutable snapshot clones
the base's copy-on-write tile references and the journal's command `Arc`s, so a
save or device-loss worker sees one stable base/target pair while the live
timeline advances. Pure tests cover partial retirement, geometry/ahead-of-log
failure, returned ownership, and snapshot survival after full retirement.

Mirror-anchor retirement and exact-spill attachment now have matching
read-only previews for the live coordinator. A journal acknowledgement reports
the precise record count and bytes it would retire; the timeline layers canvas,
tile, monotonic-base, and journal-order checks over that preview. Separately,
spill attachment validates the registered history ID, source/target revision,
pending state, and complete byte budget while only borrowing the transition.
The mutating methods reuse those checks, so a coordinator can validate both
owners before committing either. Tests verify that previews leave the mirror
base, journal records, pending spill, and byte accounting unchanged.

The first simulated device-loss replay now consumes that immutable pair. A
mirror base can materialize one sparse `RasterLayer` by stable `LayerId`.
The typed raster journal unifies full-flow round semantics with exact mapped
raster transitions and accounts for the enum's real inline size rather than
double-counting a variant. Replay walks every global revision in order,
applies only commands for the requested layer, accumulates damage/work stats,
and clears the reconstruction-only CPU undo stack before returning the target
revision. Pure loss simulations cover semantic paint followed by an exact
full-float overwrite, commands interleaved across layers, and exact absence
reclaiming a tile.

An undo/redo whose exact result is still mapping still needs the retained
older anchor plus inverse history. That unsupported interval remains explicit
rather than falling back to a direction-only record.

Revisioned background work now has a bounded scheduling state machine for the
save/autosave side of the migration. A payload reports its immutable target
revision. At most one task is active and one newer task is pending; newer
requests replace and return the previous pending payload, while duplicate or
older requests are already covered and returned without retaining another
snapshot. Serial tokens make completion transactional. Failure, a counterfeit
token, a future task relative to interactive state, and ID exhaustion all
return ownership without corrupting the queue. Successful completion reports
`saved_current_revision` only when the written revision still equals the
interactive revision, so saving `R` after drawing reaches `R + n` cannot clear
the modified marker. Pure tests exercise coalescing, stale success, worker
failure, promotion, token mismatch, and exhaustion.

Whole-document recovery now has an immutable CPU metadata half. It snapshots
canvas geometry, stable layer IDs, order, names, visibility, opacity, and the
active layer with a checked retained-byte count. Structural revisions enter
the raster journal as explicit `MetadataOnly` boundaries; the target metadata
snapshot carries their small canonical result, so they do not manufacture GPU
pixel work. Pairing metadata with a raster timeline requires identical target
revision and geometry and returns both snapshots unchanged on failure.

The simulated loss path now rebuilds every target layer, then constructs an
editable `Document` at the original revision with empty runtime undo history
and a recomputed composite. A six-revision pure case covers semantic paint,
layer creation, rename, opacity, visibility, reorder, active-layer identity,
layer-local pixels, and final revision. This also gives the background task
queue a complete revision-tagged payload for the currently supported raster
commands.

Exact sparse whole-layer ownership now covers imports, duplicates, and
restoring a deleted layer. The typed command snapshots canonical tile order,
shares immutable full-float pixel allocations, rejects mid-gesture and
non-finite capture, conservatively accounts for the complete retained tile
storage, and transactionally replaces the preceding layer before later
semantic strokes replay. A loss simulation advances the mirror base through a
deletion, then restores the missing layer from this command at the next
revision. Another keeps imported pixels immutable while the live layer
continues changing.

Application integration now retains that payload for resident duplication and
import, and reuses the ordinary presence edit for undo/redo while the atlas
payload remains dormant. An undo/redo result that exists only on the GPU and has not
finished mapping still needs the older inverse-capable anchor; the whole-layer
payload does not hide that ownership requirement.

The same immutable document payload can now build the existing layered
checkpoint entirely off the live path. Worker-side reconstruction produces a
`DocumentSnapshot` whose raster tiles retain shared pixel ownership; encoding
and atomic replacement then reuse the established `.sketchpad` format. The
six-revision loss case now also encodes and decodes that checkpoint and checks
layer metadata, active identity, and recovered pixels. The checkpoint object
retains its source revision and replay stats so the revision-task completion
can decide whether a successful write is still current.

Mirror reconstruction and layer duplication now preserve that ownership all
the way into `RasterLayer`: they install the existing immutable tile `Arc`
instead of copying 128-by-128 full-float pixels. The first subsequent edit
uses the raster's existing copy-on-write path, so duplicate layers remain
independent while untouched tiles cost only metadata and reference counts.
Tests assert pointer sharing before mutation, pointer separation afterward,
and an unchanged mirror snapshot.

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

That smoke now additionally checks the retained damage contract. The two-tile
sweep reports exact clipped local bounds on both sides of the tile boundary,
the replacement dot reports only its own local bounds, and dropping an encoded
stroke before submission restores an empty active-damage set. This is a state
and addressing proof; it does not add a timing claim.

The matching full-float document smoke also passed on that Intel adapter. A
half-opacity `[0.2, 0.4, 0.8]` straight-color sweep committed two logical tiles
in one color-page pass, cleared two previously uninitialized slots, and wrote
96 bytes of uniform/instance data. A quarter-opacity eraser then committed one
resident tile in one pass, performed no redundant color clear, and wrote 48
bytes. Exact `Rgba32Float` readback produced premultiplied paint
`[0.1, 0.2, 0.4, 0.5]`, destination-out erase
`[0.075, 0.15, 0.3, 0.375]`, unchanged paint elsewhere on both tiles, and
transparent pixels outside the sweep. This proves blend, addressing,
retention, and lazy-clear semantics on the first hardware adapter; it is not a
timing result and does not yet exercise the live executable.

The document smoke now captures and exchanges both mementos as well. The paint
capture selected two coalesced regions / 20 blocks / 81,920 bytes; the smaller
eraser selected one region / 4 blocks / 16,384 bytes. The exact hardware
sequence was paint, erase, undo erase, undo paint to transparent, redo paint,
and redo erase. Every checked pixel matched its original full-float value at
every state. Each swap transfers three times its stored bytes because it
captures current pixels, restores target pixels, and replaces the memento;
that traffic is a correctness counter, not a benchmark result.

The same smoke now records those two real GPU buffers in the bounded owner.
Their resident total is 98,304 bytes. Paint pins both tile residents, eraser
adds a second pin to the left tile, and the complete undo/redo sequence moves
the entries without changing either byte accounting or pin counts. Explicit
history clear returns both entries, reports zero resident history bytes, and
releases all three atlas pins. The pixel assertions remain exact after routing
through the pending history protocol.

That hardware sequence now additionally asserts that undoing the first paint
makes both color residents logically absent and redo restores their exact
`(LayerId, TileCoord)` identities. A control encodes another first-paint undo,
observes the staged absent metadata, drops its command encoder, and explicitly
discards the swap; both resident identities and the history side are restored.

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
- resident autosave replay, checkpoint encoding, and disk I/O run on the
  bounded worker; explicit save and shutdown recovery remain synchronous and
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
- hard round and eraser are GPU-resident on selected live hardware, and the
  active layer, revisioned layer properties/presence/order, duplication, and
  PNG import are resident there. Flat, pencil, bristle, palette knife, layer
  rename, and color picking are temporarily disabled rather than mutating
  stale CPU fallback data;
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
