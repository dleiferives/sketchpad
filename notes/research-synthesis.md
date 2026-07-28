# Rendering Architecture Research Synthesis

Status: research snapshot, 2026-07-24. This document records what is known,
what is inferred, and what still needs proof. The implementation/product
priority is defined separately in
[first-usable-product.md](first-usable-product.md).

## Current Product Decision

The first Sketchpad is a fast, usable desktop painter:

- defined and resizeable canvas bounds;
- sparse canonical raster layers;
- excellent real stylus input;
- hard ink, a flat nib, textured pencil/chalk, a palette knife, and a bristle
  brush;
- fixed-color deposition first, with brush-local pickup and pigment-like color
  mixing deliberately deferred;
- custom bounded `wgpu` tile work;
- layers, raster eraser, per-gesture tile undo, navigation, save, recovery, and
  export;
- integrated-GPU performance as a real gate.

Deep zoom, infinite extent, retained general vector content, semantic scale,
nested canvases, full wet simulation, broad renderer integration, and mobile
release work are post-MVP. The research remains available; it no longer blocks
the product.

## How to Read These Notes

The earlier notes sometimes present a promising mechanism as if it were already
a complete architecture. This document uses four evidence levels:

- **Verified**: directly supported by a primary source or the current prototype.
- **Inference**: a design consequence supported by several verified facts.
- **Working hypothesis**: plausible, but requires an experiment or a product
  decision.
- **Decision**: deliberately chosen for Sketchpad. There are very few of these
  yet.

The distinction matters. “An ADF can compactly represent a boundary” does not
prove “one ADF texture can preserve arbitrary colored, transparent brush
history.” “A paper expands strokes on the GPU” does not prove “its output can
efficiently update an SDF cache.”

## Executive Findings

1. **The current prototype is a useful flat-SDF experiment, not a production
   canvas architecture.** It proves that a sampled signed field can be updated
   and displayed through `wgpu`. It does not yet prove deep zoom, ordered color,
   soft brushes, local GPU updates, sparse storage, or a practical undo model.

2. **Mischief was not simply one flattened SDF per colored layer.** The
   published ICE API describes ordered layers, ordered drawing operations,
   logical strokes with begin/update/finalize, per-layer rendering, conventional
   Porter-Duff `over`, incremental/full render states, and reversible history.
   Distance fields sit underneath those semantics as a geometry/rendering
   technique.

3. **Distance, coverage, color, and draw order are different information.** A
   scalar signed distance can describe a hard boundary. It cannot by itself
   preserve the order and colors of overlapping translucent strokes. A soft
   brush needs a coverage/opacity profile; adding a constant to distance only
   moves the boundary.

4. **“Infinite canvas” and “infinite resolution” are separate properties.**
   Sparse raster tiles can provide unbounded canvas extent at a fixed
   resolution. Retained geometry can provide resolution-independent
   reconstruction, but only while coordinates remain numerically meaningful
   and the renderer can refine from the retained source.

5. **Mature painting engines keep canonical edits separate from disposable
   render state.** MyPaint uses sparse tiles, copy-on-write tile snapshots,
   damage bounds, and lazily generated mipmaps. Krita has tiled storage,
   mementos, swapping, and an explicitly approximate low-resolution instant
   preview while the authoritative stroke renders at full resolution.

6. **A logical stroke should be first-class.** Google’s Ink API and the ICE API
   both separate an in-progress stroke from a finalized one. Real input,
   predicted input, the brush recipe, and derived render geometry have distinct
   lifetimes. Recording every spacing dab as a document operation loses that
   structure.

7. **Cross-platform stylus input needs a dedicated platform adapter.** `winit`
   is useful for windows, lifecycle, mouse, and some touch data, but it is not a
   complete cross-platform tablet API. Pressure, tilt, twist, coalesced samples,
   prediction, proximity, and eraser-tool state differ across Apple, Windows,
   Linux, and Android APIs.

8. **Pigment mixing is orthogonal to the geometry representation.** Mixbox is
   an RGB-in/RGB-out mixing operation whose internal interpolation uses four
   pigment concentrations plus three residual channels. It does not justify a
   three-channel “latent cache,” and it should not silently replace ordinary
   alpha compositing across every layer.

9. **The strongest working direction is a retained, ordered document with
   replaceable derived representations.** Direct vector rendering, an
   ADF/procedural-distance cache, sparse raster tiles, or a combination can then
   be selected per brush or layer after product semantics and measurements are
   clearer.

10. **The newest opportunity is a brush compiler, not a universal renderer.**
    One immutable, versioned brush graph and its real input can remain canonical
    while derived backends specialize: analytic curve coverage for hard edges,
    sparse strips for general vector content, continuous density or bounded
    stamps for soft/textured brushes, and sparse simulation for wet media.

