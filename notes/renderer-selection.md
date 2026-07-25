# Renderer Selection: Vello, Raw `wgpu`, and Target Specialization

Status: working renderer decision, 2026-07-24. This note answers whether
Sketchpad should be built around Vello, another general renderer, or a
purpose-built `wgpu` architecture. It is a recommendation to guide experiments,
not a claim that unmeasured performance is already known.

The first product is narrower: custom bounded `wgpu` tile painting and
compositing for sparse raster layers. Vello integration is not required before
that painter is usable. See
[first-usable-product.md](first-usable-product.md).

## Short Answer

**Sketchpad should own a narrow rendering architecture built on `wgpu`. Vello
should not be the center of the canvas or the form of the document.**

Vello remains useful in three smaller roles:

1. a replaceable renderer for conventional vector content;
2. a correctness and performance baseline for custom hard-edge work;
3. a source of algorithms, tests, path/color vocabulary, and possibly CPU
   fallback components.

The part worth building directly is the part specific to Sketchpad:

- live predicted ink;
- brush evaluation and deposition;
- analytic hard-edge, continuous-density, stamp, and simulation kernels;
- damage tracking and sparse canvas caches;
- layer compositing and cache scheduling;
- view-dependent backend selection;
- GPU resource, pass, and presentation policy.

The part **not** worth rebuilding without a demonstrated product need is a
complete general-purpose graphics library:

- every SVG/PostScript path corner case;
- a complete text shaping and glyph-rendering stack;
- every clip, mask, gradient, image filter, and blend mode;
- PDF/SVG import fidelity;
- a Skia-sized CPU rasterizer;
- separate Metal, Vulkan, and Direct3D runtimes from the beginning.

This is not “Vello versus `wgpu`” in the literal sense. Vello itself uses
`wgpu`. The real choice is whether Sketchpad owns its renderer policy and
specialized algorithms, or expresses the canvas through Vello's general scene
and accepts Vello's algorithms, lifecycle, capabilities, and performance
cliffs.

## Working Decision

The recommended dependency direction is:

```text
canonical document and brush graph
                │
                ▼
scene extraction · culling · damage · active-stroke state
                │
                ▼
Sketchpad render plan
     ├── custom hard-edge backend
     ├── custom density/stamp backend
     ├── custom raster/simulation islands
     ├── conventional-vector adapter ──► Vello Hybrid or equivalent
     └── CPU/reference adapter ────────► Vello CPU or equivalent
                │
                ▼
shared wgpu resources · passes · compositor · surface
```

The `Sketchpad render plan` should be narrower than a generic Canvas 2D API. It
should describe the operations and cache dependencies that Sketchpad actually
needs. A renderer adapter may consume that plan, but no third-party renderer
scene becomes canonical application state.

This preserves three freedoms:

- replace Vello if its API, performance, or maintenance changes;
- specialize the hot workloads without forking the whole renderer;
- retain a portable path while adding device-specific implementations.

## Why Raw `wgpu` Is Justified Here

### The workload is unusually specific

General 2D renderers optimize an imaging model: paths, fills, strokes, images,
gradients, clips, layers, filters, and glyphs. Sketchpad's intended workload is
not merely “draw many SVG paths.” Its difficult operations are temporal and
medium-specific:

- an active stroke changes every input sample;
- predicted samples must be replaced without corrupting committed ink;
- repeated deposition and self-overlap are part of brush semantics;
- soft density can require integration rather than polygon coverage;
- textured brushes need bounded stamp lookup and stable spacing;
- wet media may update local state over time;
- only a damaged viewport region should be recomputed;
- different zoom levels can justify different derived representations.

Those operations would sit beside or outside Vello even if Vello drew every
finalized hard path. Once custom passes, storage, damage, scheduling, and
compositing already belong to Sketchpad, making Vello the architectural center
buys less than it first appears.

### `wgpu` does not force one identical shader on every GPU

