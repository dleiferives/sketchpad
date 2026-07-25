# Design Vision — Sketchpad

Status: product direction, not a renderer specification. The immediate product
is defined in [first-usable-product.md](first-usable-product.md). See
[research-synthesis.md](research-synthesis.md) for the wider evidence,
definitions, corrections, and post-MVP research.

## Core Identity

Sketchpad should feel like a nearly frictionless place to draw:

1. a canvas that stays responsive and visually stable while navigating;
2. a small set of brushes whose marks and color interaction feel excellent;
3. excellent pen input rather than mouse input with pressure added later;
4. fast layers, undo, navigation, save, and recovery;
5. a light, canvas-first interface inspired by Sketchbook Pro.

The artistic experience is the goal. ADFs, vector rendering, sparse raster
tiles, and GPU compute are candidate mechanisms, not the identity of the app.

The first product deliberately uses defined/resizeable bounds and sparse raster
tiles. Deep zoom, semantic scale, nested canvases, retained vector editing, and
large simulations remain future capabilities rather than prerequisites.

## Terms We Will Use Precisely

- **Unbounded canvas extent** means content is not constrained to a fixed page
  rectangle.
- **Deep zoom** means retained source can be reconstructed at a much finer view
  scale than the initial drawing scale.
- **Semantic scale** means content intentionally appears, disappears, changes
  representation, or opens a nested drawing as view scale changes. This is
  different from geometric zoom and derived LOD.
- **Painter's order** means the explicit order in which translucent operations
  composite. It is not camera depth or cache level.
- **Resolution-independent media** means the source is not a fixed pixel grid.
- **Render cache** means disposable data derived from source.
- **Logical stroke** means one pen-down-to-pen-up gesture.

“Infinite canvas” is sometimes used for both unbounded extent and deep zoom.
The design and UI should not make that ambiguity.

## Reference Products

### Mischief / ICE

