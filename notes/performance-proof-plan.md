# Performance Proof Plan

Status: governing optimization plan, 2026-07-25. This note turns the current
research and Apollo measurements into an ordered program of experiments. It
does not select a speculative rewrite. Its purpose is to make Sketchpad much
faster without accidentally changing the drawing result, hiding work outside a
timer, or optimizing only for a powerful discrete GPU.

Product scope remains in
[first-usable-product.md](first-usable-product.md). General implementation
doctrine remains in
[performance-aware-code.md](performance-aware-code.md). Workload definitions,
raw-result requirements, and hardware protocols remain in
[performance-laboratory.md](performance-laboratory.md).

## Decision

Build a **performance proof system** around the current focused `wgpu` and
sparse CPU-raster architecture, then remove measured work in descending order.

The first optimization phase must be bit-exact relative to the current
canonical linear premultiplied `f32` raster. It will target:

1. work that does not contribute to the result;
2. memory copied at a granularity much larger than the change;
3. repeated CPU/GPU preparation within one display opportunity;
4. transfer and render work repeated for unchanged content;
5. hot loops whose instruction/data shape remains poor after the preceding
   amplification is removed.

Do not begin by replacing `wgpu`, moving the brush wholesale to the GPU,
changing the canonical pixel format, adding approximate LOD, or writing SIMD
intrinsics. None of those changes is supported as the first bottleneck by the
current evidence. Raw Vulkan would expose more controls but would not itself
remove the redundant snapshots, uploads, visibility rebuilding, or
one-submit-per-input-sample schedule that exist above the API boundary.

“10× faster” is a useful ambition but not one scalar acceptance test. Sketchpad
will pursue and report independent improvement factors for snapshot traffic,
upload calls and bytes, CPU hot-path time, GPU dirty/full-view time, and
input-to-submit latency. End-to-end visible latency also has a lower bound set
by sampling, display refresh, the compositor, and scanout.

## What the Current Evidence Proves

The canonical Wacom trace contains 68 samples over 385.124 ms and emits 450
dabs across 45 damaged 128×128 tiles. The measured work exposes several clear
amplification points.

### Whole-tile undo is a known traffic source

One 128×128 `Rgba32Float` tile occupies 262,144 bytes. A dense replay of the
canonical stroke records one complete before-image for each of its 45 touched
tiles: 11,796,480 bytes, or 11.25 MiB, even though much of each tile is not
changed. The current 1,000-stroke dense corpus records roughly 3.16 GiB of
snapshot bytes; the stress corpus has measured roughly 3.57 GiB.

This proves excessive snapshot traffic. It does not yet prove the best
replacement block size. An 8×8, 16×16, or 32×32 first-write shadow scheme
trades copied pixels against metadata, lookup work, and fragmentation. For
reference, one 16×16 `f32` RGBA block is exactly 4 KiB.

### Upload work is amplified across input samples

The current GPU replay issues 128 tile uploads while processing 68 input
samples. Depending on the initial scene, logical uploaded data for the
canonical stroke has measured approximately:

| Scene | Logical upload bytes |
| --- | ---: |
| empty | 13.788 MiB |
| sparse | 9.623 MiB |
| dense | 3.607 MiB |
| stress | 3.607 MiB |

These bytes are submitted through `Queue::write_texture`. Current GPU
timestamps cover the render pass, not execution of the implicit staging copy.
The data proves call/byte amplification at the application boundary; it does
not yet reveal copy-engine time, driver staging cost, row-padding cost, or the
best transfer mechanism.

### The replay schedule is not the intended application schedule

The offscreen replay deliberately submits one frame after every recorded input
sample. It therefore produces 68 submissions for this trace. That was useful
for exposing per-sample work but is not the desired event-loop policy.

The trace averages about 176.6 samples per second. Its average arrivals per
display frame are therefore approximately:

| Refresh | Samples per frame |
| ---: | ---: |
| 60 Hz | 2.94 |
| 120 Hz | 1.47 |
| 144 Hz | 1.23 |
| 240 Hz | 0.74 |

Document semantics require processing every real sample in order. Presentation
does not require submitting a separate frame for every sample. The candidate
policy is to drain all available input, retain its exact semantic order, and
prepare/submit at most one late-latched display frame per display opportunity.

### Per-frame scene preparation repeats document-wide work

