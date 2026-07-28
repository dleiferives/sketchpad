# Continuous Brush Contact and Physical Paint Research

Status: research and experiment design, 2026-07-28.

## Scope

This note is about producing fast, expressive marks from a moving physical
tool. It is specifically **not a watercolor plan**. It does not propose
diffusion through paper, capillary flow, drying fronts, or a canvas-wide wet
simulation.

The likely hyper-realistic reference is Adobe and Ohio State's
[Project WetBrush](https://wanghmin.github.io/publication/chen-2015-wgb/), a
GPU oil/impasto system that simulates brush, paint, and canvas interactions at
the bristle level. WetBrush is useful evidence and inspiration; its full
particle/fluid simulator is not an appropriate first implementation for
Sketchpad.

The immediate problem is narrower:

> Given consecutive pen poses, generate one continuous, stable contact mark
> whose cost follows visible coverage rather than the number of overlapping
> stamps.

This should make a flat nib, palette knife, pencil, and bristle brush both
faster and more natural before any destination-color pickup, paint mixing, or
fluid state is introduced.

## Decision Summary

The next brush experiment should:

1. keep the current sparse `f32` raster document;
2. separate input dynamics, contact geometry, material transfer, and
   compositing;
3. replace dense palette-knife stamps with a GPU-rendered swept blade/ribbon;
4. use the same transient contact commands for a CPU reference backend;
5. render fixed-color source-over first, without reading destination pixels;
6. compare appearance rather than require the prototype's exact checksum;
7. add height, brush load, pickup, scraping, or impasto only as separately
   measured material features.

This is a selected experiment, not yet a production renderer decision.

## What the Research Actually Shows

### WetBrush: the high-fidelity end of the spectrum

[WetBrush](https://www.zhilichen.com/research/wet_brush/2015-WB.pdf) is a
real-time 3D oil-paint system implemented in CUDA. Its central performance
choice is a hybrid paint representation:

- paint near the brush is represented by particles so sub-pixel bristle and
  liquid interactions survive;
- paint away from the brush is represented by a density field;
- brush and particles are simulated in non-inertial frames;
- the implementation uses a fixed-point acceleration for Jacobi iterations
  and an Eulerian-Lagrangian liquid method.

This is strong evidence for two architectural principles:

- the detailed representation only needs to exist where brush contact demands
  it;
- brush dynamics, contact transfer, stored canvas state, and final rendering
  can use different representations.

It is not evidence that a low-latency drawing app should begin with millions
of paint particles. WetBrush targeted a high-end GPU, used CUDA rather than a
portable graphics API, and solves a much more expensive problem than the
current flat-color brush.

### The mobile oil-paint result: convincing approximation wins

[Real-Time Oil Painting on Mobile Hardware](https://diglib.eg.org/items/d37a6d8c-1ea9-47f0-a39c-49bddbd67e5f)
deliberately replaces full 3D paint with a 2.5D height field. It stores fluid
height, velocity, and vertically stacked pigment layers in 2D textures and
uses an approximate shallow-water model for oil-like behavior.

The paper reports:

- 45–46 frames per second on an iPad Air 2;
- a `1024 × 768` simulation feeding a `2048 × 1536` render;
- two pigment layers and four Jacobi iterations;
- about 19 MB of dynamic state;
- 16-bit half-float state and OpenGL ES 3 fragment programs;
- a typical cost split of 30% height computation, 15% velocity, 10%
  advection, 18% rendering, and **27% brush stamping**.

That implementation is valuable evidence, but it is not our blueprint:

- Sketchpad's working pixels are `f32`; silently substituting half precision is
  not acceptable.
- Its brush model is explicitly a simple stamp texture. The authors identify
  simulated brush behavior as a major missing feature.
- Even inside an aggressively simplified mobile simulation, stamping consumed
  more than a quarter of the measured frame.

The transferable lesson is not “copy these equations.” It is “choose the
cheapest representation that preserves the behavior artists perceive.”
Their artist study rated multi-layer paint state and approximate dynamics
highly despite knowingly crude physics.

### Industrial bristles: delete physics artists cannot see

[Industrial-Strength Painting with a Virtual Bristle Brush](https://research.adobe.com/publication/industrial-strength-painting-with-a-virtual-bristle-brush/)
models bristles as independent strand dynamics in a non-inertial brush frame
and performs bidirectional paint transfer. The paper's production work
contains particularly relevant simplifications:

- bristle-bristle collision was removed after visual and artist evaluation
  showed little useful change in the resulting marks;
- removing it yielded roughly an order-of-magnitude speedup;
- fewer simulated bristles can be made thicker to preserve aggregate coverage;
- brush dynamics can run at a lower rate while intermediate contact geometry
  is interpolated;
- the contact geometry is rasterized as projected quad strips;
- brush physics, rasterization, and compositing can be pipelined.

This is close to the Casey Muratori-style lesson we need: model the visible
result, remove work that does not change it, and expose fidelity as a bounded
quality control instead of accidentally tying it to footprint size.

### Sweeping versus stamping

[A Brush Stroke Synthesis Toolbox](https://research.adobe.com/publication/a-brush-stroke-synthesis-toolbox/)
separates two questions:

1. how instantaneous brush shapes are produced—simulation, captured examples,
   or procedural dynamics;
2. how those discrete shapes become a continuous mark—stamping or sweeping.

This means we can improve continuity and raster cost without first solving
perfect brush physics.

[Efficient Rendering of Linear Brush Strokes](https://jcgt.org/published/0007/01/01/)
provides a useful complexity result for a restricted round/soft brush.
Repeated diameter-sized stamps require work proportional to `O(N M²)` for
stroke length `N` and diameter `M`; continuous evaluation of the swept stroke
reduces this to `O(N M)`, avoids inter-stamp overdraw, and can be rendered in a
single GPU draw.

[Ciallo](https://doi.org/10.1145/3641519.3657418) demonstrates several
practical GPU brush families:

- capsules/trapezoids for continuous basic strokes;
- arc-length-aware placement for brushes that genuinely require texture
  stamps;
- bounded candidate lookup per fragment;
- a continuous alpha-density integral for dense airbrushes.

The important conclusion is not that stamps are always wrong. It is that
different mark families deserve different kernels. A texture brush may still
need distance-spaced stamps; a palette knife does not.

### Data-driven physical appearance is another valid route

[RealBrush](https://research.adobe.com/publication/realbrush-painting-with-examples-of-physical-media/)
captures isolated, overlapping, and smudged examples of real media and
synthesizes new marks from them. This can reproduce physical appearance
without solving physical equations.

It is a later candidate for paper texture, dry-brush breakup, or complex
material appearance. It is not the first live path because neighborhood
synthesis, example storage, and overlap semantics create a separate
performance and document problem.

## The Separation We Need

The current brush prototype folds too many concepts into one pixel loop. The
replacement should have explicit stages:

```text
timestamped pen events
    │
    ▼
resampling and brush dynamics
    │   position, pressure, tilt, time
    ▼
contact poses
    │   center, orientation, width, depth, load
    ▼
continuous contact commands
    │   ribbon, strand ribbon, textured sweep, or bounded dab
    ▼
material transfer
    │   initially fixed-color coverage only
    ▼
active-layer raster/composite
```

Each boundary should remain independent:

- Input resampling determines temporal and geometric stability.
- Contact dynamics decide how the tool bends, spreads, rotates, and runs out.
- Contact geometry decides which pixels are influenced.
- Material transfer decides what coverage deposits or removes.
- Compositing decides how that result changes a layer.

A rectangular contact should not implicitly enable color pickup. A future
impasto brush should not require every pencil stroke to own fluid state.

## Proposed Transient Contact Model

The durable document remains one logical gesture with timestamped samples and
a versioned brush recipe. The renderer can derive compact, disposable poses:

```text
ContactPose
    center
    direction
    half_width
    half_depth
    pressure
    timestamp
```

The initial command families are:

```text
BladeSweep
    previous left/right blade endpoints
    current left/right blade endpoints
    thickness and coverage profile

StrandSweep
    previous/current strand contact point
    radius, load, strength, seed

TexturedSweep
    centerline interval
    transverse profile
    canvas-anchored texture transform

BoundedDab
    pose and tip resource
```

These are renderer commands, not new document operations and not a public
brush-file format.

### Palette knife and flat nib

Treat the contacting blade as an oriented line segment with small thickness.
Two consecutive blade poses define a ruled quadrilateral between their
left/right endpoints. Render the connected quadrilaterals as one batch, with
caps only at true stroke boundaries.

The pose path must subdivide when translation, width change, or angular change
exceeds a visual error bound. This is not stamp spacing: it refines a connected
surface so rotation and curvature remain faithful. The tolerance should be
screen-aware but deterministic for a declared render scale.

Potential failure cases requiring fixtures:

- a blade rotates around a nearly stationary center;
- endpoint correspondence creates a self-crossing quad;
- a very sharp corner leaves a pinhole or creates excessive overlap;
- pressure collapses one dimension;
- tilt crosses its upright dead zone;
- the input packet rate changes while the geometric path does not.

Large angular changes can initially be handled by bounded subdivision. A later
implementation can generate a tighter analytic swept envelope if measurements
justify the complexity.

### Bristle brush

Represent a bounded number of independent bristle groups rather than one lane
decision for every destination pixel. Each group owns a lateral rest position,
current contact point, stiffness/damping state, radius, transfer strength, and
paint load. Sweep its contact point into a narrow connected ribbon or capsule.

The first model should intentionally omit:

- bristle-bristle collisions;
- a 3D fluid solver;
- per-pixel destination pickup;
- a strand count derived from brush diameter.

Coverage can remain stable across quality levels by widening or grouping
strands as count decreases. A smaller count should alter fine breakup without
making the whole brush transparent.

### Pencil and chalk

Use a connected swept footprint for geometric contact, then modulate coverage
with a deterministic canvas-anchored paper field. The grain must be fixed in
document space so it does not swim with event rate, viewport motion, or
replay.

A side-tilted pencil can use a wider transverse profile and lower-frequency
contact modulation. It does not require fluid or paint-height state.

### Brushes that still deserve stamps

A stamp remains appropriate when the stamp image itself is the authored
content: foliage, particles, discrete texture shapes, or a deliberately
repeating pattern. Those stamps should be placed by path distance with a
versioned seed, not once per input event.

The engine should not disguise these as continuous ribbons, and it should not
force continuous brushes through this path.

## First GPU Experiment

The first experiment is a fixed-color `f32` palette-knife sweep rendered
offscreen on Apollo.

### Rejected CPU incremental prototypes — 2026-07-28

Three continuous CPU raster variants were implemented and measured against the
same `2048 × 2048`, 256-sample, `512 px` palette-knife workload on Apollo.
They were removed rather than allowed to replace the live brush:

| Variant | Empty median | Painted median | Result |
|---|---:|---:|---|
| Existing oriented-dab control | 263.544 ms | 260.293 ms | baseline |
| Full old/new hull per load lane | 608.612 ms | 589.448 ms | rejected |
| Boundary-only sweep per load lane | 561.706 ms | 574.492 ms | rejected |
| One boundary sweep with transverse profile | 1073.741 ms | 1049.711 ms | rejected |

The full-hull version still rasterized most of a large contact at every input
update. Boundary-only geometry reduced geometric overlap but twelve separately
scanned load bands retained high scan-conversion cost. Replacing those bands
with one transverse-profile lookup increased conservative tile damage from
18.9 to 33.676 million pixels on the rotating trace and failed the collinear
packet-batching fixture when the blade moved along its own major axis.

The broader finding is that “continuous geometry” alone is insufficient inside
the current synchronous CPU mutation path. Every input update still performs
some combination of tile lookup, undo capture, conservative damage,
scan conversion, active-layer mutation, layer recomposition, and CPU-to-GPU
upload scheduling. A rotating `512 px` blade produces large conservative
axis-aligned regions even when only its boundary is new.

Consequences:

- Keep the pure `BladePose`/convex-envelope geometry and its deterministic
  tests as backend-neutral groundwork.
- Do not integrate either rejected CPU rasterizer.
- Preserve the old knife as the live control until another path passes both
  performance and packet-invariance gates.
- Advance the offscreen full-float GPU proof next.
- A future live GPU path must batch contact work for one presentation
  opportunity and avoid readback; it must not write directly into the
  flattened display cache.
- If GPU live presentation proves necessary, define correct active-layer
  ownership or below/active/above composition before integration.

### Capability probe

Record, rather than assume:

- whether `Rgba32Float` supports render attachment and blending;
- whether `Rgba32Float` supports storage binding;
- timestamp-query support and timestamp period;
- relevant texture limits and workgroup limits;
- the exact adapter, driver, backend, and power state.

If full-float fixed-function blending is unavailable, compare:

1. a portable `f32` storage-buffer compute path;
2. a CPU span implementation from the same contact commands;
3. only then, an explicit alternative representation.

There must be no silent `f16` fallback.

#### Apollo result — 2026-07-28

`gpu_brush_capabilities` now reports every primary adapter rather than
silently selecting the first device. On Apollo's constrained target, it
identified:

- Intel UHD Graphics (JSL), integrated GPU, Vulkan;
- Intel open-source Mesa driver 25.0.7-2;
- `Rgba32Float` support for render attachment, texture binding, storage
  binding, copy source, and copy destination;
- `Rgba32Float` format flags for fixed-function blending, filtering,
  read/write storage, multisampling, and resolve;
- the optional `FLOAT32_BLENDABLE` device feature;
- timestamp queries both at render-pass boundaries and inside command
  encoders, with a reported 52.083332 ns timestamp period;
- a 16,384-pixel maximum 2D texture dimension, 2,147,483,644-byte maximum
  storage-buffer binding, and 65,536-byte compute workgroup storage limit.

The software `llvmpipe` adapter is reported separately and must not be confused
with the integrated-GPU measurement. Run the repeatable report with:

```sh
scripts/apollo run cargo run --release --bin gpu_brush_capabilities
```

This removes a major uncertainty from the first proof: Apollo can render and
source-over composite directly into a full-precision `Rgba32Float` target.
It does not establish that the path is fast, that multisampling is the right
edge strategy, or that a live GPU-owned active layer has correct document and
undo semantics. Those remain measurement and architecture gates.

### Offscreen GPU result — 2026-07-28

`gpu_brush_bench` consumes the deterministic `BladePose`/`BladeSweep` model
and triangulates the same `2048 × 2048`, 256-sample, rotating `512 px` trace
used by the CPU control. It uploads the mesh once and performs one source-over
draw into an `Rgba32Float` target. Target reset, brush rendering, CPU
encode/submit, and readback are separately measured. Thirty measured runs
after three warm-ups on Apollo produced:

| State | Old CPU control median | GPU brush median | GPU brush p95 | Median speedup |
|---|---:|---:|---:|---:|
| Empty | 263.544 ms | 8.366 ms | 9.034 ms | 31.5× |
| Painted | 260.293 ms | 8.353 ms | 8.457 ms | 31.2× |

The GPU geometry batch contained 256 sweeps, 1,018 triangles, 3,054 vertices,
and 24,432 bytes of vertex data. CPU construction took 108.723 µs. The sum of
the clipped polygon bounding-box areas was 33,892,792 pixels; this is a
conservative work-amplification indicator, not a hardware fragment count.

The median CPU encode/submit costs were 267.589 µs empty and 260.790 µs
painted. Full-target resets were timestamped separately at 10.208 µs and
10.104 µs median. A single correctness readback after all timed runs took
66.403 ms and 66.680 ms, respectively, and is excluded from brush timing.
Both states changed the same 1,211,528 pixels, and their checksums repeated
exactly across the 12-run and 30-run captures.

Run the experiment with:

```sh
scripts/apollo run cargo run --release --bin gpu_brush_bench -- \
  --adapter Intel --runs 30 --warmups 3
```

This passes the first throughput gate: even Apollo's constrained integrated
GPU is more than ten times faster than the current CPU control without
reducing storage precision. It does **not** yet pass the product gate:

- the benchmark uses one opaque, hard-edged color so internal triangulation
  overlap cannot accumulate opacity;
- it measures a fully batched stroke rather than display-paced incremental
  command batches;
- the target is disposable and has no layer, undo, save, recovery, or cancel
  ownership contract;
- it does not prove a final antialiasing, transverse load, texture, or dry-mark
  model;
- it excludes the intentionally isolated 64 MiB readback, as the live stroke
  path must never perform it.

The next proof should retain this full-float target, split the trace into
display-paced incremental batches, and measure new geometry only. Live
integration must then give the active layer an explicit GPU/CPU ownership and
undo contract; writing into the flattened visible composite remains invalid.

### GPU path

For the initial render path:

- build or upload one batch of connected blade segments;
- draw only their conservative bounds;
- evaluate edge coverage in the fragment shader;
- use source-over fixed-function blending when supported;
- retain the active layer on the GPU for the timed loop;
- do not read pixels back during a stroke;
- copy or read back only after the timed loop for correctness images.

The first prototype may generate vertices on the CPU. On an integrated GPU,
eliminating millions of CPU pixel operations is more important than proving
that every small geometry calculation belongs in a compute shader.

### CPU reference

The CPU backend should rasterize coverage spans from the same blade sweeps:

- determine conservative rows;
- intersect each row with the swept polygon;
- process contiguous spans;
- apply the same declared coverage and source-over semantics;
- avoid per-pixel inverse transforms and lane selection.

This gives a portable control, a correctness oracle for simple cases, and a
useful fallback without preserving the old dab kernel.

## Performance and Quality Gates

The old CPU prototype is a control, not a golden image. Compare:

### Workloads

- diameters `48`, `128`, and `512`;
- straight, diagonal, curved, rotating, and pressure-ramped strokes;
- empty and heavily painted target tiles;
- sparse and dense input event streams describing the same path;
- live-equivalent `1×`, `2×`, and `4×` playback;
- repeated randomized order after one warm-up;
- long strokes crossing many sparse tiles;
- tiny slow details with frequent direction changes.

### Measurements

- input-to-submit and input-to-present distributions;
- CPU command-generation and submit time;
- GPU timestamp for the brush pass;
- generated segments and vertices;
- conservative pixels/fragments versus changed pixels;
- destination reads and memory bytes where measurable;
- temporary allocation high-water mark;
- active dirty tiles and recomposited tiles;
- readback/copy time reported separately;
- sustained behavior and thermal/power state.

### Visual gates

- no scalloped stamp silhouette;
- no gaps or seams between contact poses;
- comparable marks at different packet rates and stroke speeds;
- stable width, opacity, and grain on diagonals;
- intentional self-overlap behavior;
- smooth pressure and orientation transitions;
- a useful dry-brush/strand breakup rather than a synthetic comb;
- correct physical tilt orientation after a labeled Wacom calibration.

Exact checksums still apply to document state transitions, undo, persistence,
and repeat runs of one backend. Different brush algorithms are compared with
reference images, difference images, continuity metrics, and artist judgment.

## Material Features After the Continuous Path

The continuous fixed-color path should ship before any of these. They are
separate experiments:

### Brush load without color pickup

Track a scalar load per blade region or strand. Coverage decreases or breaks
up as load drains. This adds expressive depletion without reading the
destination or mixing colors.

### Local paint height and impasto

Store optional height separately from RGBA on layers that use it. A brush can
deposit height, and lighting can derive normals from the local height field.
A layer without impasto state should pay no simulation or memory cost.

The first useful result may be kinematic rather than fluid:

- deposit or redistribute height only under the swept contact;
- conserve or explicitly account for displaced volume;
- form grooves from bristle paths;
- let a knife spread or scrape local height;
- avoid a whole-canvas time-stepped solver.

This borrows WetBrush's representation separation while refusing its full
simulation cost.

### Resolution-matched local pickup

[Detail-Preserving Paint Modeling for 3D Brushes](https://research.google/pubs/detail-preserving-paint-modeling-for-3d-brushes/)
uses a canvas snapshot and a resolution-matched pickup map beneath the brush.
That avoids repeatedly mapping and oversampling canvas detail onto a 3D brush.

If pickup returns later, a small brush-local transfer map is a stronger first
candidate than arbitrary destination-color reads inside every current brush
pixel. It still needs explicit transparency, layer, undo, and persistence
semantics.

### Full fluid or particle simulation

A WetBrush-class hybrid particle/grid simulation is a long-term research
backend for oil/impasto, not a requirement for excellent core brushes. It
should only begin after:

- continuous contact is fast and visually validated;
- optional height state has a clear document contract;
- constrained-device budgets are known;
- local kinematic transfer has been shown insufficient;
- its artistic value is judged against the latency and memory cost.

## What Not to Do

- Do not build one universal per-pixel brush kernel.
- Do not make dab spacing an accidental function of input packet rate.
- Do not preserve the current lane kernel's pixels as the brush definition.
- Do not introduce destination pickup merely because a tool is called a
  “palette knife” or “bristle brush.”
- Do not use half precision without an explicit, quality-validated product
  decision.
- Do not run a fluid solver over the whole canvas to improve a dry brush.
- Do not retain every transient ribbon segment as a separate document
  operation.
- Do not read the active layer back to the CPU inside the live input path.
- Do not optimize only average frame time; long input-to-present spikes are
  product failures.

## Ordered Research and Delivery Plan

1. [Complete] Add an Apollo capability report for full-float render/storage
   paths and GPU timestamps.
2. [Complete] Define the internal contact-pose and blade-sweep command contract
   with deterministic fixtures.
3. [Complete] Add an offscreen GPU benchmark for a `512 px` continuous knife
   stroke. Apollo is approximately 31× faster than the old CPU control for the
   first full-float opaque geometry proof.
4. [Rejected] Add a CPU span implementation consuming the same commands; the
   measured prototypes were slower than the existing dab control, so retain
   the evidence rather than their product code.
5. Compare GPU, CPU span, and old dab prototype on performance and visual
   continuity.
6. Integrate the winning path as an active fixed-color palette knife without
   live readback.
7. Validate Wacom orientation with a labeled calibration view.
8. Generalize the path to a flat nib and canvas-anchored pencil profile.
9. Add bounded independent strand ribbons and measure bristle quality levels.
10. Only after these marks are fast, evaluate scalar load depletion and a
    local `f32` paint-height experiment.

## Primary References

- Chen, Kim, Ito, and Wang,
  [WetBrush: GPU-Based 3D Painting Simulation at the Bristle Level](https://wanghmin.github.io/publication/chen-2015-wgb/)
- Stuyck, Da, Hadap, and Dutré,
  [Real-Time Oil Painting on Mobile Hardware](https://diglib.eg.org/items/d37a6d8c-1ea9-47f0-a39c-49bddbd67e5f)
- DiVerdi, Krishnaswamy, and Hadap,
  [Industrial-Strength Painting with a Virtual Bristle Brush](https://research.adobe.com/publication/industrial-strength-painting-with-a-virtual-bristle-brush/)
- DiVerdi,
  [A Brush Stroke Synthesis Toolbox](https://research.adobe.com/publication/a-brush-stroke-synthesis-toolbox/)
- Hsu, Lee, and Wiseman,
  [Efficient Rendering of Linear Brush Strokes](https://jcgt.org/published/0007/01/01/)
- Shen et al.,
  [Ciallo: GPU-Accelerated Rendering of Vector Brush Strokes](https://doi.org/10.1145/3641519.3657418)
- Lu et al.,
  [RealBrush: Painting with Examples of Physical Media](https://research.adobe.com/publication/realbrush-painting-with-examples-of-physical-media/)
- Chu et al.,
  [Detail-Preserving Paint Modeling for 3D Brushes](https://research.google/pubs/detail-preserving-paint-modeling-for-3d-brushes/)
