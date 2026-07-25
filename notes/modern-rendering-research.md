# Modern Rendering, Stroke, and Latency Research

Status: primary-source research snapshot, 2026-07-24. Most custom analytic,
multiscale, and simulation work here is post-MVP unless a narrow result directly
improves the sparse-raster painter in
[first-usable-product.md](first-usable-product.md). This note surveys techniques
that could materially change Sketchpad's renderer. The architecture
consequences are hypotheses until they survive representative workloads on the
target machines.

## Executive Summary

There is no single modern technique that dominates all of Sketchpad's likely
marks. The strongest opportunity is to keep one editable, deterministic mark
description while allowing its derived renderer to vary:

```
canonical mark
  input samples + brush graph + seed + transform + order
                         │
                         ▼
                  backend selection
       ┌────────────┬────────────┬────────────┬────────────┐
       │ analytic   │ sparse     │ density /  │ local      │
       │ hard edge  │ vector     │ stamp      │ simulation │
       └────────────┴────────────┴────────────┴────────────┘
                         │
                         ▼
             coverage/color tiles or passes
                         │
                         ▼
                  ordered composition
```

This is an inference from the sources, not a claim that an existing renderer
already supplies the system.

The most promising concrete findings are:

1. **Hard-edged vector marks can be evaluated directly from curves.** Slug
   renders quadratic Bézier outlines from compact curve and band data, with
   analytic antialiasing and without a per-zoom bitmap. In March 2026 its author
   announced that the patent had been dedicated to the public domain and
   released updated reference shaders under permissive licenses. This makes a
   previously awkward technique materially more usable, subject to normal
   license/NOTICE review.

2. **Some soft brushes need not be thousands of dabs.** Ciallo treats the
   editable source as a polyline but provides GPU renderers for capsules,
   textured stamps, and dense airbrushes. Its airbrush derivation replaces a
   long product of alpha stamps with a continuous product integral. That is a
   compelling model for a continuous-density brush backend.

3. **The CPU/GPU boundary is still moving.** Vello Hybrid has explored CPU
   SIMD path processing followed by compact strip data on the GPU. A rewrite
   merged on 2026-07-24 removes an earlier coarse-raster stage because its CPU
   overhead and interaction with filter layers were poor. Sketchpad should
   standardize semantics and benchmarks before standardizing one work
   distribution or cache layout.

4. **Latency needs a separate active path.** Platform ink systems combine
   coalesced real input, replaceable prediction, and sometimes front-buffer
   rendering. The committed prefix of a stroke can become stable while only
   the predicted tail is redrawn. The live path and the retained document
   should therefore share semantics but need not share a renderer.

5. **Intermediate coverage or geometry is often a better cache than final
   RGBA.** It can survive recoloring and some compositing changes. Final raster
   mips are still useful for distant views, complex filters, and expensive
   media. Cache choice should follow reuse and invalidation, not ideology.

6. **A small simulation can be a brush backend rather than the whole canvas.**
   Recent real-time thin-film work makes wet paint and drips plausible in
   bounded regions. The durable document can retain events, parameters, and a
   seed or explicitly bake a result; every layer need not become a fluid grid.

## Workloads Must Be Separated

“Stroke rendering” currently hides several different problems:

- collecting irregular, predicted, and coalesced input;
- smoothing or fitting a centerline;
- evaluating a time-varying brush recipe;
- expanding a centerline into a boundary;
- computing pixel coverage for a boundary;
- sampling or integrating a soft/textured footprint;
- preserving ordered transparent composition;
- maintaining a spatial index and caches;
- presenting the active stroke with low latency;
- rebuilding distant or evicted content;
- simulating stateful media such as wet paint.

A paper solving one row does not solve the others. In particular, fast
path-to-outline expansion is not a document model, and an analytic coverage
shader is not a brush engine.

## Technique Survey

### 1. Slug-Style Analytic Curve Coverage

