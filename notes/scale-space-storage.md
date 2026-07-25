# Scale-Space, Depth, and “3D” Drawing Storage

Status: post-MVP architecture research, 2026-07-24. This note evaluates using a
third axis for zoom/depth, compares it with ordinary 2D and hierarchical
storage, and defines later experiments. It does not block the sparse-raster
painter in [first-usable-product.md](first-usable-product.md).

## Short Answer

There is a strong idea here, but it should be described as **scale-aware 2.5D
storage**, not as one dense 3D texture or one overloaded `z` coordinate.

Recommended direction:

1. Keep ordinary marks as canonical 2D geometry or sparse 2D paint.
2. Keep painter's order/layers separate from scale.
3. Represent view scale continuously as `log₂(pixels per document unit)`.
4. Key derived tiles and LOD data hierarchically by `(level, x, y)`.
5. Allow explicit semantic scale ranges or nested 2D canvases if the product
   should reveal different drawings/details while zooming.
6. Index `(x, y, scale)` when that semantic behavior provides useful pruning.
7. Do not store the canvas as a dense GPU 3D volume.

This preserves the exciting behavior—drawings inside drawings, meaningful
scale bands, progressive reveal, and stable deep coordinates—without confusing
zoom, paint order, and raster LOD.

## First Separate the Meanings of `z`

At least five independent concepts can look like a third dimension:

### Painter's order

Which operation composites over another. This is a total or partially
structured order within layers and groups. Ordinary translucent painting needs
this even when every mark lies in the same 2D plane.

Painter's order must not be replaced by GPU depth testing. A depth buffer keeps
the nearest fragment; source-over painting needs ordered color and alpha from
multiple fragments.

### Layer/group order

The user-visible organization of operations, masks, opacity, isolation, and
filters. It can often supply painter's order but is also an editing structure.

### View scale

The camera magnification. A useful continuous coordinate is:

```text
scale = log₂(screen pixels / document unit)
```

Adding one to this value doubles magnification. It is a property of a view at a
moment in time, not automatically a property of each mark.

### Derived LOD/cache level

The discrete approximation or raster resolution selected to keep visual error
below a screen-space threshold. It is derived from view scale, device scale,
content frequency, brush behavior, and available resources.

### Semantic scale or nested depth

A deliberate artistic property: an object appears, disappears, changes
representation, or opens into another 2D world at certain view scales. This is
the meaning for which a scale axis may belong in the canonical document.

Do not put all five into one `f32 z`. They have different ordering,
serialization, query, interpolation, and editing rules.

## Ordinary 2D Already Encodes Much of “Drawing at a Zoom Level”

Suppose the camera is zoomed in 1,000× and the artist draws a mark two screen
pixels wide.

If the brush is screen-size controlled, the application converts that width to
a very small world-space width. When the view zooms out, the mark becomes tiny
or subpixel. Its 2D position and world-space width already encode the scale at
which it was created.

If the brush is document-size controlled, drawing the same world path with the
same brush should produce the same canonical stroke regardless of camera zoom.
The authoring zoom should not silently change it.

Therefore, storing `authoring_zoom` as `z` is redundant unless it drives an
explicit semantic rule. A valuable invariant is:

> Camera zoom does not change canonical mark semantics except for brush/tool
> properties explicitly defined in screen space or semantic scale.

This avoids zoom becoming a hidden brush input.

## When a Scale Axis Adds Real Product Value

A scale dimension is meaningful if Sketchpad wants any of the following:

- details that become visible only after zooming in;
- overview marks that disappear before they become enormous;
- one object changing from symbol to outline to detailed drawing;
- a region opening into a separately editable nested drawing;
- multiple artistic “depths” occupying the same 2D location;
- scale-dependent annotation or storytelling;
- different canonical representations across scale, not just cached
  approximations;
- extreme zoom through locally precise nested coordinate systems.