The inspiration is Mischief’s combination of extreme zoom, fluid drawing, and
compact procedural marks. The important architectural lesson from the published
[ICE API](https://www.ronaldperry.org/IP_Package/ICE_API.pdf) is broader than
“use an ADF”:

- drawing operations remain ordered within ordered layers;
- a stroke has begin, update, and finalize phases;
- curve fitting happens while drawing;
- layers render independently and composite with ordinary `over`;
- history contains reversible semantic operations;
- rendering can be current, incrementally updated, or require a full rebuild;
- geometry may use explicitly stored or on-demand procedural distance fields.

We should not assume Mischief flattened every colored layer into a single signed
field. The published architecture says otherwise.

### Sketchbook Pro

The interface inspiration remains:

- maximum canvas and minimum permanent chrome;
- dismissible, repositionable tools;
- one-action access to brush, eraser, color, and undo;
- natural pan, zoom, and rotation;
- UI contrast that recedes behind the work.

These principles do not depend on the rendering architecture.

### MyPaint and Krita

MyPaint and Krita are references for mature painting-engine behavior:

- sparse document-space raster tiles;
- damage tracking rather than global redraw;
- copy-on-write or memento-based undo;
- mip/LOD state derived from authoritative pixels;
- low-resolution transient preview while full-quality work catches up;
- honest handling of brushes that cannot be approximated at lower resolution.

They establish a serious baseline for soft, textured, translucent, and smudging
media even if Sketchpad also supports retained deep-zoom strokes.

### Google Ink

Google’s Ink APIs are a reference for input and stroke lifetime:

- real and predicted samples are distinct;
- an in-progress stroke is mutable and replaceable;
- a finalized stroke is immutable;
- brush recipe and canonical input survive;
- meshes are derived and regenerable;
- fidelity has an explicit error/memory tradeoff.

## Product Capabilities Under Consideration

These are goals to define and prioritize, not promises that every brush must
provide all of them.

The first-product subset and non-goals are authoritative in
[first-usable-product.md](first-usable-product.md).

### Drawing

- low-latency response with replaceable prediction;
- pressure-sensitive width and/or opacity;
- tilt, twist, velocity, and time as optional brush inputs;
- hard ink, soft airbrush, textured media, and possibly wet mixing;
- stable results across pan, zoom, save, reopen, and device loss.

### Editing

- per-gesture undo and redo;
- ordered layers with visibility and opacity;
- explicit normal compositing;
- transforms and selections;
- well-defined erasers:
  coverage erase, whole-stroke erase, geometric split, and/or masks;
- optional retained-stroke editing where the medium supports it.

### Navigation

- a defined, resizeable, potentially very large working extent;
- smooth pan, rotation, and zoom;
- progressive detail without flicker or stale regions;
- predictable ordinary raster zoom;
- later research into unbounded extent, deeper LOD, semantic scale, and nested
  canvases.

## Representation Principles

The durable architectural principle is separation:

1. **Canonical document source** stores artistic meaning and history.
2. **Derived geometry and spatial indexes** make source searchable/renderable.
3. **Derived render caches** accelerate display at selected scales.
4. **Screen output** is produced for the current view.

The source must be sufficient to rebuild after cache eviction, device loss, or
format migration. A cache must not quietly become the only copy of artistic
information.

For the first product, sparse raster tiles are canonical for painting layers.
The active stroke, brush reservoir, GPU tile atlas, command buffers, and
temporary material state are derived or transactional. Retained procedural
strokes may later coexist in a separate layer/media type; they are not required
to display ordinary finalized paint.

### Canonical Logical Stroke

A retained procedural stroke will probably need:

```
Stroke
  identity and layer order
  real timestamped input samples
  brush recipe/version
  deterministic random seed
  transform and conservative bounds
  explicit draw/erase/mask semantics
```

Whether raw input, a smoothed path, a fitted curve, or more than one of these is
saved remains a research question.

### In-Progress Stroke

The live stroke is not yet the document operation:

```
real samples ─────┐
                  ├─► mutable in-progress stroke ─► transient display
predicted samples ┘                │
                                   └─ pen-up ─► finalized document stroke
```

Predicted geometry may be replaced every frame. Only real input becomes
canonical unless the finalization policy explicitly says otherwise.

### Derived Data

Depending on the selected medium, a stroke may derive:

- filtered samples or fitted curves;
- expanded line/arc outlines;
- a partitioned mesh;
- spatial index entries;
- procedural distance data or ADF cells;
- sparse raster tiles and mip levels;
- GPU buffers and textures.

All of these need revision tracking and invalidation. None should be required to
interpret the saved document.

## Candidate Media Architectures

### Retained stroke layer

Ordered procedural/vector strokes are canonical and rendered directly or
through a spatial cache.

Best fit:

- hard or moderately textured ink;
- deep zoom;
- individual stroke selection and transformation;
- whole-stroke or geometric erasers.

Open issues:

- dense-scene rendering cost;
- soft and accumulated brush behavior;
- spatial indexing and minification;
- coordinate precision.

### Sparse raster paint layer

Premultiplied RGBA tiles are canonical for the layer, with copy-on-write or
memento undo and lazily generated mip levels.

Best fit:

- soft brushes, airbrush, grain, smudge, and wet paint;
- familiar pixel/coverage erase;
- mature layer compositing semantics.

Open issues:

- fixed resolution;
- transforms and resampling;
- file size and tile eviction;
- how it coexists with deep-zoom retained media.

### Procedural-distance or ADF-backed layer

Ordered source operations are canonical and a distance representation is
generated explicitly or on demand.

Potential fit:

- hard boundaries;
- CSG-like shape editing;
- adaptive detail and compact spatial evaluation.

Unresolved:

- concrete 2D cell representation and reconstruction error;
- incremental insertion, erasing, reorder, and color;
- borders across LOD;
- worst-case complexity;
- whether it beats direct retained vector rendering on real drawings.

### Hybrid document

Different layers or brush families use different canonical media and composite
through explicit layer semantics.

This may best match artists’ expectations, but it makes transforms, masks,
selection, file format, and UI more complex. Hybrid is a product design choice,
not a free combination of engines.

## Geometry, Coverage, Color, and Order

These must remain separate concepts:

- **Geometry** determines a boundary or footprint.
- **Coverage** determines antialiasing and soft influence.
- **Color** lives in a named working color space.
- **Alpha/opacity** determines transparency.
- **Order** determines overpainting.
- **Pigment mixing** is an optional color interaction.

A signed distance can help compute coverage around a hard edge. It does not
encode the colors and order of arbitrary translucent strokes.

The default layer model should be explicit premultiplied-alpha compositing.
Pigment/wet mixing can later be a brush interaction or named blend behavior.

## Input Principles

The window/event layer is not the whole tablet system. Platform-specific
collectors should normalize:

- position and monotonic timestamp;
- pressure;
- tilt and orientation/twist;
- tangential pressure where available;
- hover and proximity;
- tool identity, barrel buttons, and eraser end;
- coalesced history;
- prediction with provenance.

Missing capabilities remain optional rather than being fabricated. Mouse and
basic touch are fallback inputs.

## Interface Principles

### Canvas first

The canvas fills the application. Permanent controls should earn their space.
Transient UI must not intercept drawing unexpectedly.

### Direct manipulation

- pen or one-finger behavior depends on input/tool mode;
- pinch pans and zooms;
- rotation is available where the platform gesture is reliable;
- keyboard and mouse equivalents exist on desktop;
- undo, redo, eyedropper, and brush-size changes are rapid actions.

Exact gesture assignments require platform research. For example, one-finger
drawing and one-finger navigation conflict when no stylus is present.

### Visible approximation

If the engine temporarily displays predicted input, a lower LOD, or an
approximate brush result, replacement by the authoritative result should be
stable and unobtrusive. The UI should never imply that a coarse cache is the
saved artwork.

## Performance Intentions

Targets should eventually be expressed as measured budgets per platform and
representative mark:

| Concern | Initial intent |
|---|---|
| Input response | visible feedback within the current refresh interval |
| Active stroke | bounded by new samples and affected regions, not full document size |
| Idle canvas | no continuous redraw without animation or damage |
| Navigation | stable frame pacing with progressive cache refinement |
| Undo | one user gesture behaves as one command |
| Recovery | derived data can be dropped and regenerated |

The first-product plan now proposes an under-8-ms p95 renderer-work target for
its canonical 1080p active scenario on Atlas's Intel UHD 630, while preserving
raw distributions and quality evidence. This is a laboratory hypothesis, not a
universal platform guarantee.

## Technology Status

| Concern | Current status |
|---|---|
| Rust | working choice |
| `wgpu` + WGSL | working cross-platform GPU choice |
| `winit` | working window/lifecycle layer; incomplete tablet abstraction |
| `kurbo` | candidate curve-math library |
| Sparse raster tiles | selected canonical first-product paint medium |
| Brush-local pickup/deposit | selected first-product interaction model; exact equation open |
| Pigment-like interpolation | first-product mixing candidate; quality/licensing comparison required |
| SQLite native document | working first-product save/recovery direction |
| Vello | post-MVP baseline and possible conventional-vector component |
| Custom stroke expansion | post-MVP retained-media candidate |
| ADF/SDF cache | post-MVP research hypothesis |
| UI toolkit | open |

## Product Questions and Later Research

1. Which five representative marks define Sketchpad?
2. Which of those must remain crisp under deep zoom?
3. Are ordinary strokes individually editable after drawing?
4. What should two translucent strokes do when they overlap?
5. What exactly does the eraser remove?
6. Is pigment mixing a core behavior, a special brush, or a blend mode?
7. Can a document mix retained and raster layers?
8. What coordinate range and zoom ratio are actually useful?
9. Which stylus features are required on each launch platform?
10. Which behavior matters more when tradeoffs are unavoidable: latency,
    editability, deep zoom, painterly richness, or file compactness?
11. Should zoom only magnify ordinary geometry, or intentionally reveal
    different/nested artwork?
12. If semantic scale exists, how does the artist see and edit its visibility,
    transitions, and relationship to layer order?

Questions 1, 4, 5, 6, and 9 directly affect the first painter. Extreme
coordinate/zoom, retained-media, semantic-scale, and nested-canvas questions do
not block it. The current sequence is maintained in
[first-usable-product.md](first-usable-product.md#milestones) and
[research-synthesis.md](research-synthesis.md#research-agenda).

## Representative Mark Contract

The next product artifact should specify these five deliberately different
marks:

| Mark | Defining property | Semantic question |
|---|---|---|
| hard pressure ink | crisp taper and deep zoom | centerline edit or outline edit? |
| translucent marker | ordered self-overlap | does overlap accumulate within one stroke? |
| textured pencil/chalk | discrete grain and pressure | texture locked to stroke, page, or screen? |
| soft airbrush | continuous density accumulation | how should repeated passes accumulate? |
| wet paint | changes destination state over time | replayable simulation or baked result? |

For each mark, record:

- canonical source and which derived data may be discarded;
- behavior under self-overlap and crossing another mark;
- whole, split, coverage, and mask erasing;
- translation, scale, rotation, recolor, and brush editing;
- expected appearance from overview to deepest useful zoom;
- whether scale behavior is ordinary geometric zoom, an authored visibility
  interval, or a link into a nested canvas;
- undo/recovery unit and deterministic replay requirements;
- acceptable live approximation and finalization behavior.

This matrix is the test contract for renderer research. It can change when the
product vision changes, but a renderer cannot silently choose the answers.