The [Slug paper](https://jcgt.org/published/0006/02/02/paper.pdf) renders closed
outlines represented by quadratic Bézier curves. Curves are organized into
horizontal and vertical bands. A fragment examines only curves in the relevant
band, classifies winding and local boundary contribution, and computes
antialiasing from the actual curve geometry.

The current [reference implementation](https://github.com/EricLengyel/Slug)
stores curve data in a four-channel 16-bit-float texture and band indices in a
two-channel 16-bit unsigned-integer texture. Curves within a band are sorted to
permit early termination. The 2026 update also computes a perspective-aware
half-pixel dilation in the vertex shader.

The author's
[March 2026 announcement](https://terathon.com/blog/decade-slug.html) says the
related US patent was dedicated to the public domain effective March 17, 2026.
The repository offers MIT and Apache-2.0 licensing and carries attribution
requirements. This note records the primary-source claim; it is not legal
advice.

**Potential Sketchpad role**

- crisp finalized ink represented as closed outlines;
- imported vector fills and possibly text;
- a hard-edge backend at arbitrary view transforms;
- tile- or cell-local curve bands rather than one global glyph band table.

**What it does not settle**

- generation of correct variable-width outlines;
- cubic-to-quadratic conversion and its error tolerance;
- self-intersection and eraser semantics upstream;
- soft, textured, wet, or smudging brushes;
- ordered layer caching;
- whether curve loops per covered pixel beat sparse strips on target hardware.

The useful experiment is not “implement Slug as the renderer.” It is “compare
analytic curve coverage with sparse strips and expanded meshes for the
hard-edge mark corpus.”

### 2. Ciallo and Continuous Brush Evaluation

[Ciallo](https://researchportal.hkust.edu.hk/en/publications/ciallo-gpu-accelerated-rendering-of-vector-brush-strokes-2/)
keeps a polyline as the editable source and renders several brush families on
the GPU. Its
[tutorial](https://shenciao.github.io/brush-rendering-tutorial/) describes:

- instanced capsule/trapezoid geometry for a basic round brush;
- cumulative-length prefix sums so textured stamps are placed by distance
  rather than by the density of input vertices;
- bounded per-fragment evaluation of only stamps that could cover the pixel;
- a continuous integral for very dense airbrush stamps.

For a constant-density circular airbrush, repeated alpha application is
rewritten as a product integral. In simplified form:

```
A(x, y) = 1 - exp(-∫ alpha_s(x - l, y) dl)
```

Inside a straight “bone,” the intersection length through a disk produces:

```
A(y) = 1 - exp(-2 alpha_c sqrt(R² - y²))
```

The significant idea is not this one formula. A brush whose visual effect is
the accumulation of many overlapping footprints may sometimes be compiled
into a continuous density kernel instead of submitting every dab.

The [research repository](https://github.com/ShenCiao/CialloResearch) is
GPL-3.0. It also corrects an important detail in the paper: the implementation
uniformly samples parametric curves in `t`; it does not first provide a true
arc-length parameterization. A clean-room implementation and license review
would be required before product use.

**Potential Sketchpad role**

- analytic airbrush and marker backends;
- distance-spaced textured stamps with stable density;
- a reference design for compiling one brush recipe to different kernels;
- a fast active-stroke renderer when full final geometry is not yet available.

**Failure modes to measure**

- sharp corners and quickly varying radius;
- input density and path-fitting sensitivity;
- variable opacity, anisotropic nibs, and noncircular footprints;
- self-overlap semantics;
- WebGPU portability if an implementation depends on geometry shaders or
  nonportable texture access.

### 3. Strongly Correct Stroke Expansion

Levien and Uguray's
[GPU-friendly stroke expansion](https://linebender.org/gpu-stroke-expansion-paper/)
uses Euler spirals and analytical error bounds to convert stroked paths to
lines or circular arcs in parallel. It addresses cusps and evolutes more
carefully than common flattened-offset methods. The
[paper code](https://github.com/linebender/gpu-stroke-expansion-paper) is a
useful correctness and performance baseline.

A separate
[2025 piecewise-quadratic method](https://www.sciencedirect.com/science/article/pii/S1524070325000426)
uses curvature-guided subdivision and Newton iteration for arc-length
parameterization. Its reported speedups are against the paper's selected
baselines, not proof of whole-canvas performance.

These techniques solve centerline-to-boundary conversion. They remain valuable
even if the resulting boundary is fed to Slug-style coverage, Vello-style
strips, a mesh rasterizer, or a distance-field builder.

**Working conclusion:** keep stroke expansion behind an interface and measure
CPU and GPU variants. “GPU” does not automatically mean lower latency on an
integrated Intel GPU or a mobile system sharing memory and power budgets.

### 4. Vello, Sparse Strips, and a Moving CPU/GPU Boundary

[Vello](https://github.com/linebender/vello) is the most important direct-vector
baseline in the Rust/WGPU ecosystem. It can also be a source of components
rather than an all-or-nothing commitment.

The
[sparse-strips implementation](https://skia.googlesource.com/external/github.com/linebender/vello/+/refs/tags/sparse-strips-v0.0.9/sparse_strips/)
explores CPU path processing, tiling/sorting, compact strip representations,
and GPU composition. Strip geometry can be cached independently of final
color, and simple translations may be cheaper to reuse than scale or skew.

However, the current design is not frozen. Vello
[commit `c334386` on 2026-07-24](https://skia.googlesource.com/external/github.com/linebender/vello/+/c33438626fbd307bdc7b158566c028d16355b1d7)
contains a Hybrid rewrite that removes an earlier CPU coarse-raster step.
According to its commit description, that step consumed too much CPU and
interacted badly with filter layers that needed complete offscreen results.
The new path sends a less processed “soup of strips” to the GPU.

**Lessons for Sketchpad**

- benchmark CPU SIMD preprocessing plus GPU composition on the actual Atlas
  GPU and intended mobile class;
- cache reusable intermediate geometry or coverage when its invalidation
  boundary is clearer than RGBA;
- do not freeze a universal coarse-tile representation before defining
  filters, groups, masks, and blend semantics;
- use Vello as a baseline before custom work, but do not treat a changing
  renderer as the canonical document format.

### 5. Bézier Splatting and Gaussian Primitives

[Bézier Splatting](https://arxiv.org/abs/2503.16424) samples anisotropic
two-dimensional Gaussians along Bézier curves. The representation is
differentiable and maps well to modern GPU splatting hardware patterns. Its
reported forward/backward speedups are for differentiable rendering of open
curves against DiffVG in the authors' research setting.

This is not evidence that Gaussian splats should replace a production vector
rasterizer. It is interesting for:

- airbrush, chalk, glow, and other naturally soft marks;
- fitting or optimizing imported/vectorized marks;
- a common soft primitive that supports varying width and opacity;
- approximating expensive kernels at distant levels of detail.

Hard boundaries, exact closed-shape fills, alpha-order correctness, sample
count, and editing behavior still need independent treatment.

### 6. Local Thin-Film Simulation

The 2026
[Dripping Thin Films](https://eliemichel.github.io/dripping-thin-films/)
work models a height-field fluid with pigment advection, diffusion, and mixing
in real time. Adobe's
[publication page](https://research.adobe.com/publication/dripping-thin-films-for-real-time-digital-painting/)
frames it as interactive digital painting.

The architectural opportunity is a **simulation island**:

- allocate state only in wet-media tiles or bounded active regions;
- give the simulation explicit parameters, versions, and deterministic seeds
  where possible;
- preserve the causal brush events or deliberately bake a raster result;
- composite the island with ordinary retained and raster layers.

Making the entire infinite canvas a fluid domain would destroy most of the
sparsity advantage and make deterministic undo, replay, and migration much
harder.

## Input-to-Photon Architecture

Google's
[Ink Stroke Modeler](https://android.googlesource.com/platform/external/ink-stroke-modeler/)
combines smoothing, upsampling, and prediction. Its output has a useful
stable-prefix property: committed results do not change when later input is
added, while predictions become invalid as soon as new real input arrives.

Android's
[advanced stylus guidance](https://developer.android.com/develop/ui/views/touch-and-input/stylus-input/advanced-stylus-features)
separates hardware/OS latency, application rendering latency, and compositor
latency. Front-buffer rendering can present a small changed region quickly,
with tearing risk, then commit to the ordinary double-buffered surface when
the gesture ends. Predictions must be visually replaced, never treated as
final input. Apple exposes analogous
[predicted touches](https://developer.apple.com/documentation/uikit/incorporating-predicted-touches-into-an-app).

This supports a three-part stroke:

```
stable committed prefix | unstable real tail | predicted tail
       cacheable         | redraw locally     | always replaceable
```

The active overlay should be composited above the authoritative retained
document. On finalization it becomes one semantic operation and the overlay is
removed only after authoritative pixels or geometry are ready.

Measurements must distinguish:

- event timestamp to application receipt;
- application receipt to submitted work;
- submitted work to visible presentation;
- p50 and p95 latency;
- changed pixels and upload bytes;
- visible correction when predictions are replaced.

## Cache Algebra and Its Limits

The W3C
[Compositing and Blending specification](https://www.w3.org/TR/compositing-1/)
defines premultiplied source-over and notes group invariance for simple normal
source-over composition. In that restricted case:

```
A + B + C = A + (B + C) = (A + B) + C
```

This suggests an **ordered reduction cache** for a dense tile: leaves represent
ordered batches of marks, internal nodes cache their source-over composite, and
an edit recomputes the path from one leaf to the root. It preserves order while
potentially reducing an edit from replaying every mark to logarithmic
recomposition.

This is a working hypothesis with serious constraints:

- a full tree stores nearly twice as many tile images as leaves;
- non-normal blend modes, masks, isolation, group opacity, filters, and many
  Porter-Duff operators can break simple reassociation;
- wet paint, smudge, and destination-dependent brushes are not independent
  source-over leaves;
- sparse or block-adaptive trees may save memory but add scheduling and
  fragmentation costs.

The Vello Hybrid rewrite is a practical warning: an optimization that looks
good for paths and ordinary layers can become wrong-shaped for filters.

Candidate cache products, from most semantic to most baked:

| Cache | Survives recolor | Survives transform | Handles filters cheaply | Main cost |
|---|---:|---:|---:|---|
| fitted path / outline | yes | usually | no | repeated rasterization |
| curve bands / sparse strips | often | translation best; others vary | no | coverage work |
| coverage tile | yes if separated | limited | sometimes | memory and resampling |
| composited RGBA tile | no | limited | yes after bake | invalidation/replay |
| distant raster mip | no | view-range only | yes | refinement and storage |

No one row should be universal. Each cache needs a source revision, transform
scope, memory cost, rebuild path, and visual-error policy.

## Deep Zoom and GPU Numerical Range

The [WGSL specification](https://gpuweb.github.io/gpuweb/wgsl/) exposes `f32`
and optional `f16`, not `f64`, as concrete floating-point types. Large absolute
world positions therefore cannot remain distinguishable indefinitely on the
GPU.

The
[SVG implementation notes](https://www.w3.org/TR/SVG/implnote.html) recommend
splitting high-precision content into tiles with per-tile local coordinates and
transforms. Camera-relative high/low representations, such as
[Cesium's relative-to-eye transform](https://cesium.com/downloads/cesiumjs/releases/b20/Documentation/czm_translateRelativeToEye.html),
are another established pattern.

The likely Sketchpad model is:

- wide or hierarchical coordinates in the canonical document;
- spatial cells with local origins;
- local `f32` geometry and camera-relative GPU transforms;
- screen-space error tolerances for fitting, coverage, and LOD;
- raster mips only where a baked approximation is acceptable.

This needs a numerical error budget, not an “infinite zoom” slogan.

## WebGPU Portability Constraints

Current
[`wgpu` storage texture documentation](https://docs.rs/wgpu/latest/wgpu/enum.StorageTextureAccess.html)
marks read-only and read-write storage-texture access as adapter-specific
features; read-write access is not a portable WebGPU assumption. Portable
algorithms should prefer:

- storage buffers for general read/write data;
- render passes when blending and raster hardware already express the work;
- ping-pong textures rather than in-place texture feedback;
- capability-gated native fast paths;
- explicit device-loss rebuilds from canonical state.

If global prefix scans remain useful, the 2025
[Decoupled Fallback](https://escholarship.org/uc/item/0bk9z4bt) paper shows a
portable single-pass WebGPU scan that avoids relying on forward-progress
guarantees or 64-bit atomics. It is an enabling primitive, not a reason to move
the whole renderer to compute.

## The Most Interesting Architectural Exploit

The strongest research hypothesis is a **brush compiler**, not a new universal
primitive.

An immutable brush graph describes:

- source signals: pressure, speed, tilt, orientation, time, distance, and
  prediction status;
- filtering and normalization;
- arithmetic and response curves;
- terminals such as width, opacity, color, footprint angle, spacing, wetness,
  and completion behavior;
- a version and deterministic random seed.

The graph and real input are canonical. A derived classifier chooses:

| Mark behavior | Candidate backend |
|---|---|
| hard, closed, crisp outline | Slug-style curve bands or sparse strips |
| general stable vector path | Vello/direct vector |
| continuous soft density | Ciallo-style integral or Gaussian kernel |
| discrete texture | cumulative-length bounded stamps |
| wet/destination-dependent | sparse raster or local simulation island |
| live predicted tail | transient low-latency overlay |
| distant expensive content | derived raster mip |

The renderer may change with zoom, device capability, edit state, or measured
cost without changing the mark's identity. Transitions need visual-equivalence
tests and should avoid visible popping.

What may be distinctive is the combination:

- one deterministic, editable brush description;
- multiple replaceable render backends;
- screen-error and cost-based backend selection;
- ordered document semantics independent of caches;
- explicit simulation islands rather than a universal medium.

Every ingredient has prior art. Novelty or patentability is not claimed and
would require a separate prior-art and legal review.

## Things Not to Bet the Architecture On

- one SDF or ADF representation for all color, order, and media;
- “GPU everything” without CPU/GPU/power measurements;
- Gaussian splats for every crisp closed shape;
- thousands of dabs when an equivalent integral exists;
- whole-canvas fluid simulation;
- universal tile independence across filters and blend groups;
- renderer caches as canonical saved data;
- a benchmark from a discrete GPU as evidence for Intel integrated or mobile
  hardware.

## Experimental Roadmap

### Corpus

Define at least these marks before renderer work:

1. a hard opaque pressure-ink stroke with a sharp taper;
2. a translucent marker with self-overlap;
3. a textured pencil or chalk stroke;
4. a soft airbrush with dense accumulation;
5. a wet brush that changes destination state.

For each, specify zoom, overlap, eraser, transform, undo, and edit behavior.

### Experiments

| Experiment | Compared paths | Decides |
|---|---|---|
| Hard-edge corpus | Slug-style bands vs Vello Hybrid vs expanded mesh | analytic coverage backend |
| Soft density | 30+ dabs vs product integral vs Gaussian approximation | continuous brush backend |
| Live ink | stable prefix + predicted tail vs full redraw | active overlay design |
| Cache/edit | fresh render vs cached strips vs adaptive reduction | reusable cache boundary |
| Deep zoom | wide CPU coordinates + local GPU `f32` over an extent/zoom grid | coordinate model |
| Wet island | bounded simulation state vs baked sparse raster | stateful-media contract |
| Portability | same scenes on Atlas Intel GPU and at least one mobile-class GPU | CPU/GPU split and feature floor |

Record:

- p50/p95 CPU preparation and GPU time;
- input-to-visible latency;
- bytes uploaded per frame;
- peak resident and temporary memory;
- number of invalidated tiles/cells/nodes;
- rebuild time after cache eviction and simulated device loss;
- zoom-dependent visual error;
- correction magnitude for predicted input;
- power or sustained-performance behavior where measurable.

### Stop Conditions

A custom backend should stop or narrow in scope when:

- it does not beat Vello or sparse raster on its representative mark;
- its cache consumes more memory than replay saves;
- transform or layer semantics invalidate it too often;
- its visible approximation cannot be bounded;
- it requires nonportable GPU features for the baseline product;
- it makes save/undo depend on derived data.

## Research Questions Still Open

1. Can Slug-style cell-local bands remain efficient for long, self-overlapping
   pressure strokes at extreme zoom?
2. Which brush-graph subset can be proven equivalent across analytic, stamp,
   and raster backends?
3. Can the stable prefix of a modeled stroke be committed incrementally without
   a join artifact when the tail changes?
4. At what mark density does cached coverage beat rerasterizing sparse strips?
5. Can an ordered compositing reduction be sparse enough to justify its memory?
6. Which filters and blend groups must force an offscreen or baked boundary?
7. How should renderer switching be made visually continuous?
8. Which state of a wet simulation is canonical, replayable, or intentionally
   baked?
9. What is the minimum portable WebGPU feature set, and which optimizations are
   native-only?
10. Does any proposed combination create freedom-to-operate concerns even when
    the component implementation is permissively licensed?