`wgpu` supplies Vulkan, Metal, Direct3D 12, and other backends behind a common
safe API. It exposes adapter identity, device type, backend, limits, texture
format capabilities, subgroup ranges, and whether transient attachments save
memory. Optional features are requested and checked at runtime.

That is enough to support a common renderer with multiple measured variants:

- compute versus render-pass implementation;
- tile and workgroup size;
- subgroup-assisted versus portable scan/reduction;
- texture and accumulation format;
- CPU versus GPU geometry preparation;
- storage buffer layout;
- upload and staging strategy;
- transient attachment and pass-merging policy;
- cache size, eviction, and LOD thresholds.

These are “raw `wgpu`” specializations and do not require three independent
native renderers.

The official [`wgpu::AdapterInfo`
documentation](https://docs.rs/wgpu/latest/wgpu/struct.AdapterInfo.html)
exposes the data needed to form such capability profiles. The
[`Features`](https://docs.rs/wgpu/latest/wgpu/struct.Features.html) and
[`Limits`](https://docs.rs/wgpu/latest/wgpu/struct.Limits.html) APIs allow
feature-gated variants rather than compile-time guesses.

### The abstraction is unlikely to be the first performance ceiling

Vello previously maintained its own cross-platform GPU abstraction. Its author
described replacing that layer with `wgpu` because the custom layer carried
high maintenance and interoperability cost while `wgpu` had become capable
enough. The article also identifies genuine common-denominator and shader
startup tradeoffs; it does not claim abstraction is free. See
[Requiem for piet-gpu-hal](https://raphlinus.github.io/rust/gpu/2023/01/07/requiem-piet-gpu-hal.html).

For Sketchpad, likely first-order performance factors are:

- how much of the canvas is touched;
- how much intermediate data is generated and transferred;
- whether passes preserve tile/local memory;
- whether geometry and coverage work are repeated;
- overdraw and blend bandwidth;
- cache hit rate and invalidation granularity;
- synchronization and pipeline creation;
- the selected brush algorithm.

Those can all change by multiples. The overhead or restrictions of `wgpu`
should only outrank them after a profile proves a missing API or unavoidable
translation cost.

## What Vello Actually Buys

Vello is valuable engineering, not a magical speed layer. Classic Vello already
provides:

- a PostScript-like scene API;
- arbitrary path fills and strokes;
- gradients, images, clips, layers, and glyph outlines;
- compact scene encoding;
- GPU path processing and rasterization;
- integration with a caller-owned `wgpu` device and output texture.

That is a large amount of conventional vector functionality. If Sketchpad were
primarily a diagram, SVG, PDF, or UI application, adopting it as the main
renderer would be much more compelling.

However, the [Vello repository](https://github.com/linebender/vello) still
labels the classic compute renderer alpha and lists open work around filters,
artifacts, GPU allocation, and glyph caching. Its public performance number is
explicitly described as a best case, with formal benchmarks still pending.
That number cannot answer performance on Sketchpad's brushes, Atlas's Intel
integrated GPU, or a thermally constrained phone.

### “Vello” currently names three different choices

| Choice | Architecture | Best role in Sketchpad | Main concern |
|---|---|---|---|
| Vello Classic | compute-centric general vector renderer | benchmark and algorithm reference | alpha; compute requirement; memory/performance cliffs on weaker GPUs must be measured |
| Vello Hybrid | CPU SIMD path setup plus GPU strip rendering/compositing | optional conventional-vector backend | promising mobile/integrated shape, but API and internals are still moving |
| Vello CPU | SIMD and multithreaded software renderer | CPU fallback, export, golden images, benchmark | pixel upload and CPU energy; not a substitute for live GPU brush kernels |

Vello Hybrid is the most relevant of the three. Its
[README](https://raw.githubusercontent.com/linebender/vello/c33438626fbd307bdc7b158566c028d16355b1d7/sparse_strips/vello_hybrid/README.md)
describes CPU path processing with GPU rendering and compositing, with minimal
transfer between them. Linebender called it “roughly beta quality” in its
[2026 Q1 update](https://linebender.org/blog/tmil-25/), while the sparse-strips
[development README](https://raw.githubusercontent.com/linebender/vello/c33438626fbd307bdc7b158566c028d16355b1d7/sparse_strips/README.md)
still says the implementation is under active development and not yet suitable
for production use. Both facts matter: it is testable and interesting, but not
a boundary to hard-code into the document or brush system.

Vello CPU is explicitly intended for devices with no or underpowered GPU. Its
[README](https://raw.githubusercontent.com/linebender/vello/c33438626fbd307bdc7b158566c028d16355b1d7/sparse_strips/vello_cpu/README.md)
describes a fixed-area software render context. It is a useful fallback and
reference. It does not make CPU rendering automatically preferable on mobile:
CPU work, framebuffer writes, and subsequent display upload still need
measurement for time, bandwidth, and energy.

### Current integration mismatch

At the researched commit, Vello 0.9.0 and the experimental Hybrid/CPU 0.0.9
workspace use `wgpu` 29.0.3, while Sketchpad already uses `wgpu` 30. Two
different major `wgpu` crates do not share `Device`, `Queue`, or `Texture`
types. A real Vello experiment must therefore align versions in an isolated
branch or wait for a compatible release; it must not silently initialize a
second unrelated GPU stack.

This is a temporary versioning issue, not a fundamental rejection of Vello. It
is another reason to put Vello behind an adapter.

## Candidate Comparison

The following is a workload-fit judgment, not a benchmark result.

| Candidate | General paths | Custom brushes | GPU/mobile control | CPU fallback | Maturity/integration judgment |
|---|---:|---:|---:|---:|---|
| custom `wgpu` core | only what we build | excellent | excellent | must be separate | best product fit; scope must be controlled |
| Vello Hybrid | strong and growing | external passes still needed | good through `wgpu`; CPU/GPU split chosen by Vello | related Vello CPU project | most useful optional vector component; moving |
| Vello Classic | strong | external passes still needed | compute-centric | no | useful benchmark; weak choice as sole production floor today |
| Vello CPU | strong and growing | CPU implementations required | no GPU kernels | native role | useful fallback/reference |
| Skia / `rust-skia` | broadest conventional imaging | custom GPU integration is possible but foreign | mature native backends, less `wgpu` ownership | excellent | safest compatibility engine; heavy C++ and context/build integration |
| Lyon + custom `wgpu` | fill/stroke tessellation | good for mesh-like marks | high | geometry stage is CPU | simple baseline, not a complete quality renderer |
| Blend2D | broad CPU vector | CPU-only extensions | no GPU | excellent | strong software benchmark; C++/JIT integration |
| `tiny-skia` | useful subset | CPU-only extensions | no GPU | simple | small correctness fallback, not the fast primary |

### Skia

[Skia](https://skia.org/docs/) is the conservative answer when breadth,
production history, text, filters, color management, and mobile deployment are
more important than owning the renderer. [`rust-skia`](https://github.com/rust-skia/rust-skia)
offers CPU and native GPU bindings and prebuilt binaries for common targets.

It is not the leading recommendation because:

- it introduces a large C++ build and binary dependency;
- its GPU contexts and resources are not naturally the same as `wgpu` values;
- specialized brush passes still need custom integration;
- its general imaging architecture is not Sketchpad's canonical model;
- deep internal specialization is harder than owning a focused Rust/WGSL core.

Skia should remain a golden-image and competitive baseline. It becomes the
primary recommendation if the product shifts toward broad conventional 2D
compatibility rather than novel brush behavior.

### Lyon

Lyon turns paths into triangles that are straightforward to render with
`wgpu`. It is useful as a boring, inspectable hard-shape baseline and possibly
for geometry that caches well. It is not a universal final answer. Its
[`StrokeTessellator`
documentation](https://docs.rs/lyon_tessellation/latest/lyon_tessellation/struct.StrokeTessellator.html)
notes incorrect rendering for some self-intersecting translucent strokes, and
flattening/tessellation quality and cost vary with transformation.

### Blend2D and `tiny-skia`

[Blend2D](https://blend2d.com/about.html) is a high-performance C++ software
renderer with an analytic rasterizer, curve-offset stroking, JIT-generated
pipelines, SIMD, and multithreading. It is a serious CPU baseline, especially
on integrated-GPU systems where “GPU” does not automatically mean faster. It is
not a GPU brush architecture, and displaying its pixels introduces an upload
or shared-surface problem.

[`tiny-skia`](https://github.com/linebender/tiny-skia) is appealing as a small,
pure-Rust fallback and test oracle for a subset of 2D operations. Its own
documentation reports substantial performance gaps from Skia, especially on
ARM, and it deliberately omits many systems. It should not drive the canvas
architecture.

## Portable First, Specialized Deliberately

The correct portability goal is **shared semantics and a shared correctness
path**, not identical machine code or identical scheduling on every device.

### Tier 0: portable correctness path

Every supported adapter needs a path that uses conservative `wgpu` features and
produces the specified result. It may be slower than an optimized variant. It
is the reference for:

- correctness comparisons;
- uncommon GPUs;
- new backends and driver workarounds;
- recovery when an optional feature is unavailable;
- keeping optional web support from becoming expensive.

Web does not determine the architecture. Avoiding unnecessary nonportable
assumptions is still valuable because it also improves support for old,
integrated, and mobile GPUs.

### Tier 1: runtime capability profiles

Choose algorithms from reported capabilities and measured device behavior, not
only operating-system names. A profile should include at least:

- backend, device type, vendor, device, and driver;
- limits and downlevel capabilities;
- subgroup range and optional features;
- texture format features;
- transient attachment behavior;
- timestamp/profiling availability;
- known driver workarounds;
- benchmark-calibrated tile, batch, and cache parameters.

Profiles should be broad initially:

- conservative integrated/mobile;
- tile-based mobile;
- desktop integrated;
- desktop discrete;
- software/CPU fallback.

Per-device tables are justified only for demonstrated driver or performance
outliers. Otherwise they become an unmaintainable folklore database.

### Tier 2: shader and pass variants

Good early specializations remain inside `wgpu`:

- `8×8`, `16×8`, or `16×16` workgroups selected by kernel and adapter;
- subgroup versus portable prefix/reduction operations;
- fragment/render-pass coverage versus compute coverage;
- compact versus wide primitive records;
- `f16`, normalized integer, or `f32` intermediates where quality permits;
- tile-local accumulation versus persistent storage textures;
- fused versus split brush/composite passes;
- different overdraw and sorting strategies;
- CPU-SIMD versus GPU geometry preprocessing;
- different active-stroke and finalized-stroke representations.

These variants should implement the same semantic operation and pass the same
image/error tests. A shader variant is not allowed to invent a different brush.

### Tier 3: native API escape hatch

A Metal-, Vulkan-, or Direct3D-specific path is justified only when all of the
following are true:

1. a representative workload misses a product target;
2. profiling identifies a specific unavailable operation or abstraction cost;
3. the native implementation demonstrates a material improvement;
4. the improvement cannot be obtained with a different `wgpu` algorithm;
5. the semantic and resource boundary lets the native path remain isolated;
6. the team accepts testing, synchronization, device-loss, and driver burden.

A useful proposed adoption policy is:

- do not add a native path for a single-digit percentage;
- seriously consider it for a repeated, product-visible improvement around
  1.5–2×, a large latency-tail reduction, or a capability unavailable through
  `wgpu`;
- retain the portable implementation as a fallback and correctness oracle.

The numbers are policy proposals, not laws. A 10% improvement could matter in a
hard 120 Hz budget; a 2× microbenchmark win can be irrelevant if the pass is
only 2% of the frame.

## Target Implications

### Atlas Intel UHD 630

This is the useful local floor. It shares memory bandwidth with the CPU and is
more representative of constrained hardware than Atlas's GTX 1650. It should
expose:

- bandwidth-heavy full-canvas uploads;
- excessive temporary buffers;
- small-work dispatch overhead;
- CPU/GPU split mistakes;
- memory-pressure and allocation cliffs.

Every renderer experiment should run on the Intel adapter and the NVIDIA
adapter. A win only on NVIDIA is not a portable win.

### Desktop discrete GPU

Discrete GPUs favor large parallel kernels but make CPU-to-GPU transfers more
visible. They are the strongest case for:

- GPU-resident derived data;
- compact incremental uploads;
- large enough batches;
- asynchronous background cache construction;
- compute-heavy specialized kernels where occupancy is healthy.

### Apple mobile and Apple Silicon

Apple GPUs are tile-based. Render-pass locality, attachment lifetime, and
avoiding unnecessary device-memory traffic can matter as much as arithmetic
throughput. This makes fragment/render-pass and tile-local variants important;
it does not prove compute is bad.

Vello Hybrid's render-centric composition may therefore be a better candidate
than Classic's globally compute-centric pipeline on these devices, but this is
an inference to benchmark, not an established result for Sketchpad.

### Android

Android is a broad device/driver range rather than one target. Start with a
conservative Vulkan path and a small number of capability profiles. Measure
shader/pipeline creation, persistent caches, thermal behavior, and memory
pressure. `wgpu` exposes a
[`PipelineCache`](https://docs.rs/wgpu/latest/wgpu/struct.PipelineCache.html);
its safety contract and device-specific cache data must be handled carefully.

### CPU-only or broken-GPU mode

A CPU renderer is valuable for:

- headless export and tests;
- remote or virtual systems;
- device-loss recovery UI;
- broken/blocked GPU configurations;
- potentially small scenes on weak integrated graphics.

It does not need to implement every experimental wet or procedural brush on day
one. Unsupported brushes can use a canonical rasterized fallback for export if
that behavior is specified and disclosed.

## What We Should Own

Owning the following is proportionate to the product:

- canonical-to-derived scene extraction;
- brush graph evaluation and classification;
- active stroke prefix/tail/prediction lifecycle;
- spatial indexing, damage, and viewport queries;
- render-plan and pass dependencies;
- sparse tile and LOD caches;
- custom brush kernels;
- layer compositing contract;
- resource budgeting and cache eviction;
- adapter profiling and shader selection;
- GPU timing, validation images, and performance corpus;
- surface lifecycle and device-loss reconstruction.

These are architectural even if every path is initially drawn by Vello.

## What We Should Reuse

Reuse should be the default for:

- path and transform mathematics (`kurbo` or equivalent);
- color/style vocabulary where it matches the product (`peniko` or equivalent);
- text shaping and font parsing;
- image decoding;
- conventional path fill/stroke fallback;
- CPU raster baselines;
- shader translation and native API portability through `wgpu`;
- testing corpora and established compositing equations.

Code can be reused without adopting its scene as the document or its renderer
as the scheduler.

## The Real Cost of a Custom Renderer

### Reasonable scope

A focused backend for one mark family is a bounded research project:

- define a compact primitive stream;
- create one or several pipelines;
- cull/bin primitives;
- evaluate coverage or density;
- composite into a damaged region;
- validate against a reference;
- profile on the target matrix.

Several such backends sharing a render plan and cache system are ambitious but
aligned with Sketchpad's differentiation.

### Dangerous scope

The project becomes unbounded if “raw `wgpu`” means all of:

- a complete arbitrary-path rasterizer;
- complete stroking and dash semantics;
- all clipping and layer/filter behavior;
- full text and color-glyph rendering;
- comprehensive SVG/PDF imaging;
- a high-performance CPU renderer;
- independent native GPU abstractions;
- novel brush research at the same time.

That is no longer a drawing application with a specialized renderer. It is a
general graphics-platform project competing with Vello and Skia before the
product exists.

The renderer contract should make this scope visible. Unsupported conventional
operations route to a component or remain explicitly unsupported; they must not
quietly expand the custom core.

## Experiment and Adoption Gates

### Representative corpus

No candidate can be selected with SVG Tiger or Paris alone. The corpus needs:

1. a long pressure-sensitive hard stroke with tight turns;
2. ten thousand and one hundred thousand short marks at multiple zooms;
3. dense translucent self-overlap;
4. a soft airbrush with repeated deposition;
5. a textured stamp brush;
6. active real and predicted tails over a cached committed prefix;
7. clipped/masked layers and conventional vector content;
8. large pan/zoom with sparse damage and cache misses;
9. tiny complex geometry when zoomed far out;
10. memory pressure, resize, suspend/resume, and device recovery.

### Measurements

Record:

- input-to-visible and sample-to-submit latency where available;
- p50, p95, and p99 CPU frame preparation;
- p50, p95, and p99 GPU time by pass;
- missed 60/90/120 Hz deadlines;
- bytes uploaded and copied;
- temporary and persistent GPU memory;
- primitive expansion ratio;
- damage area and cache hit rate;
- first-frame and pipeline-creation stalls;
- background rebuild time;
- sustained performance and power/thermal behavior on mobile;
- visual difference from the reference at target zooms.

Average frames per second alone hides the failures that make drawing feel bad.

### Backend adoption rules

Proposed rules:

- Use Vello Hybrid for a conventional operation if it is correct, stable enough,
  and its total frame behavior is close enough that custom work would not
  improve the product.
- Build a custom backend when the mark cannot be expressed correctly, or when a
  representative workload shows a large and persistent latency, memory,
  bandwidth, or power win.
- Keep Vello or another renderer as a benchmark even when custom wins.
- Never encode a Vello `Scene`, sparse strip, GPU buffer, or shader choice as
  canonical saved data.
- Stop expanding a custom backend when it ceases to outperform the reused
  implementation on its intended workload.

“Close enough” should be decided in frame-budget terms. A 25% slower pass can
be irrelevant at 0.2 ms and fatal at 7 ms.

## Immediate Research Sequence

No production code is implied by this note. When implementation experiments
begin:

1. Freeze representative mark semantics and golden images.
2. Define the narrow render-plan boundary before integrating a renderer.
3. Build a profiling harness that can force Atlas's Intel and NVIDIA adapters.
4. Align `wgpu` versions in an isolated Vello Hybrid experiment.
5. Measure Vello Hybrid, Vello Classic, and a simple Lyon/`wgpu` baseline for
   conventional hard paths.
6. Implement one genuinely product-specific `wgpu` backend—preferably the
   active hard or continuous-density stroke—to measure the value of ownership.
7. Test the same corpus on at least one Apple mobile and two materially
   different Android GPUs before calling the architecture mobile-ready.
8. Introduce capability-selected variants only when the measurements identify
   the relevant bottleneck.
9. Consider a native API escape hatch only after a `wgpu` ceiling is isolated.

## Bottom Line

Vello is worth using **as leverage**, not as the identity of Sketchpad's
renderer.

Raw `wgpu` is worth using **as the foundation**, because Sketchpad's important
work is specialized enough to justify owning its scheduling, storage, and hot
kernels. It is not worth using as an excuse to rebuild all of general 2D
graphics.

The intended strategy is:

> portable semantics, a conservative correctness path, shared `wgpu`
> infrastructure, measured shader/pass variants per capability profile, and
> native specialization only when a proven ceiling pays for its maintenance.

That gives Sketchpad the chance to be unusually fast where it matters without
making every platform a separate renderer project.
