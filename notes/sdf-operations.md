# SDF Operations — Drawing, Erasing, and Beyond

## Core Principle

The canvas is a signed distance field. For every point, the SDF value `d` means:

- `d < 0`: inside a stroke (filled)
- `d = 0`: on the edge
- `d > 0`: empty / background
- `|d|`: distance to the nearest edge

Every tool is a CSG (constructive solid geometry) operation on this field.

## Drawing

A stroke is a shape with its own SDF. Drawing unions it into the canvas.

```
SDF_canvas = min(SDF_canvas, SDF_stroke)
```

`min(d1, d2)` takes whichever value is more negative — i.e., whichever shape claims the point is "more inside." If you draw a stroke over empty canvas, the stroke's negative interior overwrites the positive empty space. If you draw over an existing stroke, `min` naturally handles the overlap.

The stroke SDF comes from the GPU stroke expansion pipeline. For each cache page or flat-field region the stroke overlaps, the compute shader evaluates the distance from each sample to the expanded stroke geometry (polyline or arc segments from the Levien/Uguray algorithm), then does `min`.

```
Drawing compute shader:
    for each affected cache page or field region:
        for each sample in region:
            d_stroke = distance_to_stroke_outline(sample_pos)
            region[sample] = min(region[sample], d_stroke)
```

### Soft Brushes

A hard brush has a sharp SDF: `d = distance_to_circle(center, radius)` — goes from negative to positive in zero space.

A soft brush adds a fall-off zone:

```
d = distance_to_circle(center, radius)
d_soft = d + falloff_width  // shifts the zero-crossing outward
```

The gradient in the fall-off zone gives anti-aliasing for free. The eraser can also be soft — this gives a gradual erase rather than a hard cut.

## Erasing

Erasing removes material. In CSG terms, it's **subtraction**: remove the eraser shape from the canvas.

```
SDF_canvas = max(SDF_canvas, -SDF_eraser)
```

Why `max(d1, -d2)` works:

| `d_canvas` | `d_eraser` | `max(canvas, -eraser)` | Result |
|---|---|---|---|
| −2 (inside stroke) | −1 (inside eraser) | `max(−2, 1)` = 1 | Erased (now outside) |
| −2 (inside stroke) | 2 (outside eraser) | `max(−2, −2)` = −2 | Unchanged |
| 3 (empty) | −5 (inside eraser) | `max(3, 5)` = 5 | Still empty, nothing to erase |
| 3 (empty) | 2 (outside eraser) | `max(3, −2)` = 3 | Unchanged |

The negation flips the eraser's inside/outside. Inside the eraser, `−SDF_eraser` is positive, so `max` selects it, overwriting whatever was there with "outside" (erased). Outside the eraser, `−SDF_eraser` is negative, so `max` preserves the canvas value.

```
Eraser compute shader:
    for each affected cache page or field region:
        for each sample in region:
            d_eraser = distance_to_eraser_shape(sample_pos)
            region[sample] = max(region[sample], -d_eraser)
```

The eraser shape SDF is built the same way as a brush — same stroke expansion pipeline, same brush kernel, just used with a different CSG operation. A soft eraser naturally produces soft erased edges.

### Eraser as a Tool (not a Layer)

An eraser is a source operation applied to the current layer and an invalidation of the affected cache regions. The cache may be updated destructively for speed, but the source operation remains available for rebuild and undo.

## CSG Operations Reference

All operands are SDF values. All operations produce a valid (though not necessarily *exact*) SDF. The result is exact near edges, which is all we need.

| Operation | Formula | Use |
|---|---|---|
| **Union** (draw) | `min(d1, d2)` | Adding a stroke to the canvas |
| **Subtraction** (erase) | `max(d1, −d2)` | Removing material with eraser |
| **Intersection** | `max(d1, d2)` | Keeping only overlapping regions |
| **Smooth union** | `d1 + d2 − √(d1² + d2²)` | Blending two strokes smoothly |
| **Smooth subtraction** | `−(d1 − d2 − √(d1² + d2²))` | Soft erasing with a blend radius |

The smooth variants are useful for blending modes but come with a performance cost (square root per sample). Hard CSG (`min`/`max`) is a single instruction.

## SDF Exactness

CSG `min(d1, d2)` produces an *exact* SDF only when d1 and d2 are exact and the surfaces don't intersect in a way that creates a new minimal-distance feature. In practice, this means:

- Near edges, the SDF is exact (distance to the nearest stroke outline)
- Far from edges, the value has the correct sign but `|d|` may be slightly underestimated
- The display shader only cares about the neighborhood of `d ≈ 0` (the edge), so this is fine

For a drawing app, this is completely acceptable. The visual result is identical to an exact SDF because anti-aliasing only evaluates within ±1 pixel of the edge.

