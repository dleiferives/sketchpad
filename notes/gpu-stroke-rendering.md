# Research — GPU Stroke Rendering Deep Dive

## Primary Reference

**"GPU-friendly Stroke Expansion"** — Raph Levien, Arman Uguray  
SIGGRAPH 2024 (ACM Transactions on Graphics)  
[arXiv: 2405.00127](https://arxiv.org/abs/2405.00127)  
[Full paper (HTML)](https://arxiv.org/html/2405.00127v2)

Authors are from Google. Raph Levien is also the creator of Vello (formerly piet-gpu), kurbo, peniko, and the Xilem GUI toolkit. This is the source of truth for the modern GPU stroke rendering approach.

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
| Vello | GPU | Strong | Polyline | Same algorithm |

## Implementation References

- Vello source: https://github.com/linebender/vello
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
