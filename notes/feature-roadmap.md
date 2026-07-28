# Product Feature Roadmap

Status: active delivery queue, 2026-07-27.

This roadmap governs the product-feature phase. Performance work supports each
feature but does not replace shipping it. Features land as small vertical
slices with deterministic correctness tests, explicit document semantics, and
enough measurement to expose accidental full-canvas or all-layer work.

## Delivery Rules

Every shipped slice must define:

- its canonical document state;
- its active-layer and compositing semantics;
- its undo, save, and recovery behavior;
- conservative damage and GPU invalidation;
- deterministic unit or replay coverage;
- malformed-input and resource-limit behavior where files are involved;
- counters or a benchmark case for work that can scale with pixels, tiles, or
  layers.

When a benchmark or hardware run changes a decision, establishes a reusable
baseline, or exposes a nontrivial bug, record the setup, evidence,
interpretation, and engineering consequence in the relevant note and commit
history. Routine green verification belongs in the commit message or status
note; it should not bury durable findings in documentation noise.

The first implementation may be deliberately narrow. It must not silently
create a second incompatible document model, flatten editable data during
save, or hide unbounded work behind a convenient API.

## Ordered Feature Queue

### 1. Raster layer document foundation

- [x] Replace the unused SDF `Document` prototype with an ordered raster-layer
  document.
- [x] Give layers stable IDs, names, visibility, opacity, and sparse raster
  media.
- [x] Support create, activate, duplicate, delete, rename, and reorder.
- [x] Maintain an incremental premultiplied-linear composite for presentation.
- [x] Paint and erase only the active layer; incrementally present the derived
  visible composite.
- [x] Make structural layer changes produce explicit damage.
- [x] Add layer-aware semantic undo after the basic command path is stable.
  The selected design is one 256-entry document sequence interleaving raster
  memento references with reversible structural commands; it is not a second
  per-feature stack.
- [x] Measure recomposited tiles, pixels, and source layers visited.

Acceptance: ordering, opacity, visibility, sparse allocation, and incremental
damage are deterministic; ordinary brush damage does not recomposite the
declared canvas or unrelated tiles.

### 2. Layer-aware native save and recovery

- [x] Version the native checkpoint/document container for multiple layers.
- [x] Preserve IDs, names, order, visibility, opacity, active layer, canvas
  geometry, and exact sparse `f32` pixels.
- [x] Reject corrupt, truncated, oversized, duplicate-ID, and incompatible
  documents.
- [x] Keep atomic replacement and recovery behavior.
- [x] Migrate the existing flat version-1 checkpoint as one recovered layer.
- [x] Add multi-layer deterministic round-trip and corruption tests.

Acceptance: save/reopen never flattens editable layers and recovery either
restores a valid whole document or leaves the prior file intact.

### 3. Minimal layer controls

- [x] Add temporary keyboard commands before building the graphical panel.
- [x] Show active layer, layer count, and modified state in existing feedback.
- [x] Prevent structural commands during an active brush transaction.
- [ ] Add the graphical layer panel after command semantics are proven.
  Begin with the bounded embedded-egui experiment and promotion gate in
  [Immediate-mode UI overlay architecture](ui-overlay-architecture.md);
  preserve the same `UiAction` boundary for the custom fallback.

Acceptance: a user can create, select, reorder, hide, duplicate, rename, and
delete layers without corrupting active strokes or undo state.

### 4. PNG image import

- [x] Decode PNG with strict dimension, decoded-byte, and pixel-count limits.
- [x] Convert declared sRGB input into the named linear working representation.
- [x] Preserve alpha correctly as premultiplied linear RGBA.
- [x] Import into a new named layer without resizing or replacing the document
  implicitly.
- [x] Define placement for images smaller or larger than the canvas; begin with
  centered, clipped placement.
- [x] Make import one semantic undoable command.
- [x] Add tiny golden fixtures covering opaque, translucent, grayscale, and
  malformed images.
- [x] Accept native window file-drop import through the same bounded codec and
  one-command document insertion.

Acceptance: known input pixels produce the declared working values and a
failed import leaves the document unchanged.

### 5. PNG flattened export

- [x] Export the visible composite, not the active layer.
- [x] Convert linear premultiplied working pixels to straight-alpha sRGB
  correctly.