11. **Direct analytic curve coverage is newly practical to evaluate.** Slug's
    author announced in March 2026 that its patent was dedicated to the public
    domain, and its updated reference shaders are permissively licensed. It is a
    strong hard-edge experiment, not a solution for variable-width expansion,
    painterly media, document order, or caches.

12. **The CPU/GPU split must be measured rather than assumed.** Vello's Hybrid
    pipeline has moved toward CPU SIMD geometry work and compact GPU strip
    processing, but a substantial rewrite merged on 2026-07-24 after an earlier
    CPU coarse-raster stage proved costly and conflicted with filter layers.
    Semantic boundaries are more durable than one scheduling strategy.

13. **Low latency benefits from a distinct active-stroke lifecycle, but not
    necessarily a second portable presentation layer.** A stable committed
    prefix can be cached incrementally, an unstable real tail can redraw
    locally, and predicted input must remain replaceable. In one `wgpu`
    surface, presentation mode still applies to the entire frame. Retained
    viewport caching, many-layer compositing caches, and a native front buffer
    are separate experiments with different costs. The current direct renderer
    remains the control; see
    [Active-stroke presentation](active-stroke-presentation.md).

14. **Storage deserves architectural research now.** SQLite is a credible
    native-document candidate because it provides transactions, incremental
    updates, partial loading, and migration in one file. OpenRaster/SVG/PNG are
    valuable interchange formats. Neither storage choice should make
    renderer-specific caches canonical.

15. **The working renderer decision is to own a focused `wgpu` architecture,
    not to build the canvas around Vello.** Sketchpad should own scene
    extraction, damage, render planning, caches, compositing, profiling, and
    product-specific brush kernels. Vello Hybrid remains the leading
    replaceable backend for conventional vectors; Vello CPU is a fallback and
    reference candidate. “Raw `wgpu`” does not mean rebuilding a complete text,
    SVG, filter, and CPU graphics stack.

16. **Portability means common semantics plus a conservative path, not one
    identical schedule for every GPU.** `wgpu` exposes enough adapter
    capabilities to select tile sizes, workgroups, CPU/GPU splits, formats,
    pass layouts, uploads, and optional shader features at runtime. Native
    Metal/Vulkan/Direct3D paths require a measured abstraction ceiling and a
    product-visible win. See
    [renderer-selection.md](renderer-selection.md).

17. **A trace-driven performance and correctness laboratory should precede
    backend selection.** A useful test begins with a declared document and
    cache state, replays real/predicted input plus edit and view traces, and
    records raw latency distributions, GPU passes, work counts, transfers,
    memory, and visual error. Drawing a new stroke over existing content,
    finalization, old-content edits, navigation, and sustained mobile behavior
    are different scenario families. See
    [performance-laboratory.md](performance-laboratory.md).

18. **Scale is a useful conceptual third axis, but not one universal `z`
    coordinate or dense 3D texture.** Ordinary deep-zoom ink remains 2D;
    painter's order remains explicit; derived caches use hierarchical
    `(level, x, y)` keys. Explicit `(x, y, log₂ scale)` visibility/indexing and
    nested local 2D canvases are promising only if zoom intentionally reveals
    different artistic content. See
    [scale-space-storage.md](scale-space-storage.md).

19. **A physical brush and a repeated 2D stamp are separate design choices.**
    WetBrush uses localized particle detail and a field representation away
    from contact; industrial bristle work simulates bounded strand dynamics
    and sweeps projected contact strips; linear-stroke work shows that
    continuous evaluation can remove diameter-scaled stamp amplification.
    Sketchpad should first test connected blade and strand sweeps with
    fixed-color transfer. Optional load, pickup, height, and impasto are
    separate material stages, not hidden behavior in every brush. See
    [Continuous brush contact and physical paint](continuous-brush-contact.md).

## Definitions

### Canonical document

The durable information necessary to save, undo, edit, and regenerate the
artwork. Canonical data must not depend on a particular GPU, zoom level, cache
resolution, or current viewport.

For a procedural stroke this probably includes:

- a stable identity and position in layer order;
- real, timestamped input samples;
- a brush definition or versioned brush snapshot;
- deterministic random seeds;
- transforms and conservative bounds;
- operation semantics such as draw, mask, or erase.

Whether raw samples, a fitted curve, or both are canonical is still open.

### Logical stroke

One user gesture from pen-down to pen-up, not one brush dab. A brush engine may
generate hundreds of dabs, meshes, curve pieces, or cache writes from one
logical stroke.

### In-progress stroke

Mutable, low-latency state for the gesture currently being drawn. It may contain
real samples plus replaceable predicted samples. It should not become an undo
entry until finalized. Predicted samples must never be confused with canonical
input.

