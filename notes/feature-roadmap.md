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
- [ ] Add layer-aware semantic undo after the basic command path is stable.
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

Acceptance: a user can create, select, reorder, hide, duplicate, rename, and
delete layers without corrupting active strokes or undo state.

### 4. PNG image import

- [ ] Decode PNG with strict dimension, decoded-byte, and pixel-count limits.
- [ ] Convert declared sRGB input into the named linear working representation.
- [ ] Preserve alpha correctly as premultiplied linear RGBA.
- [ ] Import into a new named layer without resizing or replacing the document
  implicitly.
- [ ] Define placement for images smaller or larger than the canvas; begin with
  centered, clipped placement.
- [ ] Make import one semantic undoable command.
- [ ] Add tiny golden fixtures covering opaque, translucent, grayscale, and
  malformed images.

Acceptance: known input pixels produce the declared working values and a
failed import leaves the document unchanged.

### 5. PNG flattened export

- [ ] Export the visible composite, not the active layer.
- [ ] Convert linear premultiplied working pixels to straight-alpha sRGB
  correctly.
- [ ] Support full canvas and content-bounds export explicitly.
- [ ] Write atomically and report dimensions, encoded bytes, and elapsed time.
- [ ] Add deterministic decode-after-export image checks.

Acceptance: exported transparency and color match reference swatches within
the format’s declared 8-bit quantization contract; editable layers remain
untouched.

### 6. Painterly color-mixing brush

- [ ] Define a versioned brush recipe and bounded reservoir state.
- [ ] Separate pickup, reservoir mixing, and deposition.
- [ ] Begin with a conventional linear-RGB control.
- [ ] Add pigment-like candidates only against a saved swatch and stroke
  corpus with acceptable licensing.
- [ ] Make pickup source explicit: active layer by default, visible composite
  only as an opt-in semantic mode.
- [ ] Define behavior across transparency, repeated passes, stroke start/end,
  undo, and deterministic replay.
- [ ] Add CPU reference tests before any GPU kernel.
- [ ] Measure work per dab, affected pixels, sampled tiles, and temporary
  reservoir storage.

Acceptance: the saved mixing corpus is deterministic, visually intentional,
bounded to brush damage, and does not implicitly mix hidden layers.

### 7. Usability follow-through

- [ ] Color picker and recent colors.
- [ ] Minimal brush/preset selector.
- [ ] Layer panel.
- [ ] Open/import/export dialogs.
- [ ] Canvas rotation and reset-view controls.
- [ ] Save As and explicit native document paths.
- [ ] Long-term versioned brush editor described in
  [first-usable-product.md](first-usable-product.md#long-term-todo--brush-editor).

## Test and Performance Infrastructure

Add infrastructure alongside the feature that needs it:

- a layer/compositor microbenchmark with sparse and overlapping layer cases;
- a versioned multi-layer replay scene used by CPU and GPU runners;
- import/export golden images generated from tiny explicit pixel tables;
- document round-trip and corruption tests with hard resource limits;
- counters for composite tiles, pixels, source-layer visits, imported pixels,
  exported pixels, and mixing samples;
- cold and warm cases kept distinct;
- exact canonical checksums plus declared image-error metrics only at
  presentation/export boundaries.

Do not require a broad benchmark campaign before using a feature. Do require a
cheap repeatable regression case before its data size or algorithmic work can
grow unnoticed.