The current visible-scene path gathers allocated tile coordinates, allocates a
visible vector, sorts it, builds an allocated-coordinate set, creates
per-page instance vectors, and rewrites instance buffers. Repeating that work
for unchanged tiles after every input sample is a strong architectural
hypothesis, especially as documents grow. It still needs an isolated scaling
test because current fixed-scene timings combine it with other work.

The intended replacement is persistent visibility and instance state updated
from camera, residency, allocation, and damage changes—not a permanent ban on
sorting or hash tables outside hot paths.

### Apollo establishes an integrated-hardware floor

On a clean, AC-powered Apollo in the performance profile, the current
1,000-stroke CPU corpus measured:

| Scene | Full-stroke median | Full-stroke p95 |
| --- | ---: | ---: |
| empty | 1.113 ms | 2.740 ms |
| sparse | 1.307 ms | 2.605 ms |
| dense | 1.321 ms | 2.611 ms |
| stress | 1.432 ms | 2.644 ms |

Controlled scheduled 1× GPU repetitions measured these per-sample p95
medians:

| Scene | Application to submit | GPU render pass |
| --- | ---: | ---: |
| empty | 0.950 ms | 1.556 ms |
| sparse | 0.866 ms | 2.801 ms |
| dense | 0.951 ms | 3.866 ms |
| stress | 1.000 ms | 4.430 ms |

GPU render time grows with painted screen coverage even though
application-to-submit time remains comparatively flat. On Apollo this is
consistent with fill, texture, and shared-memory pressure, but it is not proof
that raw memory bandwidth is the limiting counter. Frequency state also
matters: sustained unrelated GPU work can raise the integrated GPU clock and
make an isolated pass look faster while making the machine as a whole less
representative.

Cold, naturally paced, warm, and sustained protocols must remain separate.

### Process-wide CPU counters identify a broad shape, not a hot function

An exploratory Apollo `perf stat -r 3 -d` run around the existing CPU replay
reported approximately:

- 5.582 seconds of task clock at 0.999 CPUs;
- 17.95 billion cycles;
- 14.49 billion instructions;
- 0.81 instructions per cycle;
- 671 million branches with a 1.74% miss rate;
- 30.3 million last-level-cache loads with a 2.86% miss rate;
- about 45,000 page faults.

Those process-wide counters include scene construction, checksums, undo
verification, aggregation, and result formatting in addition to the timed
stroke transactions. They therefore do **not** prove that the paint kernel has
0.81 IPC or is LLC-bound. Detailed symbol capture was also disproportionately
large and slow to symbolize on Apollo.

The correct follow-up is a compact profiling workload with scene preparation,
large checksums, and reporting outside a long repeated hot interval. Only then
should hardware counters guide a kernel change.

## Exact Quality Contract

The canonical CPU raster remains the reference oracle. During the first
optimization phase every candidate must preserve:

- the exact number and order of emitted dabs;
- bit-identical canonical pixels after every declared checkpoint;
- the final whole-layer checksum;
- exact undo restoration and redo reproduction;
- identical results for full and dirty rendering paths;
- identical results across supported input-drain batch sizes;
- identical output from scalar and specialized kernels;
- GPU readback equality for transfer/cache/presentation changes.

The oracle must compare useful intermediate states, not only the final image.
A final checksum can conceal a transient wrong frame or an invalid batching
assumption.

Required checkpoint schedules:

1. after each recorded input sample;
2. after batches of 1, 2, 4, 8, and all currently available samples;
3. after commit, undo, and redo;
4. after dirty upload and forced full upload;
5. after scalar and candidate optimized kernels;
6. after current and candidate snapshot restoration.

Failures must report the first divergent checkpoint, affected tile/damage
region, both checksums, and optionally a reference/difference image. Expensive
checksums and readbacks belong outside measured intervals.

Changing canonical storage to `f16` or another 8-byte pixel is a later,
explicit quality experiment, not an invisible performance optimization. A
compact GPU presentation cache may be tested earlier only if the final surface
readback passes its declared exact or visual-error contract.

## Performance Proof System v2

### 1. Exact scheduled replay oracle

Extend recorded replay so one semantic trace can be drained with different
batch schedules while preserving sample order. Record intermediate canonical
checksums and damage after each declared boundary. This becomes the gate for
frame coalescing, tile binning, snapshot changes, and kernel specialization.

Success:

- all schedules agree at corresponding semantic boundaries;
- the test catches a deliberately perturbed sample/order;
- oracle work is excluded from reported performance spans.

### 2. Region-dominated CPU profiling workload

Create a prepared-scene workload that:

- builds and warms the selected scene before the measured loop;
- repeats deterministic paint/undo transactions enough that setup is
  negligible;
- performs no full-canvas checksum, JSON serialization, or allocation-heavy
  report generation inside the hot interval;
- verifies exact pre/post checksums outside that interval;
- reports stable work counters and permits Linux `perf` attachment;
- can isolate resampling/dab generation, tile binning/lookup, first-write
  snapshotting, blending, bounds/damage maintenance, commit, and undo.

The first implementation is `cpu_profile_replay`, invoked through:

```text
scripts/apollo benchmark profile --scene dense --hot-strokes 10000
```

It verifies exact 1/2/4/8/all-sample checkpoints before warmup, precomputes
the transformed hot strokes, prints a `profile-ready` process ID, and performs
no checksum or serialization inside its aggregate paint-plus-undo interval.
Coarse paint/undo subdivision remains the next instrumentation step.

Start with coarse spans, then subdivide only the largest span. Build profiling
releases with line tables and frame pointers. Use allocation tools separately
from hardware counters.

### 3. Undo shadow-granularity experiment

Instrument the same exact stroke transactions without initially changing
production undo. For each touched tile, count unique first-written 8×8, 16×16,
and 32×32 blocks and compute:

- payload bytes;
- metadata bytes;
- block lookup/mark operations;
- copied bytes divided by actually changed canonical bytes;
- restore work;
- empty, sparse, dense, erase, and large-brush behavior.

Then implement the best one or two candidates behind the exact undo oracle.
The first gate is a substantial reduction in snapshot bytes on dense/stress
traces with no unacceptable regression on small strokes.

`cpu_profile_replay --shadow-strokes N` now implements the measurement-only
portion for the current hard-round kernels. It captures pixels in committed
damage, undoes, and compares the exact before/after state. Because current
paint and erase are monotonic within a gesture, final changed blocks equal the
blocks requiring a first-write snapshot. This equivalence must not be extended
to a future kernel that can mutate and restore the same pixel within one
gesture; that kernel needs direct first-write marking.

The first clean Apollo matrix at revision `5abeaf3` reduced modeled snapshot
bytes by 4.97–5.02× for 8×8 blocks, 3.84–3.89× for 16×16 blocks, and
2.66–2.70× for 32×32 blocks across sparse/dense/stress scenes. Eight-pixel
blocks used about 3.1 times as many block records as 16-pixel blocks. Advance
both 8×8 and 16×16 to direct implementation/performance tests; drop 32×32
from the first comparison. The full table is in
[performance-laboratory.md](performance-laboratory.md).
The profiler now accepts explicit brush mode, diameter, and opacity so the
pending large-brush and eraser rows retain their workload configuration in the
raw artifact.

The completed rows keep 8×8 at about 5× and 16×16 at about 3.86× less modeled
traffic for a 48 px eraser. At 192 px paint the candidates narrow to 2.33× and
2.15× respectively, while 8×8 creates 3.68 times as many payload records.
Prototype 16×16 first, then compare 8×8 directly if record/copy overhead leaves
room. The 16×16 pixel payload is 4 KiB, but that convenient size is a layout
hypothesis rather than a presumed performance win.

The first `UndoStorage::Blocks16` implementation was introduced behind
`cpu_profile_replay --undo-storage blocks16`; whole-tile remained the default
while it was measured.
It uses conservative edit bounds rather than the final-change lower bound,
deduplicates blocks with a per-tile bitset, keeps payload pixels contiguous per
tile, and row-copies/cross-swaps 16-pixel spans. Exact tests cover cancellation,
allocation, reclamation, undo, redo, and the full recorded-trace checkpoint
oracle. The profiler separately reports paint/capture time and undo time plus
capture/swap blocks and bytes.

The initial Apollo results at revision `bb5f7c8` used an adversarial
one-paint/one-undo transaction loop. On dense 48 px paint, pure blocks cut the
reported paint/capture interval by 15.0% and before-pixels by 3.44×, while
explicit undo rose from 0.007 to 0.206 ms. Empty-scene paint was flat, eraser
paint improved 2.8%, and 192 px paint regressed 0.7%.

The first `Hybrid16` experiment at revision `9dff4da` was rejected. It kept
the zero-payload whole-state marker for new tiles and chose a whole snapshot
when the *first* conservative edit covered at least half of a tile. Recorded
strokes enter a tile through a small leading footprint, including the 192 px
brush, so the threshold never fired. It retained the same block counts while
making the block path materially slower. The experiment was removed rather
than hidden behind an unmeasured constant.

