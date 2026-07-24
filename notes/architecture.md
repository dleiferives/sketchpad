# Sketchpad — Architecture & Research Notes

## Goal

A blazing-fast cross-platform drawing app. Custom GPU stroke rendering, not reliant on library stroke APIs. Desktop + mobile (Vulkan/Metal/DX12 via wgpu).

## Stack Decision

**Rust + wgpu + WGSL** — no raw Vulkan, no C++.

| Layer | What | Why |
|---|---|---|
| Language | Rust | Safety, performance, cargo ecosystem |
| GPU API | `wgpu` | Cross-platform — compiles to Vulkan, Metal, D3D12, WebGPU |
| Shader language | WGSL | wgpu native, compiles to SPIR-V/MSL/DXIL |
| Windowing | `winit` | Events, stylus input, platform surfaces |
| Curve math | `kurbo` | Bézier paths, shapes, affine transforms |
| Colors/brushes | `peniko` | Gradients, images, color types |

Vello (`vello`) sits on top of this stack and provides a complete 2D vector renderer. However, Vello's `Scene::stroke()` is a library call — fine for SVG strokes but not extensible for custom brush engines. We are implementing our own stroke pipeline instead.

## Conceptual Architecture

```
┌─────────────────────────────────────┐
│  App Layer (layers, undo, UI)       │
├─────────────────────────────────────┤
│  Custom Stroke Pipeline (WGSL)      │  ← we write this
│  ┌───────────────────────────────┐  │
│  │ Bézier → Euler spiral        │  │
│  │ Parallel curve expansion     │  │
│  │ Cusp / evolute handling      │  │
│  │ Caps, joins, dashing         │  │
│  │ Flattening to line soup      │  │
│  │ Brush customization (width,  │  │
│  │   texture, pressure, taper)  │  │
│  └───────────────────────────────┘  │
├─────────────────────────────────────┤
│  Tile-based rasterizer (WGSL)       │
├─────────────────────────────────────┤
│  wgpu                               │
├─────────────────────────────────────┤
│  winit (window, input, stylus)      │
├─────────────────────────────────────┤
│  Vulkan / Metal / D3D12 / WebGPU    │
└─────────────────────────────────────┘
```

## The Stroke Algorithm

Based on: **"GPU-friendly Stroke Expansion"** (Levien & Uguray, SIGGRAPH 2024).
This is the paper behind Vello's stroke pipeline.

### Key Idea

The standard problem: parallel curves (offset curves) of cubic Béziers are 10th-order algebraic curves — analytically intractable, numerically fragile.

The solution: **Euler spirals** as an intermediate representation. An Euler spiral has curvature linear in arc length (`κ(s) = a·s + b`), which makes:

- Parallel curves analytically tractable via Cesàro equations
- Cusp detection a simple linear equation (instead of polynomial root-finding)
- Flattening (approx to line segments) has an invertible error metric — no sampling loops
- Arc approximation similarly cheap and produces ~3× fewer primitives than lines

### Pipeline Steps

1. **CPU encoding**: Paths → compact tag stream (tag monoid, prefix-sum). Minimal CPU work.
2. **GPU stage 1 — Stroke expansion compute shader** (per-path-segment, 1 dispatch):
   - Cubic Bézier → Euler spiral fit (7th-order Hermite interpolation)
   - Error estimation (closed-form, no iterative measurement)
   - Adaptive subdivision (stack encoded in 2 words, no recursion)
   - Parallel curve expansion (± half-width offset)
   - Cusp detection + evolute patches for strong correctness
   - Caps (butt/square/round), joins (bevel/miter/round)
   - Flatten Euler spiral → polylines or arcs
   - Output: unordered "line soup" in GPU storage buffer
3. **GPU stage 2 — Tile sorting** (prefix-sum based, spatial sort into 16×16 tiles)
4. **GPU stage 3 — Fine rasterizer** (scanline coverage per tile)

### Strong vs Weak Correctness

- **Weak**: parallel curves + caps + outer join contours only
- **Strong**: also includes *evolutes* and *inner join contours* when curvature > 1/half-width
  - Without these, high-curvature regions have visual artifacts (missing geometry, winding errors)
  - The paper achieves strong correctness with minimal added cost via the Euler spiral representation

### Error Metrics

- Flattening tolerance: 0.25 device pixels (sub-visual threshold)
- Arc approximation: ~3× fewer primitives than lines at same tolerance
- All metrics are conservatively bounded and computed analytically (no sampling)

### Performance Numbers (from paper)

| Scene | Input Segments | GPU (M1 Max) | Output (lines) | Output (arcs) |
|---|---|---|---|---|
| waves.svg | 13,308 | ~0.5ms | 475,855 | 181,229 |
| mmark-70k | 70,000 | ~3ms | 1,577,705 | 1,162,117 |
| mmark-120k | 120,000 | ~5ms | 2,709,013 | 1,994,946 |
| long dash (round) | 503,304 | ~9ms | 2,625,300 | 1,672,561 |

On mobile (Pixel 6 / Mali-G78): waves.svg in ~3.5ms. Still real-time.

## Custom Brush Extension Points

Where the paper's algorithm can be extended for custom brush behavior:

- **Variable width**: Feed pressure/velocity into the offset parameter (the `± half-width` in step 2)
- **Texture along stroke**: Sample a texture in the rasterizer stage using arc-length parameter
- **Taper**: Modify offset at stroke endpoints (this falls out naturally from the parallel curve evaluation)
- **Brush simulation**: Run additional compute passes between stroke expansion and rasterization
- **Smudge / liquify**: Post-process the rasterized output with a separate compute shader

## Mobile Strategy

- **Android**: `cargo apk` — Vello's `with_winit` example already supports this
- **iOS**: `wgpu-in-app` crate for embedding wgpu into UIKit/SwiftUI
- Same WGSL shaders run everywhere — the GPU pipeline is 100% portable

## Alternative Considered: C++ + Skia

- Skia is production-grade (used by Chrome, Android, Flutter)
- Vulkan backend exists, stylus support exists
- BUT: C++ build complexity, Skia's stroke API is not designed for GPU-compute customization
- Skia's stroke expansion is CPU-side and doesn't expose the parallelism the paper exploits
- Rust + custom WGSL gives us control over every stage of the pipeline

## Open Questions

- [ ] Layer compositing strategy (Vello's push_layer/pop_layer vs custom)
- [ ] File format (.sketchpad custom binary? SVG? OpenRaster?)
- [ ] Undo/redo system architecture
- [ ] Text rendering (path text vs glyph atlas)
- [ ] Color management / HDR support
- [ ] UI toolkit for tools panel, menus, etc.
