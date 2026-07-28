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

- [x] Define a versioned brush recipe and bounded reservoir state.
- [x] Separate pickup, reservoir mixing, and deposition.
- [x] Begin with a conventional linear-RGB control.
- [ ] Add pigment-like candidates only against a saved swatch and stroke
  corpus with acceptable licensing.
- [x] Make pickup source explicit: active layer by default, visible composite
  only as an opt-in semantic mode.
- [ ] Define behavior across transparency, repeated passes, stroke start/end,
  undo, and deterministic replay.
- [x] Add CPU reference tests before any GPU kernel.
- [x] Measure work per dab, affected pixels, sampled tiles, and temporary
  reservoir storage.
- [x] Expose the control in the live app as a temporary between-stroke keyboard
  toggle before designing brush UI.

Acceptance: the saved mixing corpus is deterministic, visually intentional,
bounded to brush damage, and does not implicitly mix hidden layers.

### 7. Natural-media brush family

- [x] Propagate normalized tablet tilt through distance-resampled brush
  samples.
- [x] Add a tilt-oriented flat rectangular nib.
- [ ] Add a graphite pencil with deterministic canvas-anchored paper tooth.
- [ ] Add a palette knife with persistent cross-blade paint lanes.
- [ ] Add a separated-bristle paint brush with per-strand paint load.
- [ ] Preserve one undoable gesture, bounded damage, deterministic replay, and
  allocation-free dab loops for every brush.
- [ ] Add brush-specific replay scenes and Apollo CPU measurements.
- [ ] Expose the presets through a compact selector, then add oriented cursor
  feedback.

The composition model, research basis, performance contract, and deliberate
deferral of full paint-height simulation are defined in
[Natural brush composition](natural-brushes.md).

Acceptance: each brush has distinct, stable contact/material behavior rather
than being a cosmetic hard-round preset; tilt changes the expected contact
without upright jitter; saved traces replay exactly.

### 8. Usability follow-through

- [x] Visible-composite Alt-contact color picker for mouse and tablet.
- [x] Eight-entry session-local recent colors with deterministic MRU behavior
  and keyboard traversal.
- [ ] Minimal brush/preset selector.
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