That failure also exposed a workload-definition problem. Repeating
paint-one/undo-one measures an unusual allocator and history-ownership cycle.
Revision `ed948f3` changes result format version 2 to paint a configurable
burst—64 strokes by default—before undoing the burst to restore the exact
initial raster. It reports paint, explicit undo, history destruction, and
remaining harness overhead separately. `--transaction-batch-strokes 1`
preserves the adversarial control.

Clean batch-64 Apollo rows at revision `ed948f3` are:

| Workload | Storage | Paint/stroke | Undo/stroke | Before bytes |
| --- | --- | ---: | ---: | ---: |
| dense, 48 px paint | whole | 1.382 ms | 0.0017 ms | 6,321.5 MiB |
| dense, 48 px paint | blocks16 | 1.170 ms | 0.2711 ms | 1,836.5 MiB |
| stress, 192 px paint | whole | 3.693 ms | 0.0022 ms | 4,490.0 MiB |
| stress, 192 px paint | blocks16 | 3.811 ms | 0.6439 ms | 2,368.1 MiB |

Thus 16×16 history improves the normal 48 px drawing interval by 15.4% and
reduces before-pixels 3.44×, but regresses 192 px drawing by 3.2% for a 1.90×
reduction. A 128 px stress row—the tile width—still favors blocks by 1.1% in
the paint interval and reduces before-pixels 2.24×. Revision `1678118`
therefore selects blocks once at gesture start when brush diameter is at most
the tile width and whole storage above it. Unhinted general raster gestures
conservatively use whole storage.

`adaptive16` exact replays choose the measured control behavior: at 48 px they
report 1,836.5 MiB and the block checksum; at 192 px they report 4,490.0 MiB,
zero block swaps, and the whole checksum. History entries carry their concrete
representation, so exact mixed undo requires no document-format change. Whole
and blocks remain explicit benchmark controls.

### 4. Transfer-path experiment

Compare, with identical final readback:

1. current per-damage `Queue::write_texture`;
2. one tightly coalesced dirty rectangle per tile per display frame;
3. a reusable staging belt/ring plus explicit
   `copy_buffer_to_texture`;
4. full-tile transfer as a control.

Record logical dirty bytes, actual staging bytes including row alignment,
API call count, CPU preparation time, submit count, copy execution time when
available, render-pass time, and final GPU output checksum. The objective is
not merely fewer calls; it is less total CPU/driver/copy work at the intended
frame schedule.

Revision `9417f45` establishes the first instrumented baseline and changes the
live boundary: damage packets now union into one pending rectangle per
resident tile and flush during frame preparation instead of calling
`Queue::write_texture` immediately from every input event. The offscreen
one-submit-per-sample control intentionally still flushes every sample.

On Apollo's Intel iGPU, the dense recorded stroke produces 128 damage regions
and therefore 128 partial uploads in that control. The final checksum remains
`7d45a406ea4f3667`. The new transfer counters report:

| Quantity | Dense stroke |
| --- | ---: |
| logical dirty pixels | 3.607 MiB |
| address span of strided CPU source rows | 9.303 MiB |
| tightly packed 256-byte-aligned staging candidate | 4.183 MiB |
| scheduled `write_texture` API CPU total | 5.07–6.42 ms |
| unpaced `write_texture` API CPU total | 28.29 ms |

The address span is not a claim about bytes copied internally by wgpu; it
measures the source range implied by the current full-tile row stride. The
padded number is the explicit compact-staging candidate. The unpaced API spike
is consistent with cold allocation or queue backpressure but is not yet
attributed; it must not be labeled GPU copy execution.

Next, make the replay drain all samples ready for one display opportunity
before frame preparation. That should exercise the live coalescer, reduce
upload calls, and give the explicit staging comparison a representative
submit schedule.

Revision `e428e8a` completes that replay step. `--display-hz 60,120` adds
display-paced rows for every requested playback rate. All ready samples are
processed in semantic order, then one frame is prepared and submitted.
Per-sample ready-to-process wait is reported separately from per-frame
deadline miss; intentional wait for the next display opportunity is not
misclassified as scheduler lateness.

The dense Apollo matrix remains exact at checksum `7d45a406ea4f3667`:

| Rate/display | Frames | Uploads | Logical bytes | `write_texture` CPU | Frame-late p95 | Sample wait p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1×, per sample | 68 | 128 | 3.607 MiB | 11.57 ms | 15.19 ms | 15.19 ms |
| 1×, 60 Hz | 22 | 75 | 4.065 MiB | 2.99 ms | 0.45 ms | 15.92 ms |
| 1×, 120 Hz | 27 | 81 | 3.946 MiB | 3.21 ms | 0.43 ms | 8.15 ms |
| 2×, 60 Hz | 12 | 62 | 4.216 MiB | 2.16 ms | 0.43 ms | 16.15 ms |
| 4×, 60 Hz | 6 | 52 | 4.361 MiB | 1.63 ms | 0.42 ms | 16.48 ms |

The per-sample 1× row missed deadlines on this run because one-submit-per-input
GPU work can backpressure the schedule; it is a control, not a target pacing
policy. Coalescing cuts calls and API CPU substantially, but rectangle unions
amplify logical bytes by 12.7%–20.9% in the 60 Hz rows. The next representation
should retain multiple rectangles when their union's extra byte cost exceeds
the measured per-call cost. Do not replace this tradeoff with a fixed
“one rectangle per tile” dogma.

### 5. Renderer isolation matrix

Vary independently:

- allocated tiles;
- visible tile instances;
- painted on-screen coverage;
- changed tile count and dirty area;
- current direct `Rgba32Float` presentation;
- a persistent composited display cache;
- cold, warm, and sustained integrated-GPU frequency state.

This separates visibility preparation, instance bandwidth, texture bandwidth,
fill, and DVFS effects. Use platform GPU counters only after a stable timing
case identifies the interesting row.

### 6. Frame-paced application replay

Feed the real trace through display opportunities at 60, 120, 144, and 240 Hz.
Process all input samples in source order, but drain/coalesce rendering work
before one late-latched frame submission. Compare against one-submit-per-sample
as a diagnostic control.

Measure:

- samples drained per frame;
- input receipt to semantic paint completion;
- paint completion to submit;
- submit to GPU completion;
- presentation time when available;
- missed display opportunities;
- upload calls/bytes and visibility rebuilds per displayed frame;
- exact per-checkpoint raster and final output.

Do not drop or merge document samples merely to improve presentation numbers.
Prediction, if added later, must be a replaceable transient layer over exact
committed input.

## Ordered Optimization Hypotheses

Only promote a hypothesis after its experiment produces a reproducible win.
The present order is:

1. **One render submit per display opportunity.** Drain all ready input and
   preserve semantic order, but avoid rebuilding/submitting after every sample.
2. **Dab-to-tile binning.** Compute affected tiles once and process ordered dab
   batches per tile, while preserving within-tile compositing order.
3. **Microblock first-write undo.** Copy only changed blocks once per gesture.
4. **Per-frame dirty coalescing and reusable staging.** Upload each changed
   tile/region once per displayed frame.
5. **Persistent visibility and instance data.** Rebuild only when camera,
   allocation, or residency state requires it.
6. **Stable display cache plus active dirty path.** Avoid repeatedly sampling
   or compositing unchanged dense content where the device benefits.
7. **Specialize the hard-round scanline kernel.** Compute fractional edge
   coverage only where necessary, fill exact opaque interiors in bulk, and
   arrange loops for autovectorization without changing canonical results.
8. **Compiler and ISA specialization.** After the dataflow work, measure
   `codegen-units=1`, LTO, profile-guided optimization, and runtime-dispatched
   SIMD variants. Do not assume any one is free or faster on every target.

These factors are not blindly multiplicative. Each accepted change is
remeasured because removing one bottleneck changes the relevance of the next.

## Gates

Each performance change must pass correctness, work, latency, and portability
gates.

### Correctness gate

- exact canonical checkpoints pass;
- undo/redo and damage contracts pass;
- GPU output/readback contract passes when applicable;
- no scene or quality case is silently skipped.

### Work gate

Report before/after counts appropriate to the change:

- copied snapshot and restore bytes;
- actual changed canonical bytes;
- upload calls, logical bytes, and padded staging bytes;
- tile lookups, dab/tile intersections, and pixel visits;
- allocations and retained capacities;
- visibility scans, sorts, instance writes, and submissions.

### Latency gate

- compare identical release workloads and machine states;
- report median, p95, p99 where sample count supports it, and maximum;
- separate input processing, CPU preparation, copy, render, and presentation;
- retain raw results rather than only a summary.

### Portability gate

- Apollo is the continuous integrated/low-power check;
- Atlas integrated and discrete measurements resume only when Atlas is idle;
- mobile validation must eventually include at least two materially different
  tile-based GPU families and sustained thermal runs;
