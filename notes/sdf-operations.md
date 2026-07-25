# Signed-Distance Operations

Status: geometry reference and research questions. This note no longer assumes
that the entire painted canvas is one SDF. See
[research-synthesis.md](research-synthesis.md) for the architecture comparison.

## Scope

Signed distance is a useful representation for:

- hard inside/outside geometry;
- antialiasing around a known boundary;
- offsets, outlines, and some shape effects;
- constructive geometry such as union and subtraction;
- adaptive sampling when paired with a reconstruction/error scheme.

Signed distance alone does not encode:

- colors of multiple overlapping strokes;
- translucent draw order;
- accumulated airbrush opacity;
- textured paint and smudge state;
- layer compositing;
- a logical stroke or undo history.

Those facts determine where an SDF is appropriate: as source geometry for
specific operations, as a derived geometry cache, or as one channel within a
richer representation—not automatically as the document.

## Definition

For an exact signed Euclidean distance field `d(p)`:

- `d < 0`: the point is inside the represented set;
- `d = 0`: the point is on its boundary;
- `d > 0`: the point is outside;
- `|d|`: shortest Euclidean distance to the boundary.

A sampled texture stores values at discrete positions. Reconstruction between
samples is approximate. Its usable detail is bounded by sample spacing and the
reconstruction method.

## Boolean Operations

For exact operand fields:

| Set operation | Common field expression |
|---|---|
| union | `min(d1, d2)` |
| intersection | `max(d1, d2)` |
| difference `A \ B` | `max(dA, -dB)` |

These expressions correctly represent the resulting inside/outside set under
the usual sign convention. The result is not necessarily an exact Euclidean
distance everywhere, particularly where operand boundaries interact. If a
later algorithm depends on accurate magnitude or gradients far from the
boundary, re-distancing or direct source evaluation may be necessary.

For edge antialiasing, a bounded error near the visible contour may be
acceptable. That tolerance must be measured in projected screen space, not
asserted globally.

## Hard Geometry Drawing

If a layer intentionally represents one opaque binary shape, adding a stroke
shape by union is coherent:

```
d_layer = min(d_layer, d_stroke)
```

Similarly, hard geometric subtraction is:

```
d_layer = max(d_layer, -d_eraser)
```

This is a good match for:

- a silhouette/mask layer;
- boolean shape construction;
- an opaque same-color ink medium;
- a derived union field used only to accelerate hit testing or coverage.

It is not enough for differently colored or partially transparent overpainting.
Once the layer has been reduced to the union boundary, information about
internal overlaps and operation order has been discarded.

## Softness Is Coverage, Not a Shift

The former note proposed:

```
d_soft = d + falloff_width
```

Adding a constant offsets the zero contour. It changes the apparent shape or
radius; it does not create a soft opacity profile by itself.

A simple soft edge converts distance to coverage. For example, with `d = 0` at
the outer boundary and softness width `w` inside it:

```
coverage(d) =
    1                          when d <= -w
    1 - smoothstep(-w, 0, d)  when -w < d < 0
    0                          when d >= 0
```

The precise function may use a brush-specific curve, texture, accumulated dabs,
or filtered integration. Important distinctions:

- **Geometric coverage** describes how much of a sample is covered.
- **Brush flow** describes paint deposited per unit time/distance.
- **Brush opacity** limits the stroke’s total opacity.
- **Layer opacity** applies after the layer is rendered.

Collapsing these values makes airbrush accumulation, soft erasing, and undo
semantics ambiguous.

## Antialiasing

Distance can be mapped to edge coverage over approximately one screen-pixel
filter width:

```
coverage = filter(d / document_units_per_pixel)
```

This does not make sampled SDF zoom unlimited. If source curvature or features
are smaller than the field’s sampling/reconstruction error, no display filter
can recover them. A deep-zoom design must refine from canonical geometry.

## Eraser Models

### Coverage erase

For a premultiplied raster layer, an eraser normally lowers destination alpha
and premultiplied color. Conceptually this is a destination-out operation or
equivalent coverage update. A soft eraser varies erase coverage.

This preserves raster-paint expectations but edits pixels rather than retained
geometry.

### Whole-stroke erase

The eraser selects retained strokes and removes them as document operations.
It is fast and editable but visually coarse: touching a stroke may remove the
entire object.

### Split/point erase

The eraser subtracts geometry from retained strokes and replaces each affected
stroke with zero or more fragments. This preserves localized visual behavior
and later editability, but curve/mesh intersection and brush reconstruction can
be expensive.

### Mask erase

The eraser writes a retained or raster mask. This is nondestructive but adds
mask ordering, transforms, cache, and editing semantics.

### Hard SDF subtraction

`max(d_layer, -d_eraser)` is the appropriate implementation only if the
canonical or derived layer really is one binary geometric set. It should not be
used as shorthand for all four eraser models.

## Constructing a Stroke Distance Field

The Levien/Uguray stroke-expansion pipeline produces strongly correct line or
arc outlines suitable for rasterization. It does not directly produce an
incrementally updateable distance field.