- [x] Support full canvas and content-bounds export explicitly.
- [x] Write atomically and report dimensions, encoded bytes, and elapsed time.
- [x] Add deterministic decode-after-export image checks.

Acceptance: exported transparency and color match reference swatches within
the format’s declared 8-bit quantization contract; editable layers remain
untouched.

### 6. Painterly color-mixing brush

Status: deliberately deferred. The experimental CPU mixing engine, benchmark,
live-app mode, keybinding, and destination-color pickup in natural brushes were
removed on 2026-07-28. Ordinary brushes deposit only their selected color.

- [ ] Define the artistic target and a saved visual corpus before writing a
  new mixing kernel.
- [ ] Treat tip geometry and material interaction as separate systems; a
  bristle or knife shape must not implicitly enable mixing.
- [ ] Establish a per-frame/per-dab CPU, GPU, memory-bandwidth, and temporary
  storage budget on Apollo before integrating a live mode.
- [ ] Define pickup source, transparency, repeated-pass, undo, persistence,
  replay, and hidden-layer semantics.
- [ ] Compare conventional, perceptual, and properly licensed pigment-like
  candidates against the same corpus.
- [ ] Require deterministic correctness tests and a non-mixing control with
  identical geometry.

Acceptance for any future restart: compelling visual evidence, bounded state
and work, no implicit destination sampling, and measured headroom on the
constrained target. Research history remains in
[Pigment color mixing research](pigment-mixing.md), but it is not current
product code.

### 7. Natural-media brush family

- [x] Propagate normalized tablet tilt through distance-resampled brush
  samples.
- [x] Add a tilt-oriented flat rectangular nib.
- [x] Add a graphite pencil with deterministic canvas-anchored paper tooth.
- [x] Add a palette knife with persistent cross-blade paint lanes.
- [x] Add a separated-bristle paint brush with per-strand paint load.
- [ ] Preserve one undoable gesture, bounded damage, deterministic replay, and
  allocation-free dab loops for every brush.
- [ ] Add brush-specific replay scenes and Apollo CPU measurements.
- [x] Expose the presets through a compact selector, then add oriented cursor
  feedback.

The composition model, research basis, performance contract, and deliberate
deferral of full paint-height simulation are defined in
[Natural brush composition](natural-brushes.md). The replacement of dense
knife/bristle dabs with continuous swept contact, including its GPU/CPU
measurement gate, is defined in
[Continuous brush contact and physical paint](continuous-brush-contact.md).
This is not a watercolor or live color-mixing project.

Acceptance: each brush has distinct, stable contact/material behavior rather
than being a cosmetic hard-round preset; tilt changes the expected contact
without upright jitter; saved traces replay exactly.

### 8. Usability follow-through

- [x] Visible-composite Alt-contact color picker for mouse and tablet.
- [x] Eight-entry session-local recent colors with deterministic MRU behavior
  and keyboard traversal.
- [x] Minimal brush/preset selector.
- [ ] Layer panel.
- [x] Native PNG import/export dialogs.
- [x] Native document open dialog.
- [ ] Canvas rotation controls.
- [x] Reset-to-fit view control.
- [x] Save As and explicit native document paths.
- [ ] Long-term versioned brush editor described in
  [first-usable-product.md](first-usable-product.md#long-term-todo--brush-editor).

## Test and Performance Infrastructure

Add infrastructure alongside the feature that needs it:

- a layer/compositor microbenchmark with sparse and overlapping layer cases;
- a versioned multi-layer replay scene used by CPU and GPU runners;
- [x] import/export golden images generated from tiny explicit pixel tables;
- [x] a versioned PNG first/warm codec runner with transparent, sparse, dense,
  and high-entropy cases plus exact decode→export→decode checksums;
- document round-trip and corruption tests with hard resource limits;
- counters for composite tiles, pixels, source-layer visits, imported pixels,
  exported pixels, and mixing samples;
- cold and warm cases kept distinct;
- exact canonical checksums plus declared image-error metrics only at
  presentation/export boundaries.

Do not require a broad benchmark campaign before using a feature. Do require a
cheap repeatable regression case before its data size or algorithmic work can
grow unnoticed.