- an optimization may specialize per capability, but a correct portable path
  remains available.

## Near-Term Execution

### Phase A — prove semantics and measurement boundaries

1. add exact intermediate replay checkpoints and batching invariance;
2. add the region-dominated profiling workload;
3. verify both on Apollo and capture a fresh CPU counter baseline.

### Phase B — remove the clearest memory amplification

1. run the implemented 8×8, 16×16, and 32×32 shadow-block traffic measurement
   across empty, sparse, dense, stress, eraser, and large-brush traces;
2. implement 16×16 behind the exact oracle, retaining whole-tile undo as the
   control;
3. compare actual snapshot bytes, capture time, undo/redo time, and retained
   memory;
4. implement/retain 8×8 only if its lower traffic can overcome roughly
   3.1–3.7 times as many block records in the relevant cases.

### Phase C — coalesce per-frame GPU work

1. add frame-paced replay without changing document semantics;
2. coalesce dirty regions per tile per display frame;
3. compare direct writes with a reusable staging ring;
4. timestamp or counter the copy path where supported.

### Phase D — persist stable renderer work

1. isolate visibility versus painted-coverage scaling;
2. retain visibility and per-page instance data across unchanged frames;
3. test a stable composited display cache on Apollo;
4. decide whether cache format specialization is justified.

### Phase E — optimize the remaining hot kernel

1. profile the new system rather than assuming the old hotspot remains;
2. restructure the largest exact kernel around batches and contiguous rows;
3. inspect generated code;
4. test compiler, PGO, and runtime SIMD variants;
5. keep only wins that survive Apollo and the broader hardware matrix.

## Research Basis

Performance-aware structure and measurement:

- Casey Muratori,
  [Welcome to the Performance-Aware Programming Series](https://www.computerenhance.com/p/welcome-to-the-performance-aware)
- Casey Muratori,
  [Performance-Aware Programming table of contents](https://www.computerenhance.com/p/table-of-contents)
- Casey Muratori,
  ["Clean" Code, Horrible Performance](https://www.computerenhance.com/p/clean-code-horrible-performance)
- Nicholas Nethercote et al.,
  [The Rust Performance Book: Profiling](https://nnethercote.github.io/perf-book/profiling.html)
- Nicholas Nethercote et al.,
  [The Rust Performance Book: Heap Allocations](https://nnethercote.github.io/perf-book/heap-allocations.html)
- Rust Project,
  [rustc code-generation options](https://doc.rust-lang.org/rustc/codegen-options/index.html)
- Rust Project,
  [Profile-guided optimization](https://doc.rust-lang.org/rustc/profile-guided-optimization.html)
- Rust Project,
  [`std::arch`](https://doc.rust-lang.org/std/arch/index.html)

Transfer, presentation, and mobile pacing:

- wgpu,
  [`Queue`](https://wgpu.rs/doc/wgpu/struct.Queue.html)
- wgpu,
  [`StagingBelt`](https://docs.rs/wgpu/latest/wgpu/util/struct.StagingBelt.html)
- wgpu,
  [`SurfaceConfiguration`](https://wgpu.rs/doc/wgpu/type.SurfaceConfiguration.html)
- wgpu,
  [`PresentMode`](https://docs.rs/wgpu/latest/wgpu/enum.PresentMode.html)
- Khronos,
  [Vulkan tile-based rendering best practices](https://docs.vulkan.org/guide/latest/tile_based_rendering_best_practices.html)
- Android Developers,
  [Frame Pacing library](https://developer.android.com/games/sdk/frame-pacing)
- Android Developers,
  [`CanvasFrontBufferedRenderer`](https://developer.android.com/reference/androidx/graphics/lowlatency/CanvasFrontBufferedRenderer)
- Khronos,
  [`VK_KHR_incremental_present`](https://registry.khronos.org/VulkanSC/specs/1.0-extensions/man/html/VK_KHR_incremental_present.html)
- Intel,
  [GPU Offload analysis](https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2025-1/gpu-offload-analysis.html)

## Bottom Line

The current architecture has enough measured redundant work that a major
speedup is plausible without lowering brush quality. The strongest first
targets are transaction granularity, per-frame work coalescing, and persistent
renderer state—not a new graphics API or approximate pixels.

The rule for every optimization is:

> Preserve the same drawing, measure the exact work removed, and prove the win
> on constrained hardware before making it architecture.