This is commonly called **semantic zoom**. Pad++ explicitly explored
multiscale graphical objects and scale-dependent representations in a zoomable
sketchpad. See the original
[Pad++ paper](https://hci.ucsd.edu/hollan/Pubs/pad.pdf).

The related [space-scale diagram
paper](https://doi.org/10.1145/223904.223934) represents a 2D world and its
magnifications along a scale axis to reason about visibility and navigation.
Later GIS work calls a related representation a
[space-scale cube](https://doi.org/10.5194/isprsarchives-XXXVIII-4-C21-95-2011):
a horizontal slice at a scale produces a 2D map, while changes across the third
dimension encode generalization.

These precedents validate the conceptual model. They do not require a GPU 3D
texture or prescribe Sketchpad's document semantics.

## Candidate Storage Models

| Model | Canonical structure | Strongest property | Main problem | Recommendation |
|---|---|---|---|---|
| ordinary 2D retained/raster | 2D marks/tiles plus order | simple, predictable drawing | very deep detail can stress coordinates and overview queries | mandatory baseline |
| hierarchical 2D | quadtree/pages plus local coordinates | sparse extent, LOD, local precision | hierarchy and cross-boundary edits | likely core infrastructure |
| scale-aware 2.5D | 2D marks plus scale intervals/representations | semantic zoom and scale pruning | transition/edit semantics | promising optional product model |
| nested 2D canvases | transform tree of local 2D worlds | arbitrarily deep local precision and drawings-inside-drawings | navigation, cross-level strokes, export | best “deep worlds” experiment |
| true 3D scene | XYZ geometry and camera/depth | physical depth/perspective | wrong compositing and interaction model for ordinary paint | only if 3D art becomes a product goal |
| dense 3D texture/voxel volume | samples indexed by x/y/z | hardware trilinear sampling | density, limits, mip semantics, no draw order | reject as general canvas storage |

The likely system is a combination of the first four, not a single winner:

- 2D canonical marks and tiles;
- a hierarchical spatial/coordinate structure;
- optional semantic scale ranges;
- nested local canvases for deliberately deep content;
- derived LOD/cache pages addressed by level and 2D tile coordinate.

## Model A: Ordinary 2D Canonical Scene

Each mark has:

- 2D canonical geometry or paint bounds;
- brush/media definition;
- layer/group and operation order;
- transforms;
- stable identity and revision.

The camera supplies a 2D transform and continuous view scale. A spatial index
queries visible bounds. The renderer chooses an approximation by projected
error.

### Advantages

- familiar editing and compositing;
- drawing behavior does not depend on navigation history;
- one source can refine continuously at any zoom within numerical limits;
- simple export to ordinary 2D formats;
- few scale-transition semantics.

### Limitations

- extremely large global extent/scale ratios eventually exceed fixed floating
  precision;
- an overview query can find huge numbers of microscopic objects;
- semantically different content at the same `x/y` needs extra organization;
- the renderer still needs a hierarchical cache for minification.

This remains the correct baseline even if semantic scale is later added.

## Model B: Hierarchical `(level, x, y)` Storage

This is the natural shape for sparse tiles and derived LOD:

```text
cell key = (level, integer x, integer y)
```

Each level changes cell span by a power of two. The hierarchy is conceptually
three-coordinate, but it is a quadtree/pyramid rather than a uniform 3D grid.
The number and spatial extent of cells change with level.

Map systems use this shape because a low zoom needs few coarse tiles and a high
zoom needs more fine tiles. Mapbox's
[zoom-level documentation](https://docs.mapbox.com/help/glossary/zoom-level/)
describes the standard quadtree relationship, where each tile splits into four
at the next level. That is useful precedent for addressing and streaming, not
a reason to adopt geographic projection or Mapbox formats.

### Appropriate contents

- raster color/coverage mips;
- vector/strip/curve-band chunks suitable for a scale interval;
- spatial summaries;
- candidate lists;
- reduction/composite caches;
- background-refined representations;
- storage pages and recovery chunks.

### Important rule

LOD levels are derived and disposable unless the product explicitly authors
different semantic representations. The document must be reconstructable if
the entire `(level, x, y)` cache disappears.

### Storage amplification

A complete 2D raster mip pyramid adds:

```text
1 + 1/4 + 1/16 + … = 4/3
```

of the finest-level pixel count. Sparse, bordered, compressed, or independently
versioned tiles change the real overhead. Storing a full same-resolution plane
for every zoom would instead multiply storage by the number of planes and is
not a mip pyramid.

## Model C: Scale-Aware 2.5D Objects

Add an explicit scale-visibility or representation interval:

```text
2D bounds × [minimum visible scale, maximum visible scale]
```

A view queries its 2D viewport at the current continuous scale. The object is a
prism in conceptual `(x, y, scale)` space.

Possible canonical properties:

- visible scale interval;
- fade-in/fade-out interval;
- one or more representation transitions;
- semantic importance or overview priority;
- scale-anchored line-width behavior;
- link to a nested canvas or detail group.

### What this enables

- do not even consider microscopic detail in a far overview;
- reveal intentional content at deeper scale;
- keep overview symbols legible;
- create zoom-based narratives;
- progressively request relevant data;
- organize multiple meanings at one 2D location.

### What it does not solve

- paint order;
- coordinate precision by itself;
- correct minification;
- arbitrary overlap and transparency;
- cache invalidation;
- local simulation;
- continuity across representation changes.

### Index choices

A 3D R-tree can index `(x, y, scale)` boxes. SQLite's official
[R*Tree documentation](https://www.sqlite.org/rtree.html) supports up to five
dimensions and explicitly describes 3D range queries. This is especially
interesting because SQLite is already a candidate native document store.

Important constraints:

- default R-tree coordinates are `f32`; global deep coordinates need outward
  conservative bounds, local pages, quantization, or another index;
- `rtree_i32` provides 32-bit integer bounds, not arbitrary precision;
- the R-tree returns candidates, not exact geometry;
- modifying the same R-tree during an unfinished query can lock;
- if almost every object spans all scale values, the third dimension adds
  little pruning and may worsen the index.

A hierarchical 2D spatial tree plus per-node scale intervals may be simpler
than a generic 3D R-tree. Both should be benchmarked with real scale-aware
documents.

## Model D: Nested 2D Coordinate Frames

If the artistic goal is “zoom into this drawing and find another drawing
inside,” nested coordinate systems are more powerful than a huge global float.

Each group/canvas has:

- a local 2D coordinate system;
- an affine transform into its parent;
- an activation/visibility scale envelope;
- local children, marks, layers, and caches;
- a stable semantic link from parent to child.

The view maintains a path through the hierarchy and rebases coordinates as it
descends. GPU geometry stays near a local origin and within a manageable scale.

### Advantages

- scale depth is limited by hierarchy/serialization rather than one global
  floating exponent;
- local editing remains numerically stable;
- child worlds can load, cache, save, and refine independently;
- the structure directly expresses intentional drawings-inside-drawings;
- scale-aware culling occurs at group boundaries.

### Hard questions

- Can one stroke cross from a parent into a child?
- What does selection do at a scale boundary?
- Are child pixels clipped to a portal/shape?
- Does the parent show a live thumbnail, a raster cache, or direct child render?
- How do masks, filters, and blend groups cross the boundary?
- How does undo span multiple canvases?
- What happens when a parent transform changes?
- How are nested canvases exported to PNG/SVG/other flat formats?
- Can a child be instanced in multiple parents?
- How does navigation communicate which depth the user is editing?

This should be a product experiment, not a hidden storage implementation.

## Why a Dense GPU 3D Texture Is the Wrong Default

A GPU 3D texture is useful for volumetric fields whose neighboring `z` samples
have physical/filtering meaning. Zoom levels and painter's order do not fit that
assumption.

### Portability and size

`wgpu` 30's portable modern defaults guarantee 8,192 for a 2D texture dimension
but only 2,048 for a 3D texture dimension. Its downlevel defaults reduce the 3D
limit to 256. See [`wgpu::Limits`](https://docs.rs/wgpu/latest/wgpu/struct.Limits.html).
An unbounded sparse canvas cannot be one texture in either case, but the 3D
limit makes the mismatch clearer.

### Wrong mip behavior

Successive texture mip levels halve every spatial dimension. For a true 3D
texture that includes depth. If `z` itself means zoom/LOD, ordinary hardware
mips would shrink and filter the zoom axis while also selecting a mip of that
volume. This creates two conflicting meanings of LOD.

### Dense allocation and locality

Most `(x, y, scale)` cells in an infinite drawing would be empty. Portable
`wgpu` does not make a dense 3D allocation into a general sparse document
database. Explicit 2D tile atlases, arrays, buffers, and page tables allow the
application to allocate only visible/dirty content and control eviction.

### Filtering is not semantic transition

Trilinearly blending adjacent `z` slices might be useful for a cross-fade, but
it does not correctly:

- merge or split vector objects;
- preserve thin features;
- change a symbol into detailed geometry;
- apply ordered translucent operations;
- select nested editable content.

Those require explicit representation/transition semantics.

### No history or order

One voxel can hold a field or final color. It cannot by itself preserve
arbitrary operation order, multiple translucent colors, brush definitions,
eraser history, or editability. This is the same distinction already identified
for one scalar SDF texture.

A small 3D texture may still be valid inside a specific brush or simulation,
for example a local volumetric material state. That does not make it the canvas
or document format.

## Why Physical 3D `z` Is Also Not Painter's Order

Mapping layer order to geometric height and enabling depth testing appears
convenient, but:

- depth testing normally discards covered fragments rather than compositing
  them in source order;
- transparent geometry still needs ordering or another exact compositing
  method;
- close `z` values create precision and z-fighting issues;
- reordering a layer changes geometry rather than only an ordering key;
- filters, masks, isolated groups, and erasers are not depth operations;
- camera perspective would change a fundamentally 2D artwork.

The renderer may encode operation order into temporary sort keys, tile lists,
or depth-like values as an optimization. The semantic order should remain an
explicit document concept.

## Continuous Scale, Discrete Storage

The camera should zoom continuously. Storage may remain discrete.

Let:

```text
s = log₂(pixels per document unit)
```

Derived cache level selection chooses nearby integer levels based on projected
error, not simply `round(s)`. Around a transition it may:

- keep the previous level with hysteresis;
- request the next level asynchronously;
- temporarily use a parent/child approximation;
- cross-fade only when the representations support it;
- cancel stale refinement when the camera moves.

For semantic scale, visibility and representation transitions should use
explicit continuous intervals. Discrete planes without hysteresis or transition
rules will pop as the user crosses a boundary.

## Proposed Sketchpad Model

### Canonical mark

An ordinary mark contains 2D semantics and no required authoring `z`:

- local/canonical geometry or raster mutation;
- brush version and real samples;
- layer/group/order identity;
- 2D conservative bounds;
- coordinate-frame identity;
- optional explicit semantic scale behavior.

### Optional semantic scale behavior

Only when intentionally authored:

- minimum/maximum visible view scale;
- fade/transition intervals;
- scale behavior for width and detail;
- representation or child-canvas references;
- overview importance.

Default behavior is geometric zoom with visibility at every scale where the
mark contributes to a pixel.

### Coordinate hierarchy

Use wide or hierarchical CPU coordinates and camera-relative/local `f32` GPU
coordinates. A nested canvas supplies another local frame rather than forcing
global coordinates through an extreme exponent.

### Derived index/cache

Potential keys:

```text
coordinate frame
layer/group revision
LOD level
integer tile x/y
representation/backend version
quality/error class
```

These are disposable and can be rebuilt from canonical content.

### Independent order

Keep an operation/layer order key independent of:

- spatial tile;
- view scale;
- cache level;
- coordinate-frame depth.

This is essential for correct overlapping translucent strokes.

## Interaction Semantics to Decide

Before semantic scale enters the native file format:

1. Does a deep mark naturally become subpixel, or disappear at an authored
   threshold?
2. Is authoring scale visible/editable in the UI?
3. Can a mark span multiple scale bands?
4. Are representation changes automatic approximations or artistic content?
5. Do nested canvases behave like groups, portals, layers, or separate
   documents?
6. What happens when a user draws while a transition is partially visible?
7. How are selection and hit testing resolved when several scale depths overlap?
8. Does ordinary layer order cross scale/nested boundaries?
9. How are children previewed from the parent?
10. What does flatten/export mean?
11. Are screen-space brush properties recorded as final world values or
    evaluated again during playback?
12. Can two views at different zooms edit the same document consistently?

If the answers reduce to “everything is ordinary geometric zoom,” scale should
remain a view and cache parameter rather than canonical mark data.

## Performance Experiments

The [performance laboratory](performance-laboratory.md) should compare storage
models rather than assuming a third dimension helps.

### Corpus

- ordinary drawing with no semantic scale;
- many microscopic marks visible only when deeply zoomed;
- the same `x/y` region populated at many intentional scale bands;
- nested drawings at depths of 1, 4, 16, and 64;
- long marks crossing many spatial cells;
- marks whose 2D bounds overlap but scale ranges do not;
- marks visible across all scales;
- rapid zoom through populated and empty scale bands;
- edits near the root that affect child previews;
- memory pressure with derived parent/child caches.

### Compare

1. flat 2D spatial index;
2. hierarchical 2D tree with LOD summaries;
3. 2D tree plus scale intervals;
4. 3D `(x, y, scale)` R-tree;
5. nested coordinate-frame hierarchy;
6. raster/vector cache pyramid.

### Measure

- canonical and index bytes;
- index build/update time;
- p50/p95 viewport query time;
- candidate and false-positive count;
- number of nodes/tiles loaded;
- cache hit rate and invalidation breadth;
- time to first useful image and final image;
- transition popping/error;
- coordinate error at depth;
- save/reopen and migration behavior;
- edit/undo behavior across levels;
- rendering time over empty versus densely occupied scale bands.

The most important negative control is an ordinary 2D document. If a 3D index
does not prune it, the scale dimension should cost almost nothing or remain
disabled.

## Working Recommendation

Adopt this vocabulary and architecture now:

- **2D source** for ordinary artwork;
- **order key** for painter's order;
- **continuous view scale** for the camera;
- **hierarchical `(level, x, y)` keys** for derived LOD and sparse caches;
- **coordinate-frame hierarchy** for precision and optional nested canvases;
- **semantic scale interval** only for explicitly scale-aware content;
- **scale-aware index** as a replaceable acceleration structure;
- **no dense 3D canvas texture**.

Then prototype two product behaviors on paper and in the future laboratory:

1. ordinary deep geometric drawing;
2. an explicitly nested/semantic-scale drawing in which zoom reveals a child
   world.

The result will tell us whether scale is merely a renderer/cache dimension or a
first-class artistic medium.

## Bottom Line

Treating scale as a third conceptual axis is insightful. It gives a clear way
to reason about multiscale visibility, queries, LOD, and drawings nested inside
drawings.

The best representation is unlikely to be physical XYZ geometry or a volume
texture. It is more likely:

> a 2D ordered document, organized by hierarchical coordinate frames, with a
> continuous scale query, optional semantic scale ranges, and disposable
> `(level, x, y)` render caches.

That model fits the rest of Sketchpad's architecture: canonical semantics stay
independent of the renderer, while the scale-aware index and caches can be
aggressively specialized and measured.