## Computing the Stroke SDF

When stamping a stroke into a cache page or flat field, we need `d_stroke = distance_to_stroke_outline(sample_pos)` for every affected sample.

The stroke outline is the filled polygon produced by the GPU stroke expansion pipeline — an unordered "line soup" of polylines forming closed filled regions.

For a single line segment, the distance from a point `p` to the segment `ab` is:

```
// WGSL
fn distance_to_segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let ab = b - a;
    let ap = p - a;
    let t = saturate(dot(ap, ab) / dot(ab, ab));
    let closest = a + t * ab;
    return length(p - closest);
}
```

For the full stroke polygon, we need the *signed* distance to the closed outline — negative inside, positive outside. This requires:

1. Compute unsigned distance to nearest segment edge
2. Determine sign via winding number or ray casting

Since the stroke outline is a closed polygon with consistent winding, the sign is determined by whether the point is inside the polygon. A ray-casting approach (count intersections with a horizontal ray) works but is expensive per-sample.

**Alternative approach: distance to the stroke's filled interior**

Instead of line soup, we can evaluate the stroke SDF by computing the distance to the stroke's *swept circle* — the Minkowski sum of the source Bézier curve with a circle of radius `half_width`. The distance to a swept circle (a capsule/rounded shape along the curve) is:

```
d_stroke = distance_to_curve_nearest_point(p) - half_width
```

Where `distance_to_curve_nearest_point` finds the closest point on the source Bézier to the sample point. This is analytically computable for lines and well-approximated for Béziers.

For the Euler spiral intermediate, the nearest-point query is tractable because curvature is linear in arc length. This is an area for research — it might be simpler than computing distance to a complex filled polygon.

**Practical approach for v1**: Use the line soup approach. The expanded stroke is subdivided into small enough segments (by the error-bounded flattening) that each segment approximates a small rectangle. The signed distance is:

```
d_stroke = distance_to_nearest_segment(sample_pos) - half_width
```

This overestimates slightly at joins (segments meet but the swept area extends past the segment endpoints) but is correct within the flattening tolerance. Joins and caps need separate distance evaluation for correctness.

## Cache Pages and Refinement

A cache page is a document-space storage unit, not a screen-space tile. A page stores samples at a defined document-space spacing. Pages may be sparse and may eventually exist at several refinement levels.

Refinement must not be driven only by a gradient threshold. A coarse SDF cannot recreate stroke detail that was lost during sampling. The source stroke/operation list must remain available so a finer page can be rebuilt by evaluating the source operations over that page.

For a given viewport, choose the finest available page whose projected sample spacing is appropriate for the screen pixel size. If it is missing, use a coarser valid page temporarily and mark the region for rebuild. Before adding more levels, define page bounds, border samples, invalidation, and the acceptable screen-space error.

## Undo

The source operation list is the canonical undo model. Cache-page snapshots can be used as a performance optimization, but they must not be the only record of an edit because finer pages may need to be rebuilt:

```
before drawing: record = source_operation
on undo:        remove operation and invalidate affected cache pages
```

Snapshotting small cache pages may still be useful for fast interactive undo, but the source operation remains authoritative. Older cache data can be discarded and rebuilt lazily.

## Color Data in the SDF Canvas

SDF cache pages store geometry (distance to nearest edge). To support color, each cache sample may also store **latent pigment vectors**.

```
Cache sample: (f32 sdf, vec3 latent)
  sdf:   signed distance to nearest edge
  latent: [c1, c2, c3] — pigment concentrations (4th implicit)
          residual r = RGB - mix(c) is stored alongside
```

The latent representation is from Mixbox (Sochorová & Jamriška, SIGGRAPH 2021). See `notes/pigment-mixing.md` for the full treatment.

When stamping a brush stroke:
1. **Geometry**: `min(canvas_sdf, brush_sdf)` — same as before
2. **Color**: Where brush_sdf < 0 (inside stroke), `lerp(canvas_latent, brush_latent, opacity)`

When erasing, color is removed along with the SDF (erased region reverts to empty/transparent).

The decode from latent → sRGB happens at display time in the fragment shader, via the Mixbox polynomial evaluator. This means layers can be composited with correct pigment mixing behavior.

## Future: Blend Modes

The CSG operations extend naturally. For example, multiply blend mode would be:

```
SDF_canvas = multiply_blend(SDF_canvas, SDF_brush, color_canvas, color_brush)
```

This is harder because SDF only stores geometry, not color. For blend modes that depend on underlying color, you need the color at each point too. One approach: store color alongside the SDF in each tile (RGBA + distance = 5 values per sample). The display shader then has both geometry and color.

This is deferred to a future design pass — standard draw/erase covers 90% of drawing needs.
