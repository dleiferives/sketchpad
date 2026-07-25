# Research — GPU Stroke Rendering Deep Dive

Status: technical summary of one stroke-expansion method. It is not a selection
of Sketchpad's document model or renderer. See
[research-synthesis.md](research-synthesis.md) for the architecture context.

## Primary Reference

**"GPU-friendly Stroke Expansion"** — Raph Levien, Arman Uguray  
SIGGRAPH 2024 (ACM Transactions on Graphics)  
[arXiv: 2405.00127](https://arxiv.org/abs/2405.00127)  
[Full paper (HTML)](https://arxiv.org/html/2405.00127v2)

The authors are from Google, and the work is closely related to Vello's
compute-centric rendering research. It is an important modern method, not the
only valid GPU stroking architecture.

## Scope of the Paper

The paper solves a specific problem well:

```
stroked constant-style path
  → strongly correct expanded boundary
  → bounded line or arc approximation
  → input to a fill rasterizer
```

It does not define:

- tablet sampling, smoothing, or prediction;
- a pressure-varying artistic brush model;
- textured, soft, wet, or smudging paint;
- a retained document and undo model;
- ADF construction or incremental SDF updates;
- persistent document-space cache pages;
- layers, alpha, and color compositing.

This boundary is central. The expanded “line soup” is useful renderer input,
but converting it to a signed-distance cache still needs spatial candidate
lookup, inside classification, sampling/reconstruction error, and cache
invalidation.

## The Core Insight: Why Euler Spirals

### The Problem

Stroke rendering requires computing the **parallel curve** (offset curve) of a path:

1. Take the cubic Bézier path
2. Offset outward by half the stroke width
3. Offset inward by half the stroke width  
4. Connect with caps and joins
5. Fill the resulting outline

The parallel curve of a cubic Bézier is a **10th-order algebraic curve**. There is no analytic formula. You must approximate.

### Traditional Approaches (and their problems)

| Approach | Problem |
|---|---|
| Scale the control points (Tiller-Hanson) | Performs poorly for cubics, misses cusps |
| Cut-then-measure (Nehab 2020) | Iterative error sampling is expensive, error prone |
| Polar stroking (Kilgard 2020) | Angle-step metric has unbounded distance error at low curvature |
| Quadratic Bézier lowering | Can't represent inflection points, requires extra subdivision |

### The Euler Spiral Solution

An Euler spiral (clothoid / Cornu spiral) has curvature linear in arc length:

```
κ(s) = a·s + b
```

This is Cesàro form — curvature directly as a function of arc length. Key properties:

1. **Parallel curve has a closed-form Cesàro equation** (Wieleitner 1907):
   ```
   κ(s) = c₀·(s - s₀)^(-1/2) + c₁
   ```
   The parallel curve of an Euler spiral is also analytically expressible.

2. **Cusp detection is linear**: Solve `κ = ±1/offset` → a simple equation, not polynomial root-finding.

3. **Flattening has an invertible error metric**: The subdivision density integral
   ```
   n = ∫₀^ŝ √|κ(s)| ds  ·  √(1/(8d))
   ```
   is analytically solvable and invertible. No iterative sampling needed.

4. **Can model inflection points**: Quadratic Béziers can't; Euler spirals can (curvature crosses zero linearly).

5. **Arc approximation is nearly as simple**: `n = s · ∛(|κ'| / (120d))` — constant subdivision density.

## Pipeline Detail

### Stage 1: CPU Encoding (Tag Monoid)

Paths are encoded as two parallel GPU buffer streams:
- **Tag stream**: 8-bit tags per segment (verb type, subpath end, transform/style indices)
- **Data stream**: Point coordinates (variable-length per segment)

A parallel prefix sum (tag monoid) computes stream offsets on GPU. This is minimal CPU work — unlike existing renderers that do coordinate processing on CPU.

### Stage 2: GPU Stroke Expansion (One Dispatch)

Parallelized per-path-segment. No cross-thread communication needed.

**2a. Cubic Bézier → Euler spiral fit**

Geometric Hermite interpolation using a 7th-order polynomial (Appendix A in paper). The Euler spiral parameters `k₀`, `k₁`, and chord-to-arc ratio are computed from endpoint tangent angles `θ₀`, `θ₁`:

```
k₀ = θ₀ + θ₁
k₁ = 6Δ - Δ³/70 - Δ⁵/10780 + ...  (7th order in Δ = θ₁ - θ₀, and k)
```

**2b. Error estimation**

Three-term analytical error metric (no sampling):
1. Euler-spiral-to-cubic-fit error
2. Area difference between source cubic and Euler approximation  
3. Parametrization imbalance term

Total estimated error is the sum. Validated as conservative (never underestimates) and tight (mean estimated/true ratio = 1.656).

**2c. Adaptive subdivision without recursion**

The trick: encode the recursion stack in **two scalar values** — `t0_u` (scaled start) and `dt` (range size). Pushing = halving `dt`, doubling `t0_u`. Popping = using `countTrailingZeros(t0_u)` to determine how many levels to pop in one instruction.

This avoids GPU recursion entirely — no stack array, no register pressure, no divergence.

**2d. Parallel curve expansion**

For each accepted Euler spiral segment, generate two parallel curves (at `± half_width`). The flattening uses the invertible subdivision density integral to place subdivision points.

**2e. Cusp & evolute handling (strong correctness)**

When curvature crosses `1/(half_width)`, the parallel curve contains a cusp. Detection: on an Euler spiral, curvature is linear (`κ = a·s + b`), so finding where it crosses a threshold is a simple linear solve.

Evolutes (Section 6) are drawn to fix winding numbers when the stroke self-overlaps at high curvature. The evolute of an Euler spiral is another Euler spiral (`κ = -a⁻¹·s⁻³`, a log-aesthetic curve).

**2f. Caps & joins**

Each thread processes its segment + the join to the next segment. A special "stroke cap marker" segment at subpath boundaries handles start caps.

### Stage 3: Tile Sorting

The "line soup" output is an unordered list of line segments. A prefix-sum sorts them into 16×16 pixel tiles. This is analogous to the `cudaraster` / MPVG approach.

These are transient screen-rasterization work tiles. They are not evidence for
a particular persistent document-space SDF/ADF page size or lifetime.

### Stage 4: Fine Rasterizer

Per-tile scanline rasterization computing pixel coverage. Winding numbers are computed per-pixel and resolved with the nonzero fill rule.

## Key Mathematical Formulas

### Subdivision density integral (Euler spiral parallel curve)

```
f(x) = {
  ½(x·√|x²-1| + arcsin(x))              if |x| ≤ 1
  ½(x·√|x²-1| - arccosh(x) + π/4)       if x ≥ 1
}
```

This integral needs to be inverted for placing subdivision points. The paper provides a piecewise approximation from easily invertible functions (sin, polynomial). Maximum discrepancy ≈ 6% (two Newton iterations refine to float precision).

### Flattening error bound (circular arc)

Exact:
```
n = s·κ / (2·arccos(1 - d·κ))
```

Approximate (conservative):
```
n = s·√(κ / (8d))
```

### Arc approximation error bound

```
d ≈ (1/120) · (∫₀^ŝ ∛|κ'(s)| ds)³
```

For Euler spirals where κ' is constant:
```
n = s · ∛(|κ'| / (120d))
```

For Euler spiral parallel curves (adjusted for offset):
```
n = s · ∛(|κ'|·(1 + 0.4·|h·s·κ'|) / (120d))
```

## Comparison with Other Renderers

| Renderer | Stroke GPU? | Correctness | Primitive Type | Error Metric |
|---|---|---|---|---|
| Skia | CPU | Weak (no evolutes in most cases) | Quad Béziers + lines | Hybrid Wang + angle step |
| Nehab 2020 | CPU | Strong | Quad Béziers + lines | Sampling-based |
| Kilgard 2020 | GPU | Angular only | Lines | Angle step (not Fréchet bounded) |
| **This paper** | **GPU** | **Strong** | **Lines or arcs** | **Analytical, invertible** |
| Vello lineage | GPU | strong-correctness goal | Polyline/renderer-specific | related method |

The table compares the stroking subproblem, not whole drawing applications.
Production suitability also depends on compositing, allocation, caching,
incremental edits, device support, and brush semantics.

## Implications for Sketchpad

### Direct rendering baseline

The most direct use is to feed expanded outlines into a coverage rasterizer and
composite ordered strokes. That path should be measured—using
[Vello](https://github.com/linebender/vello) where practical—before assuming an
SDF cache is faster.

This 2024 pipeline should not be read as evidence that production geometry work
belongs entirely on the GPU. Vello's 2026 Hybrid development has moved
significant path processing to CPU SIMD and sends compact strip work to the
GPU. Its coarse-raster arrangement was then rewritten in July 2026 after CPU
overhead and filter-layer interactions proved costly. Sketchpad should keep
stroke expansion swappable and report CPU preparation separately from GPU time
on integrated and mobile-class hardware.

### Variable-width brushes remain research

Replacing one constant half-width with sampled pressure is not automatically a
correct variable-width algorithm. Width interpolation changes the swept
boundary, joins, cusps, and error bounds. Taper, calligraphic nib orientation,
and textured footprints need their own derivation or a different brush
representation.

### SDF/ADF construction is a separate pipeline

If expanded outlines are used to build a field, the design must answer:

- which outline segments can affect each document cell;
- how winding/inside state is computed;
- how distance and reconstruction error are bounded;
- how cells update after append, erase, reorder, or transform;
- whether construction cost is recovered by later redraw savings.

The paper supplies high-quality boundary primitives, not those answers.

### Active stroke versus committed scene

The live stroke may justify a specialized incremental path and transient
overlay. Finalized strokes can then enter the retained scene or render cache as
one logical operation. The expansion pipeline should not dictate the lifetime
of canonical input.

### Coverage remains a separate choice

The expanded boundary could feed:

- Vello-style sparse strips;
- Slug-style analytic quadratic-curve coverage;
- a conventional expanded mesh;
- an ADF builder if that cache later proves worthwhile.

The right comparison includes outline-generation time, coverage time, upload
bytes, memory, transform reuse, and visual error. A fast expander does not make
one of those coverage representations automatically best.

## Implementation References

- Vello source: https://github.com/linebender/vello
- Vello sparse strips:
  https://skia.googlesource.com/external/github.com/linebender/vello/+/refs/tags/sparse-strips-v0.0.9/sparse_strips/
- Slug analytic curve rendering: https://github.com/EricLengyel/Slug
- Ciallo brush-rendering tutorial:
  https://shenciao.github.io/brush-rendering-tutorial/
- wgpu docs: https://docs.rs/wgpu
- kurbo (curve math): https://docs.rs/kurbo
- winit (windowing): https://docs.rs/winit
- WGSL spec: https://www.w3.org/TR/WGSL/

## Additional Reading

- Nehab, D. (2020). "Converting Stroked Primitives to Filled Primitives." ACM Trans. Graph. 39(4). — The theory of strongly correct stroke rendering, which this paper's GPU implementation is based on.
- Kilgard, M. (2020). "Polar Stroking." ACM Trans. Graph. 39(4). — GPU stroking via angle steps. Faster but less accurate.
- Levien, R. (2021). "Cleaner parallel curves with Euler spirals." Blog post. — Precursor to this paper.
- Ganacim et al. (2014). "Massively-parallel vector graphics." ACM Trans. Graph. 33(6). — The MPVG tile-based GPU rasterizer that inspired vello's fine rasterizer.
- Laine & Karras (2011). "High-Performance Software Rasterization on GPUs." — cudaraster, the scanline approach.