Possible construction strategies follow.

### Direct distance to a swept centerline

For a constant-radius line segment:

```
d(p) = distance_to_segment(p, a, b) - radius
```

This naturally describes a capsule. Variable width, joins, cusps, self
intersections, and general Bézier nearest-point queries make the full case more
complex. A claim that nearest-point evaluation on an Euler spiral is
“tractable” needs a concrete bounded algorithm before it guides implementation.

### Distance to an expanded outline

Given a closed expanded outline:

1. find the nearest candidate edge for unsigned distance;
2. determine inside/outside by winding or crossings;
3. apply cap/join/evolute semantics correctly;
4. bound candidates through a spatial index.

Naively evaluating every outline segment at every cache sample is
`O(samples × segments)` and is not a viable architecture for dense drawings.
Screen-space tile binning from a rasterizer paper is useful precedent, but it
does not automatically define persistent document-space cells or invalidation.

### Adaptive sampling from retained source

An ADF can recursively sample a procedural field and subdivide cells whose
reconstruction error is too large. A production 2D design still needs to define:

- cell samples and reconstruction polynomial;
- error estimator and termination rule;
- maximum depth and numerical coordinates;
- neighbor/border rules;
- source-operation lookup for each cell;
- incremental edits and history;
- memory layout and GPU traversal;
- behavior when a field is only approximately signed distance after CSG.

Until these are specified, “sparse ADF pages” is a research direction rather
than a storage design.

## Distance and Ordered Color

Consider two half-opacity strokes of different colors that cross. The correct
result depends on:

- which stroke was drawn first;
- each stroke’s intrinsic opacity;
- geometric coverage at the pixel;
- the working color space;
- layer and blend semantics.

The union distance only answers whether the union has a boundary nearby. A
single accompanying color sample is already the result of some flattening
policy and cannot later reconstruct the original ordered operations.

Coherent alternatives include:

- render ordered retained primitives directly;
- keep ordered primitives per spatial cell and evaluate the local stream;
- flatten into premultiplied RGBA raster tiles;
- store separate per-operation/per-layer fields and composite explicitly.

## Cache Pages

If an SDF cache is pursued, a page must have an explicit contract:

```
Page key
  document-space region
  refinement level
  layer/source revision

Page data
  reconstruction samples/coefficients
  border or neighbor information
  conservative error bound
  optional empty/full classification

Page state
  missing | approximate | valid | stale | building
```

Questions that precede implementation:

- Are levels nested or independent?
- Are fine pages rebuilt from source or derived from parents?
- How are cracks prevented at mixed-resolution boundaries?
- What projected error makes a page displayable?
- How does an edit invalidate ancestors, descendants, and neighbors?
- What happens during reorder, recolor, transform, undo, or device loss?
- Does minification use the distance hierarchy or a separate raster mip cache?

## Current Prototype

The current field is a fixed 1024×1024 CPU array. Circle operations update a
bounded sample range using `min`; display then uploads the complete field when
dirty.

This verifies a narrow path:

```
circle source operation
  → local sampled hard-shape union
  → full texture upload
  → bilinear distance display
```

It does not yet test:

- exact field maintenance after complex CSG;
- logical stroke storage;
- colored/translucent overlap;
- adaptive cells;
- page borders or LOD;
- GPU field construction;
- local GPU uploads;
- erasing and undo;
- soft brush accumulation.

One implementation detail to keep in future diagnostics: sampling regions must
reject shapes wholly outside the canvas before clamping integer bounds.
Clamping first can make an out-of-bounds shape touch a border sample.

## Research Questions

1. Is an SDF canonical, derived, or only a temporary coverage primitive for each
   proposed layer type?
2. Which required marks are binary hard geometry?
3. What ADF cell and reconstruction scheme is credible for 2D GPU use?
4. How are retained operations spatially assigned while preserving order?
5. What exact update cost follows append, erase, reorder, transform, and undo?
6. When does direct vector rendering outperform cache construction?
7. What representation handles minification?
8. Which algorithms need true distance magnitude versus only sign and local
   coverage?
9. What numerical coordinate model supports the desired deep zoom?
10. What worst-case drawing forces global refinement or operation replay?

## Primary References

- Frisken et al.,
  [Adaptively Sampled Distance Fields](https://www.merl.com/publications/TR2000-15)
- Frisken and Perry,
  [Designing with Distance Fields](https://www.merl.com/publications/TR2006-054)
- Perry,
  [ICE Technology Overview](https://www.ronaldperry.org/IP_Package/ICE_Technology_Overview.pdf)
- Perry,
  [ICE Drawing API](https://www.ronaldperry.org/IP_Package/ICE_API.pdf)
- Levien and Uguray,
  [GPU-friendly Stroke Expansion](https://arxiv.org/abs/2405.00127)
- Nehab and Hoppe,
  [Random-access Rendering of General Vector Graphics](https://hhoppe.com/proj/ravg/)