### Derived representation

Data regenerated from canonical source for speed or display, such as:

- smoothed samples or a fitted path;
- expanded stroke outlines or a partitioned mesh;
- a spatial index;
- ADF cells;
- raster tiles or mip levels;
- GPU buffers and textures.

Derived data is versioned, invalidatable, and disposable.

### Signed distance field (SDF)

A scalar function whose sign classifies inside/outside and whose magnitude is
the distance to a boundary. Sampled values are only an approximation between
samples. Boolean `min`/`max` operations preserve the intended sign but do not
always preserve exact Euclidean distance away from the resulting boundary.

### Adaptively sampled distance field (ADF)

A hierarchy that subdivides where a reconstruction function does not satisfy an
error bound. The original ADF work uses adaptive cells and higher-order
reconstruction to spend samples near detail. This is a representation of a
field, not automatically:

- a stroke data model;
- a color and alpha model;
- an incremental update algorithm;
- an unbounded coordinate system;
- or a guarantee of infinite zoom.

Those properties need additional mechanisms.

### Coverage

The fraction or weighted influence of a pixel/sample covered by a primitive.
Coverage drives antialiasing and soft brush opacity. It is not the same as
signed distance, although distance can be converted into edge coverage over a
chosen filter width.

### Premultiplied alpha

Color stored after multiplication by alpha. It is the robust representation for
filtering and compositing transparent imagery, provided the color-space
convention is also explicit. Intrinsic brush opacity, geometric coverage, layer
opacity, and mask opacity should not be collapsed into an unnamed single value.

### Infinite extent, deep zoom, and numerical range

- **Infinite or unbounded extent**: new content can be created without a fixed
  document rectangle.
- **Deep zoom**: source detail can be reconstructed at much finer screen scales
  than the initial view.
- **Numerical range**: coordinates and transforms remain stable at very large
  positions and zoom ratios.

A design may provide one without the others.

### Geometric zoom, semantic scale, and LOD

- **Geometric zoom** changes the projected size of the same 2D source.
- **Semantic scale** intentionally changes visibility, representation, or
  nested content as magnification changes.
- **LOD** is a disposable approximation chosen to satisfy screen error and
  resource constraints.
- **Painter's order** determines translucent compositing and is independent of
  all three.

Ordinary marks should not save camera zoom as hidden semantics. A semantic
scale interval or nested coordinate frame belongs in the document only when it
is an explicit artistic behavior.

### Three different kinds of “tile”

These terms must remain separate:

1. **Document tile/page**: a persistent or cached region of document space.
2. **LOD/mipmap tile**: a resolution level used for minification or preview.
3. **Rasterizer workgroup tile**: a short-lived screen-space bin such as
   Vello’s fine-rasterization tile.

They may have different dimensions, borders, ownership, and invalidation rules.

### Eraser semantics

“Eraser” is not one operation:

- **Pixel/coverage erase** lowers alpha in a raster layer.
- **Whole-stroke erase** removes retained objects that intersect the eraser.
- **Split/point erase** performs geometric subtraction and creates replacement
  stroke fragments.
- **Mask erase** writes a nondestructive layer or object mask.

These differ in performance, editability, and expected visual behavior. The
product must choose which ones it exposes.

## What the Current Prototype Actually Establishes

The repository currently contains a small Rust/`wgpu` application:

- `main.rs` owns window lifecycle, GPU state, camera interaction, input, and
  frame scheduling;
- `document.rs` stores a flat operation list and an embedded derived field;
- `sdf.rs` owns a fixed 1024×1024 CPU `f32` field and circle-union updates;
- `pipeline.rs` and the WGSL shader upload and display the field.

The drawing gesture is converted into overlapping circle stamps. Each circle is
recorded as an operation and updates a local region on the CPU, but a dirty
frame uploads the entire approximately 4 MiB field. The shader manually
bilinearly samples the unfilterable `R32Float` texture.

**Verified strengths**

- The basic signed-distance convention and circle union are understandable.
- Local CPU sampling work is bounded by a circle’s region.
- The camera and field display have been exercised on Atlas through Vulkan.
- The code is small enough to use as a diagnostic spike.

**Verified limitations**

- The document and its derived cache are coupled.
- A logical stroke is represented as many independent circle operations.
- The field is fixed-size, fixed-resolution, and globally uploaded when dirty.
- Only hard, single-color silhouette semantics are demonstrated.
- The continuously polling frame loop performs work while idle.
- Window/GPU lifecycle and surface recovery need a separate engineering pass.
- A public field buffer and shared global canvas size make invariants fragile.

This audit should be used to define experiments, not treated as a demand to
refactor immediately.

## Findings from Reference Systems

### ADF research and Mischief/ICE

The original [ADF paper](https://www.merl.com/publications/TR2000-15) describes
an adaptive hierarchy whose cells subdivide according to reconstruction error.
[Designing with Distance
Fields](https://www.merl.com/publications/TR2006-054) shows why distance fields
are useful for shape operations and high-quality boundaries.

The later [ICE technology
overview](https://www.ronaldperry.org/IP_Package/ICE_Technology_Overview.pdf)
describes “procedural detail-directed distance fields.” A field may be
explicitly stored or generated on demand, then passed to a procedural component
that applies geometry modulation, texture, and color.

The [ICE drawing API](https://www.ronaldperry.org/IP_Package/ICE_API.pdf)
reveals the surrounding application semantics:

- a canvas has an ordered list of layers;
- each layer evaluates drawing operations in draw order;
- completed layers composite with conventional Porter-Duff `over`;
- a stroke is initialized, updated with points, and finalized;
- curve fitting happens during drawing;
- draw and erase modes change layer opacity;
- history contains reversible drawing, layer, and transform operations;
- rendering distinguishes up-to-date, incremental, and full-rebuild states.

**Inference:** reproducing Mischief’s user-visible behavior requires retained
ordered operations and layer semantics even if distance fields are the core
geometry representation. One scalar field per layer is not an adequate model of
arbitrary translucent colored strokes.

**Unproven for Sketchpad:** the published material does not provide enough
implementation detail to assume a particular GPU page table, cell encoding,
local update cost, or modern cross-platform storage layout.

### MyPaint

[MyPaint’s canvas
documentation](https://www.mypaint.app/en/docs/backend/canvas/) describes a
sparse raster surface of fixed 64×64 tiles. Only painted tiles exist. New dabs
produce bounded damage, and composition is clipped to that damage.

The [tiled surface
implementation](https://github.com/mypaint/mypaint/blob/master/lib/tiledsurface.py)
uses shallow snapshot copies and marks tile data read-only. The first later
write to a tile performs copy-on-write. Mipmap surfaces are dirtied and rebuilt
on demand. Restoring a snapshot finds the changed tile set and reports only its
bounds.

The [brushwork
command](https://github.com/mypaint/mypaint/blob/master/lib/command.py) groups a
gesture into one command with before/after snapshots and a recorded input
sequence. [libmypaint](https://github.com/mypaint/libmypaint) receives
timestamped position, pressure, tilt, and related state; its tiled surface sends
each dab only to intersecting tiles.

**Inference:** sparse raster extent, copy-on-write undo, per-gesture history,
bounded damage, and lazy LOD are proven production patterns. They do not provide
resolution independence, but they are a strong baseline for soft and textured
brushes.

### Krita

Krita’s [tiled data
manager](https://github.com/KDE/krita/blob/master/libs/image/tiles3/kis_tiled_data_manager.h)
integrates tile mementos for undo/redo. Its [tile data
store](https://github.com/KDE/krita/blob/master/libs/image/tiles3/kis_tile_data_store.h)
supports compression, pooling, memory statistics, and swap storage.

[Instant Preview](https://docs.krita.org/en/reference_manual/instant_preview.html)
renders immediate feedback at lower resolution while the authoritative stroke
is calculated in the background. Krita documents brush settings for which this
preview is inaccurate or unavailable and acknowledges a possible visual “pop”
when the final result replaces it.

**Inference:** an approximate transient preview followed by an authoritative
full-resolution result is a legitimate latency strategy. The approximation and
replacement must be explicit.

### Google Ink and platform input

Google’s [Ink Stroke
Modeler](https://google.github.io/ink-stroke-modeler/) filters noisy real input
and predicts future input to reduce perceived latency. Its smoothing is an
intentional aesthetic transformation, not exact reconstruction.

Android’s [Ink
API](https://developer.android.com/develop/ui/views/touch-and-input/stylus-input/ink-api-modules)
has a particularly useful lifetime model:

- `InProgressStroke` incrementally consumes real and predicted inputs;
- a finalized immutable
  [`Stroke`](https://developer.android.com/reference/androidx/ink/strokes/Stroke)
  contains the canonical input batch, brush, and derived partitioned mesh;
- storage guidance saves the brush and input batch and regenerates mesh data;
- brush fidelity explicitly trades geometry size and speed against visible
  error at high zoom.

Native platform APIs expose information that a single `winit` event model does
not normalize completely:

- Apple supports [coalesced and predicted
  touches](https://developer.apple.com/documentation/uikit/getting-high-fidelity-input-with-coalesced-touches)
  and AppKit tablet pressure, tilt, rotation, and proximity.
- Windows
  [`POINTER_PEN_INFO`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-pointer_pen_info)
  includes pressure, rotation, and tilt; history APIs return coalesced packets.
- Linux
  [libinput tablet events](https://wayland.freedesktop.org/libinput/doc/latest/tablet-support.html)
  expose pressure, tilt, rotation, sliders, wheels, proximity, and eraser tools.
- Android documents [high-rate stylus input, prediction, hover, tilt, and
  front-buffer rendering](https://developer.android.com/develop/ui/views/touch-and-input/stylus-input/advanced-stylus-features).

**Inference:** the document-facing input type should be platform-neutral, but
the collection layer will need platform-specific backends. Real and predicted
samples should remain separate.

### GPU vector rendering and stroke expansion

[GPU-friendly Stroke Expansion](https://arxiv.org/abs/2405.00127) efficiently
turns stroked paths into line or arc outlines with strong handling of cusps and
joins. That is an important, bounded subproblem.

It does **not** define:

- a brush input model;
- an ADF construction or update algorithm;
- layer storage and undo;
- arbitrary soft textured brushes;
- or a persistent document cache.

[Vello](https://github.com/linebender/vello) demonstrates a modern
compute-centric `wgpu` vector renderer with compact scene encoding, prefix-sum
pipelines, fills, strokes, and layers. Its architecture separates encoding,
recording, and GPU execution. As of the current research snapshot, the main
renderer still documents alpha-status limitations, while Vello Hybrid is moving
toward broader production use.

**Inference:** Vello is a valuable correctness and performance baseline even if
Sketchpad eventually needs custom brush stages. Rejecting it solely because
`Scene::stroke()` is a library call would discard useful evidence prematurely.

### Random-access retained vector rendering

[Random-access rendering of general vector
graphics](https://hhoppe.com/proj/ravg/) encodes only the primitives overlapping
each coarse lattice cell. A pixel evaluates the ordered local primitive stream.
It preserves semitransparent fills, strokes, gradients, and layer order while
using exact inside classification and approximate distance for antialiasing.
Minification still uses a raster mip pyramid.

**Inference:** a spatial index over retained, ordered primitives is a serious
alternative to flattening a layer into a sampled SDF. Distance can be used for
coverage without being asked to encode color and history.

### Pigment mixing

The [Practical Pigment Mixing for Digital
Painting](https://dcgi.fel.cvut.cz/en/publications/2021/sochorova-tog-pigments/)
method intentionally lets painting remain RGB(A). Its `kmerp` operation:

1. converts each RGB endpoint into a latent representation of four pigment
   concentrations plus three additive residuals;
2. interpolates all seven latent values;
3. converts the result back to RGB.

The public [Mixbox implementation](https://github.com/scrtwpns/mixbox) exposes
RGB-in/RGB-out mixing. A LUT assists the conversion; the complete latent state
is not a three-value texture. The implementation is CC BY-NC 4.0 unless a
commercial license is obtained.

The paper also explores alpha-layer compositing where both layers behave as
thick wet paint. That is a deliberate nonstandard choice, not a universally
correct replacement for Porter-Duff `over`.

**Inference:** pigment behavior should initially be an opt-in brush interaction,
smudge/wet-paint model, or explicit blend mode. Geometry, coverage, ordinary
layer compositing, and color mixing should remain separable.

## Architecture Options

| Option | Strongest at | Main weakness | Deep zoom | Soft/painterly brushes | Risk |
|---|---|---|---|---|---|
| Retained vector + direct GPU rendering | ordered hard strokes, editability, transparency | repeated scene evaluation and tessellation; cache needed for huge scenes | yes, within coordinate/tessellation limits | possible but not its natural case | medium |
| Retained source + ADF/procedural-distance cache | hard boundaries, CSG-like shape operations, compact adaptive geometry | construction/update algorithm and ordered color semantics are unresolved | only by rebuilding/refining from source | distance alone is insufficient | high |
| Sparse tiled RGBA raster | textured/soft brushes, smudge, conventional layer semantics, proven undo | fixed resolution per layer; deep zoom reveals samples | extent yes, resolution no | excellent | low/medium |
| Spatially indexed retained primitives | local random access while preserving order | cell rebuilds, complex encoding, minification cache | yes, plus raster mips | depends on primitive/brush model | high |
| Hybrid layer/brush backends | lets each medium use an appropriate representation | file format, transforms, masks, compositing, and UI become more complex | selectively | selectively | high, but potentially best product fit |

No option should be chosen only from elegance. The intended marks and editing
semantics must choose the architecture. The first product now chooses sparse
tiled RGBA raster for ordinary painting because it best fits textured,
destination-dependent, mixed-color brushes and bounded undo. The other rows are
post-MVP media/cache options, not parallel blockers.

## Working Architectural Hypothesis

This is the longer-term hybrid hypothesis, not the first-product document
model. The immediate raster painter is specified in
[first-usable-product.md](first-usable-product.md):

```
Platform input backends
        │
        ▼
Normalized timestamped samples
  real ───────────────┐
  predicted ───────┐  │
                    ▼  ▼
              In-progress stroke
              + immutable, versioned brush graph
              + deterministic seed
                    │
                 finalize
                    ▼
Document → ordered layers → ordered logical operations
                    │
                    ├── spatial index / bounds / revisions
                    ├── fitted path or brush simulation
                    ├── backend classifier / cost policy
                    │    ├── analytic curve coverage
                    │    ├── sparse vector strips
                    │    ├── density kernel / bounded stamps
                    │    └── sparse raster / simulation island
                    ├── coverage/RGBA/LOD cache
                    └── GPU resources (all replaceable)
```

Core principles:

- The source document owns semantics; caches own performance.
- A gesture becomes one history command even if it modifies many cache tiles.
- The live stroke can use a transient overlay and prediction, then reconcile
  with the finalized source.
- Damage is expressed as affected document regions plus source revisions, not a
  single global dirty flag.
- Layers composite with explicit premultiplied-alpha semantics by default.
- A brush may target retained geometry, raster paint, or another explicit
  medium; it should not force every medium into one scalar representation.
- GPU coordinates can be camera-relative or tile-local even if document
  coordinates use a wider or hierarchical representation.
- A renderer backend may vary by brush, device, zoom, and edit state only if
  transitions have bounded visual error and do not change document identity.
- The portable GPU core must not assume in-place read/write storage textures;
  adapter-specific fast paths remain capability-gated.
- Native storage, history/recovery, and derived renderer caches are separate
  contracts.

## Claims That Should No Longer Guide Design

| Earlier claim | Corrected statement |
|---|---|
| “Every Mischief stroke is a continuous ADF function.” | Published ICE material describes retained ordered drawing operations whose geometry can use explicit or procedural distance fields. |
| “SDF zoom is perfect/free.” | Display sampling is cheap; detail is bounded by stored samples. Deep zoom requires retained source and refinement, plus a stable coordinate model. |
| “Stroke expansion output can simply be stamped into an SDF.” | Stroke expansion produces outlines. Efficient signed-distance construction needs spatial candidate lookup, inside classification, error bounds, borders, and an update strategy. |
| “A soft brush shifts the SDF by a falloff width.” | A constant shift changes the contour location. Softness is a coverage/opacity function over distance or a raster brush kernel. |
| “One SDF plus one color value preserves a painted layer.” | It cannot preserve arbitrary draw order, partial opacity, differently colored overlaps, or many erase semantics. |
| “CSG min/max always returns a valid exact SDF.” | It preserves the intended set/sign; magnitude can cease to be exact and may need re-distance if downstream logic depends on true distance. |
| “Mixbox latent color fits in `vec3`.” | The interpolation representation has four concentrations plus three residuals; one concentration is derivable, leaving six independent stored values, not three. |
| “Pigment-aware blending should composite every layer.” | Wet pigment layer mixing is an optional artistic semantic. Normal layer composition should remain available and explicit. |
| “winit provides cross-platform stylus events.” | It is a useful fallback/event-loop layer, but complete tablet fidelity requires native platform adapters. |
| “Using a custom renderer is necessarily better than Vello.” | Custom work may be justified by brush semantics, but Vello remains a valuable baseline and potentially a component. |
| “One renderer should handle every brush.” | One canonical brush model can compile to multiple derived backends; hard geometry, soft density, textured stamps, and stateful paint have different natural representations. |
| “GPU stroke expansion is automatically the fastest path.” | Expansion is one subproblem. CPU SIMD plus compact GPU work may perform better on integrated/mobile hardware and must be benchmarked. |
| “Tiles can always be composited independently.” | Ordinary premultiplied source-over has useful grouping properties, but filters, masks, isolation, non-normal blends, and destination-dependent media create explicit offscreen or replay boundaries. |
| “The native file should be a renderer dump or ZIP of caches.” | The native file must preserve semantic source and transactions. Caches are optional; OpenRaster/SVG/PNG are interchange candidates. |

## Research Agenda

The first-product milestones in
[first-usable-product.md](first-usable-product.md#milestones) take precedence.
The priorities below distinguish work required for the painter from post-MVP
architecture research.

### First product — Define the initial brushes

Specify and tune:

- hard pressure ink;
- document-anchored textured pencil/chalk;
- a tilt-oriented flat nib;
- a continuous palette knife and bounded-strand brush with fixed-color
  deposition;
- a raster coverage eraser;
- a soft airbrush after the contact paths are stable.

Destination pickup and live color/pigment mixing are deferred until they have
a visual corpus, explicit semantics, and a measured budget. Brush geometry
must not enable them implicitly.

The first product bakes finalized paint into sparse tiles and does not require
individual retained-stroke editing. The detailed contract is in
[first-usable-product.md](first-usable-product.md#initial-brush-set).

### Post-MVP — Prove or reject the ADF cache hypothesis

Research questions:

- What exactly is stored per 2D cell: corners, gradients, coefficients, bounds,
  procedural references, or a composite tree?
- What reconstruction function and error metric control subdivision?
- How are adjacent LOD borders made crack-free?
- How does one append, erase, recolor, or reorder a stroke without globally
  rebuilding a layer?
- How are candidate stroke segments found for a cell?
- How is inside/outside classified for self-overlapping expanded strokes?
- What are worst-case cell and operation counts for scribbles, hatching, and
  dense overpainting?
- Does color remain a separate ordered primitive stream or a raster cache?

Deliverable: a paper design with asymptotic costs and a small set of numerical
experiments. “Line soup → evaluate every segment at every sample” is not an
acceptable algorithm.

### Post-MVP — Establish a direct-vector baseline

Compare Vello or an equivalent retained renderer against any custom
distance-field approach on:

- one long pressure-sensitive stroke;
- ten thousand short strokes;
- dense transparent overdraw;
- extreme zoom and pan;
- incremental insertion and undo;
- mobile-class GPU constraints.

Measure first-frame latency, steady redraw cost, peak temporary memory, source
size, and visual error. A custom cache should beat a real baseline on a
product-relevant workload, not only in theory.

The current hard-edge comparison should include:

- Vello Hybrid or its current sparse-strip path;
- Slug-style analytic curve bands;
- expanded outlines through a conventional mesh/coverage path.

Treat Vello's 2026 Hybrid rewrite as current research, not a frozen production
API. Integrate it behind the narrow Sketchpad render plan rather than exposing
its scene to the document. Test CPU preparation and GPU time separately on
Atlas and a mobile-class device. The detailed role and adoption gates are in
[renderer-selection.md](renderer-selection.md).

### First product — Version brush recipes and bounded execution

Give every first-product brush recipe a version, deterministic seed, named
input mapping, and bounded raster execution contract. Pressure, time, distance,
tilt, spacing, texture, pickup, deposit, and reservoir behavior must be
reproducible for laboratory traces.

A universal expression graph and multiple equivalent retained/analytic
backends are post-MVP. The first need is a stable recipe boundary that does not
encode GPU resources or shader layouts into the document.

### Post-MVP — Coordinate and deep-zoom model

Research:

- `f64` world coordinates with camera-relative `f32` GPU transforms;
- hierarchical integer tile coordinates plus local floating-point offsets;
- origin rebasing and transform composition;
- nested 2D coordinate frames for extreme depth;
- continuous `log₂` view scale versus discrete derived `(level, x, y)` caches;
- optional semantic scale intervals and `(x, y, scale)` queries;
- serialization limits and deterministic geometry;
- pressure-width and antialiasing tolerances under extreme scale.

Deliverable: a numerical error budget across target canvas extent and zoom
ratio, plus a product decision on geometric versus semantic/nested zoom. Avoid
the phrase “infinite zoom” until that budget exists. See
[scale-space-storage.md](scale-space-storage.md).

### First product — Input architecture

First make pressure, timestamps, tool state, and gesture lifecycle correct on
the primary Linux desktop development path. Preserve normalized fields for:

- sampling rate and timestamps;
- coalesced and predicted samples;
- pressure, tilt, twist, tangential pressure;
- hover, proximity, barrel buttons, and eraser end;
- palm rejection and touch arbitration;
- latency APIs such as front-buffer rendering.

Decide which normalized fields are canonical, optional, predicted, or derived.
In the first raster painter, smoothing and brush evaluation are baked into the
committed tile result. A complete macOS/iOS, Windows, Wayland/X11, and Android
capability matrix follows when those ports enter scope.

### First product — Layer and eraser semantics

Specify normal compositing in a named working color space using premultiplied
alpha. Implement ordered raster layers with visibility and opacity plus an
active-layer coverage eraser. Test overlapping translucent colors, soft edges,
erase-and-redraw, and layer reordering.

Masks, clipping, non-normal blend modes, filters, and group isolation are
post-MVP.

### First product — Tile damage, residency, and undo

Compare and define:

- 128×128 versus 256×256 or another measured tile size;
- copy-on-write raster tile snapshots;
- before/after tile deltas;
- GPU tile atlas/array/individual allocation;
- CPU-authoritative versus GPU-authoritative active tile ownership;
- dirty rectangles versus whole-tile brush/composite work;
- cache and undo eviction/spill budgets.

Deliverable: state transitions for begin/update/finalize, undo, redo, reorder,
transform, save, reopen, cache eviction, and device loss.

LOD, retained semantic replay, strip/curve reuse, and ordered reduction caches
are post-MVP unless the first painter's measured large-document behavior
requires them.

### First product — Color and pigment semantics

Before implementing pigment mixing:

- choose the working RGB color space and transfer function;
- separate coverage, opacity, and alpha;
- identify exactly which operation uses pigment interpolation;
- determine whether Mixbox licensing is acceptable;
- compare Mixbox with open or independently generated alternatives;
- establish expected behavior for transparent paint and layer boundaries.

### First product — Storage and recovery

Store versioned document/layer metadata and compressed sparse raster tiles.
Active input and brush traces may support diagnostics and current undo, but the
ordinary painting display path does not replay all historic strokes. Include
crash recovery, autosave, partial loading, migration, cache compatibility, and
export to OpenRaster/PNG where semantics permit.

Compare a transactional SQLite application file with a ZIP/container design.
Test append/edit cost, partial viewport loading, compaction, migration, process
kill during commit, disk-full behavior, cache deletion, and recovery-image
salvage. Do not use live SQLite WAL files as a network/cloud interchange format
without an explicit checkpoint/snapshot protocol.

### First product — Runtime engineering

Separately audit:

- `winit` resume/suspend and redundant lifecycle events;
- safe `wgpu` surface ownership and recovery;
- redraw-on-damage versus perpetual polling;
- device loss and cache regeneration;
- storage-texture portability across WebGPU-class devices;
- color management, HDR, and display profiles.

These are required to make the selected sparse-raster medium usable. Mobile
lifecycle, HDR, and broad display-profile work can be staged after the primary
desktop loop.

## Prioritized Experimental Roadmap

First establish the trace, cache-state, oracle, raw-result, and hardware
protocol in [performance-laboratory.md](performance-laboratory.md). Then run:

1. **Tile core:** tile-size, format, allocation, damage, and copy-on-write undo
   comparisons.
2. **Hard ink over existing paint:** empty, sparse, dense, layered, warm, cold,
   and memory-pressure scenes on both Atlas adapters.
3. **Active input:** real/coalesced/predicted tails, cancellation, finalization,
   and gesture transaction behavior.
4. **Continuous brush contact:** connected blade and strand sweeps at large
   diameters, with a shared command stream, CPU-span reference, full-float GPU
   path, visual continuity corpus, and no live readback.
5. **Residency/navigation:** pan, zoom, rotate, eviction, revisit, and device
   reconstruction over a large sparse document.
6. **Storage/recovery:** transactional native tiles under process kill,
   partial commit, migration, disk pressure, and cache loss.

After the painter is usable, run the retained hard-edge, continuous-density,
ADF, deep-zoom, semantic-scale, pigment-mixing, and physical-paint simulation
comparisons already defined in the subject notes.

Every rendering experiment preserves the raw per-frame/event/pass evidence and
records p50/p95/p99 CPU and GPU time, precise latency interval, uploads, work
counts, peak memory, invalidated regions, rebuild cost, visual error, and
sustained/power behavior where available. See
[performance-laboratory.md](performance-laboratory.md),
[modern-rendering-research.md](modern-rendering-research.md), and
[document-semantics-and-storage.md](document-semantics-and-storage.md) for the
protocol, source survey, and stop conditions.

## Decision Gates

Before an individual production mark backend is chosen, we should be able to
answer:

1. What are the five representative marks Sketchpad must render?
2. What information must survive save/undo/edit?
3. Which representation is canonical for each layer or brush class?
4. What is the exact default compositing equation?
5. What does each eraser do to canonical data?
6. How is a live predicted stroke reconciled with real input?
7. How is a view region rebuilt after cache eviction or device loss?
8. What measured workload makes an ADF preferable to direct vector or raster?
9. What numerical range qualifies as “deep zoom” for the product?
10. Which platform capabilities are required for the first release?
11. Which brush-graph subset has equivalent analytic, stamp, and raster
    interpretations?
12. Which caches survive recolor, transforms, group changes, and renderer
    upgrades?
13. What is the transactional storage and crash-recovery guarantee?
14. Does zoom only magnify 2D source, or expose authored semantic scale/nested
    canvases?
15. Can every renderer claim be reproduced from a versioned trace with declared
    cache, quality, adapter, and result evidence?

Until those questions are answered, the durable decisions are the separation of
canonical source, derived render state, and explicit artistic semantics; a
narrow Sketchpad-owned render plan; and a shared `wgpu` foundation. No
particular third-party scene, mark backend, or cache texture is durable.
