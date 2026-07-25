# Performance and Correctness Laboratory

Status: design specification, 2026-07-24. This defines the laboratory Sketchpad
needs before selecting or optimizing renderer backends. It does not select an
implementation crate and does not imply production code work now.

The laboratory's immediate priority is the sparse-raster painter defined in
[first-usable-product.md](first-usable-product.md), especially active drawing
and color mixing over existing dense tiles. Renderer and multiscale comparisons
remain later suites.

The code being measured should follow the control-plane/pixel-plane split,
allocation policy, and instrumentation rules in
[performance-aware-code.md](performance-aware-code.md). The laboratory tests
those rules; it does not replace them.

## Decision

**Build the performance and correctness framework before treating any renderer
comparison as architectural evidence.**

The central benchmark object is not a static picture. It is a reproducible
**state transition under a timed trace**:

```text
document before the action
        +
input/edit/view trace
        +
declared cache and device state
        +
renderer/backend configuration
        ↓
per-frame semantic result, pixels, timings, memory, and internal work
```

This directly supports the important case: drawing a new stroke over existing
strokes. The framework varies what already exists, where the new stroke lands,
which caches are valid, what the camera is doing, and what correctness is
required.

Average frame rate from a static demo is not sufficient. It cannot reveal:

- active-stroke latency;
- prediction correction cost;
- a slow finalize frame;
- cache invalidation amplification;
- old-operation edit/replay cost;
- pan/zoom cache misses;
- pipeline compilation stutter;
- p95/p99 frame misses;
- memory growth;
- mobile thermal collapse;
- a faster result obtained by silently lowering image quality.

## What the Laboratory Must Answer

For any candidate representation, renderer, shader, or cache policy:

1. Is the result semantically correct?
2. What is its visual error at the relevant view scales?
3. What work happens on the CPU, GPU, and storage path?
4. How does cost change with existing scene density and overlap?
5. How does cost change with damaged area rather than total document size?
6. What are cold, warm, partially invalid, and memory-pressure behaviors?
7. What happens during active input, finalization, edits, and navigation?
8. What is the latency distribution, not just its mean?
9. What memory and transfer traffic purchases that latency?
10. Does performance sustain on integrated and mobile hardware?
11. Which capability or algorithm explains a win?
12. Does a specialization still pass the same semantic and quality contract?

## Testing Layers

The laboratory needs four layers. A result must identify which layer produced
it; numbers from different layers are not interchangeable.

### 1. Algorithm and kernel tests

Isolate one operation:

- curve fitting or stroke expansion;
- tile/cell candidate binning;
- path flattening;
- analytic coverage;
- density integration;
- stamp lookup;
- compositing;
- prefix/reduction/sort;
- cache lookup and invalidation;
- color conversion.

These tests explain mechanisms and support parameter sweeps. They do not prove
interactive performance because they omit scheduling, uploads, presentation,
and interference from other passes.

### First implemented CPU raster replay

`raster_bench` is the first dependency-free kernel/transaction replay. Run it
as a release build:

```text
cargo run --release --bin raster_bench -- --runs 12
```

It replays the same 192-dab soft stroke over:

- an initially empty sparse layer;
- an already-painted region;
- 128×128 tiles;
- 256×256 tiles.

The timed interval includes gesture creation, tile lookup/allocation, one
before-image per existing touched tile, contiguous CPU blending, damage
tracking, commit-time content-bound calculation, and empty-tile reclamation.
Document setup and the output checksum are outside the interval.

Each row reports minimum, median, p95, and maximum wall time together with tile
lookups, bulk edits, allocation count, before-image count and bytes,
conservatively touched pixels, resident tiles, and checksum stability.

This replay is intentionally not a complete brush benchmark:

- its blend is ordinary premultiplied source-over, not pigment mixing;
- it has no normalized tablet-input trace yet;
- it does not include resampling, stabilization, GPU work, upload, compositing,
  presentation, or save/recovery;
- the process is not pinned and CPU frequency is not controlled;
- it uses the current `f32` reference tile representation.

It is sufficient to expose amplification in the first sparse raster core. It
must not be used as evidence for end-to-end drawing latency or final tile-size
selection.

#### Atlas baseline, 2026-07-24

Environment:

- Intel Core i7-9750H, 6 cores/12 threads, 2.60 GHz base and 4.50 GHz reported
  maximum;
- 32 KiB L1 data and 256 KiB L2 per physical core, 12 MiB shared L3;
- x86_64 Linux;
- `rustc 1.93.1`, LLVM 21.1.8;
- Cargo `release` profile;
- 2 discarded warmups and 12 recorded runs per case;
- no CPU affinity, fixed-frequency control, or idle-system protocol;
- uncommitted implementation working tree, so this is an initial engineering
  baseline rather than a durable regression threshold.

| Initial state | Tile | Min | Median | p95 | Lookups | Allocations | Before-images | Snapshot traffic |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Empty | 128 | 9.600 ms | 11.506 ms | 12.614 ms | 352 | 42 | 42 | 0 MiB |
| Painted | 128 | 14.825 ms | 16.871 ms | 18.997 ms | 352 | 0 | 42 | 10.5 MiB |
| Empty | 256 | 15.997 ms | 19.117 ms | 23.243 ms | 254 | 16 | 16 | 0 MiB |
| Painted | 256 | 19.963 ms | 21.481 ms | 23.515 ms | 254 | 0 | 16 | 16 MiB |

All cases conservatively covered 0.624 million pixel-visits. Repeated runs had
zero checksum spread. Empty-case checksums agreed across tile sizes, as did
painted-case checksums.

Interpretation:

- 128 tiles had about 40% lower empty-case median time and 21% lower
  painted-case median time in this replay even though they required 39% more
  sparse lookups.
- The 256 painted case copied 16 MiB of before-images versus 10.5 MiB for 128.
  Commit-time content-bound scans also examine the full touched tile, so the
  256 case scans more pixels that the stroke never approached.
- Two conservatively acquired 128 tiles became empty and were reclaimed at
  commit, explaining 42 allocations but 40 resident output tiles.
- This supports the architecture's emphasis on memory traffic and damaged
  extent. It does **not** yet prove 128 is the product tile size: GPU dispatch,
  upload alignment, larger brushes, erasing, filters, mobile GPUs, compact
  formats, and dirty-subrect bound maintenance can change the tradeoff.

Immediate follow-up experiments:

1. avoid rescanning an entire touched tile when a kernel can return exact
   nonempty-bound changes;
2. compare the `f32` reference pixels with 8-byte working pixels;
3. coalesce multiple dab edits by tile before entering the kernel;
4. separate transaction/snapshot time from blend-kernel and commit time;
5. add a recorded input trace and output-image oracle;
6. repeat under CPU affinity/frequency control before setting regression gates.

#### Real hard-round brush baseline, 2026-07-24

`brush_bench` uses the actual `HardRoundStroke` resampler and painter rather
than the synthetic soft-dab loop:

```text
cargo run --release --bin brush_bench -- --runs 12
```

The trace contains 256 pressure-varying input samples across a four-wave path.
It now runs paint over empty and painted content plus destination-out erase over
painted content. The timed region includes sample resampling, hard coverage,
blend/erase, sparse lookup/allocation, gesture snapshots, damage accumulation,
the final cap, and commit. The additive paint brush reports exact changed
bounds while writing, so that path does not rescan touched tiles to recover
content bounds at commit. The destructive eraser currently performs that scan
to preserve exact nonempty bounds and reclaim empty tiles.

| Operation/state | Tile | Min | Median | p95 | Tile edits | Before-images | Snapshot traffic | Bound scan |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Paint/empty | 128 | 6.799 ms | 7.467 ms | 7.924 ms | 905 | 58 | 0 MiB | 0 Mpix |
| Paint/painted | 128 | 10.947 ms | 11.552 ms | 12.658 ms | 905 | 58 | 14.5 MiB | 0 Mpix |
| Erase/painted | 128 | 9.400 ms | 9.577 ms | 10.384 ms | 905 | 58 | 14.5 MiB | 0.002 Mpix |
| Paint/empty | 256 | 12.631 ms | 12.893 ms | 13.964 ms | 733 | 28 | 0 MiB | 0 Mpix |
| Paint/painted | 256 | 17.202 ms | 18.262 ms | 19.095 ms | 733 | 28 | 28 MiB | 0 Mpix |
| Erase/painted | 256 | 16.407 ms | 17.302 ms | 19.451 ms | 733 | 28 | 28 MiB | 0.001 Mpix |

All six cases conservatively processed 0.770 million kernel pixel-visits.
Checksums were identical across tile sizes for the same operation/initial state
and had zero spread across the 12 recorded runs. The additive path scanned zero
pixels solely for content-bound recovery.

For this real brush trace, 128 tiles have about 42% lower empty-paint median,
37% lower painted-paint median, and 45% lower painted-erase median than 256
tiles despite more tile edits. The painted 256 cases copy twice the snapshot
bytes. Exact additive bounds removed the earlier paint commit scan, and
squared-distance rejection now takes a square root only in the one-pixel
antialias fringe. Those changes reduced the 128-tile paint medians from the
initial 9.173/14.264 ms baseline to 7.467/11.552 ms in this run while preserving
the output oracle.

The first eraser run isolated a destructive-bound problem: the 128 case scanned
0.950 million pixels and the 256 case 1.835 million beyond the same 0.770
million kernel visits. The layer now distinguishes exact subtractive edits from
arbitrary edits. Removing interior pixels cannot change a tile's bounds;
removing boundary pixels triggers an inward search from the prior exact bounds;
the general-edit API retains full recomputation. In the adjacent rerun this
reduced bound inspection to 0.002/0.001 million pixels and changed the eraser
medians from 11.901/20.471 ms to 9.577/17.302 ms with identical checksums.

Overall, the replay continues to show memory amplification dominating the
reduction in tile dispatches. It is still a CPU/transaction result, not a
complete frame measurement or final format/tile-size decision.

#### Offscreen sparse-GPU correctness replay

`gpu_smoke` validates the current CPU-to-GPU presentation path:

```text
cargo run --release --bin gpu_smoke
```

On the Intel UHD 630 baseline it:

- rendered eight resident sparse tiles;
- forced those tiles across four two-layer texture-array pages to exercise
  multi-page binding and drawing;
- read back 15,178 dark ink/outline pixels and 312 cursor-colored pixels;
- performed eight initial full-tile uploads totaling 2,097,152 bytes;
- updated an already-resident tile with one 23,104-byte dirty rectangle rather
  than a 262,144-byte full tile;
- completed with no captured `wgpu` validation error.

This is a correctness smoke test, not a timing benchmark. It covers real brush
output, paged tile-array residency, uploads, page bind-group changes,
camera/paper rendering, instanced tile drawing, the brush-cursor overlay,
submission, and texture readback.

### 2. Offscreen renderer replay

Replay a complete document, input trace, and view trace into a fixed-size
offscreen target. This is the main cross-backend comparison level because it
can standardize:

- output dimensions and sample count;
- color and compositing contract;
- initial cache state;
- device and adapter;
- quality/error threshold;
- measured phase boundaries.

It should include all renderer work but exclude display compositor and physical
input latency. GPU timestamp queries are the primary portable measurement when
available. [`wgpu::QueryType::Timestamp`](https://docs.rs/wgpu/latest/wgpu/enum.QueryType.html)
records GPU timestamps; values must be converted using the queue timestamp
period, and the feature must be requested from the adapter.

### 3. Interactive application replay

Run the same trace through the real event loop, surface, pacing, and
presentation path. This catches:

- event batching and scheduling;
- input-to-render queueing;
- surface acquisition;
- CPU/GPU overlap;
- present-mode behavior;
- compositor pacing;
- resize, suspend/resume, and device recovery;
- background refinement stealing active-stroke time.

Internal timestamps can separate input receipt, document update, command
submission, and present request. They cannot by themselves measure photons on
the display.

Current checkpoint: the live executable keeps fixed-capacity timing samples and
reports one-second mean/p95/max distributions for CPU input-event handling and
CPU render-through-submit work. The same record includes dirty upload count and
bytes, visible/resident tiles, allocated GPU pages and capacity,
deferred-visible candidates, evictions, and canonical CPU tile count. A
one-shot `WaitUntil` emits the final active interval, after which the event loop
returns to indefinite `Wait`.

The first sustained Wacom session immediately found a cache-policy failure.
Canonical CPU content grew past 256 tiles while visible/resident GPU tiles
stayed pinned at 256. The renderer selected the nearest candidates, so the
display appeared to collapse into a roughly circular island even though CPU
content remained intact. Sequentially inserting that desired set also caused
massive self-eviction: observed one-second records included 3,552 evictions and
hundreds of MiB of redundant uploads.

The presentation path now allocates 256-layer texture-array pages lazily and
protects the current desired set during replacement. The present 4096×4096,
128-tile canvas has at most 1,024 tiles, matching the four-page ceiling. The
offscreen smoke replay forces an eight-tile drawing through four two-layer
pages to validate the same paging mechanics cheaply.

The repeat physical-Wacom run grew from 0 to 911 canonical and visible tiles.
Capacity stepped from one page/256 tiles through four pages/1,024 tiles;
`deferred` and `evictions` remained zero throughout. This closes the original
presentation-collapse incident for the defined canvas. It also showed that
duplicate XInput tip packets can arrive after newer button packets rather than
only adjacent to their original, so the native adapter now rejects duplicates
against a short per-device button history.

This instrumentation is useful for detecting CPU and traffic amplification,
but it still has no GPU timestamps, compositor/presentation signal, or photon
measurement.

### 4. Device experience and sustained tests

Measure the actual product loop for long enough to expose:

- thermal throttling;
- power and battery cost;
- memory pressure and operating-system intervention;
- driver shader compilation;
- display/compositor behavior;
- input-to-photon latency.

Apple's [Metal performance tools](https://developer.apple.com/metal/tools/)
expose GPU time, frame intervals, resources, counters, and thermal/system
traces. Android's [GPU Inspector](https://developer.android.com/agi/start)
provides Vulkan frame and system profiling, and
[Perfetto FrameTimeline](https://developer.android.com/topic/performance/vitals/render)
is useful for jank investigation. Arm's
[Streamline](https://developer.arm.com/tools-and-software/streamline-performance-analyzer)
can capture CPU, Mali GPU, scheduling, and hardware-counter data.

These platform tools diagnose results from the portable harness. Their metrics
should be attached to a run rather than replacing the common result format.

## Reproducible Workload Artifacts

Every test case should be a versioned, deterministic artifact with the
following components.

### Document recipe or snapshot

Describes the state before playback:

- canonical operations and layer order;
- immutable brush versions;
- deterministic random seeds;
- world bounds and coordinate hierarchy;
- masks, clips, filters, and blend/isolation boundaries;
- representative derived caches only when the test intentionally begins warm.

A procedural recipe is compact and supports sweeps. A frozen snapshot protects
regressions against generator changes. Important cases should retain both.

### Real input trace

A time-ordered stream of normalized samples:

- position and timestamp;
- pressure;
- tilt, orientation/twist, and optional device fields;
- tool state and buttons;
- begin/update/end/cancel boundaries;
- coalesced-sample provenance where relevant.

The authoritative trace contains real samples. Predicted samples should be
generated by a named, versioned predictor or stored in a separate prediction
track. This lets one renderer see exactly the same prediction and correction
sequence as another.

### Edit trace

Timed semantic actions beyond pointer samples:

- begin, update, finalize, and cancel stroke;
- undo and redo;
- transform, recolor, or change brush;
- insert, delete, split, and reorder;
- layer visibility, opacity, masks, and groups;
- save, reopen, and cache eviction;
- simulated device loss and reconstruction.

### View trace

The camera is part of the workload:

- viewport pixel dimensions and scale factor;
- pan, zoom, and rotation over time;
- focal point for zoom;
- target refresh rate and pacing mode;
- continuous view scale, not only integer LOD;
- transitions across coordinate-rebase and LOD boundaries.

### Cache protocol

“Warm” and “cold” need exact meanings. Each run declares one of:

- **cold process**: no pipelines, allocations, scene, or disk caches;
- **cold document**: process/pipelines warm, document-derived data absent;
- **warm viewport**: visible derived data resident;
- **partially invalid**: declared stroke/tile/layer revisions are stale;
- **post-edit**: an early operation changed and affected successors;
- **memory pressure**: a fixed budget forces eviction;
- **post-reopen**: optional persistent caches loaded or rejected;
- **post-device-loss**: CPU/canonical state survives, GPU state does not.

The harness must verify the requested state instead of trusting a label.

### Run configuration

Records all variables needed to interpret a result:

- source revision and dirty-tree fingerprint;
- release/debug/instrumented build mode;
- renderer and backend versions;
- shader and algorithm variant identifiers;
- adapter, vendor/device, driver, backend, features, and limits;
- CPU, operating system, memory, and power mode;
- output format, size, color space, and sample count;
- thread count and affinity policy if controlled;
- warmup, repetitions, and run order;
- quality/error target;
- environment overrides and known workarounds.

## Drawing on Existing Strokes

This must be a first-class benchmark family rather than one large “stress
scene.”

### Background axes

| Axis | Representative values |
|---|---|
| visible mark count | 0, 10, 1,000, 10,000, 100,000 |
| local overlap | empty tile, sparse, moderate, every mark crosses one region |
| opacity | opaque, low-alpha repeated overdraw, mixed |
| media | hard vector, density, stamps, raster, simulation island |
| order | new top mark, insertion near beginning, reorder of old mark |
| layers | one layer, many simple layers, isolated/masked/filter groups |
| spatial distribution | uniform, clustered, long crossing strokes, adversarial scribble |
| scale | natural, far minified, extreme magnification, mixed semantic scale |
| cache state | cold, warm, partial invalidation, memory pressure |

### Foreground axes

| Axis | Representative values |
|---|---|
| stroke length | tap, short, viewport crossing, very long |
| sample rate | slow, typical, burst/high-rate |
| width | hairline, ordinary, very wide |
| curvature | straight, smooth, tight turns, cusps/self-intersection |
| dynamics | constant, pressure taper, rapidly changing width/opacity |
| prediction | none, accurate tail, frequent correction |
| brush | hard edge, soft density, textured stamp, wet/local state |
| damage | local, tile-border crossing, many tiles, full viewport |

The full Cartesian product would be wasteful. Maintain:

- a small canonical suite that runs frequently;
- targeted one-axis sweeps for scaling curves;
- pairwise combinations for likely interactions;
- adversarial cases for known failure modes;
- captured real sessions for ecological validity.

Every bug or surprising performance cliff should be reducible into a minimal
permanent case.

## Required Scenario Families

### Active ink

Measure:

1. receipt of a real sample;
2. smoothing/brush evaluation;
3. stable-prefix update;
4. real-tail and predicted-tail replacement;
5. upload/encoding;
6. GPU passes;
7. present request;
8. correction when prediction is wrong.

The active trace runs over empty, sparse, and densely overlapping backgrounds.
An active-overlay design should ideally scale with the changed tail and damaged
region, not total document size.

### Finalization

The last pointer event may trigger:

- final curve fitting;
- outline/strip/mesh generation;
- cache insertion;
- removal of transient prediction;
- authoritative layer recomposition;
- history/storage transaction;
- background refinement.

Measure the final frame and subsequent recovery frames separately. Hiding a
100 ms finalize operation behind good steady-state averages is unacceptable.

### Old-content edit and replay

Edit the first, middle, and last operation of an ordered dense region. Measure:

- candidate lookup;
- invalidation breadth;
- recomposition/replay work;
- cache reuse;
- visible approximation duration;
- memory needed to accelerate the edit.

This is the test that distinguishes a good retained/cache architecture from a
fast append-only demo.

### Navigation and multiscale behavior

Include:

- slow and rapid pan;
- smooth zoom through several powers of two;
- oscillation around an LOD boundary;
- rotation while zooming;
- jump to a cold distant region;
- deep zoom into tiny geometry;
- zoom out over enormous candidate counts;
- semantic-scale and nested-canvas transitions if supported.

Measure pop-in, refinement latency, LOD hysteresis, query candidates, requested
tiles/nodes, and stale work cancelled after the view moves.

### Memory and recovery

Increase document complexity until cache budgets force eviction. Then revisit
old regions, undo, resize, suspend/resume, and recreate the GPU device. Report
performance as a function of the explicit memory budget rather than available
machine RAM.

### Sustained drawing

Replay realistic draw/pan/zoom loops for minutes on mobile-class hardware.
Record clock/thermal state, frame distributions, memory, and energy proxies.
Separate the initial cool interval from steady thermally limited behavior.

## Correctness Before Speed

### Semantic oracle

Validate non-pixel invariants:

- one gesture becomes one history command;
- predicted input never enters canonical history as real input;
- layer and operation order are preserved;
- eraser semantics match the named operation;
- cache eviction and device loss do not alter the document;
- deterministic seeds reproduce brush behavior;
- a camera change does not change canonical geometry;
- backend/LOD switching stays within its declared equivalence contract.

### Image oracle

Use one or more high-quality references:

- a slow supersampled implementation of the specified brush;
- Vello/Skia/CPU raster output for conventional paths;
- analytic reference equations for simple marks;
- manually reviewed golden images for complete scenarios.

Compare in the named linear working color space using premultiplied-alpha-aware
metrics. Store:

- maximum and distribution of channel error;
- edge-distance or coverage error where appropriate;
- structural/region error for soft media;
- difference images at relevant zoom levels.

Exact pixel identity is useful for deterministic paths but should not be the
only acceptance mode. Two correct antialiasing methods can differ by a
subpixel. Conversely, a low average image error can hide a missing thin stroke.

### Quality is a benchmark dimension

Every result includes its quality mode and measured error. A candidate is not
“faster” if it:

- uses fewer samples;
- changes the brush;
- drops translucent order;
- omits filters or masks;
- renders a lower resolution;
- accepts visibly different joins/caps;
- delays authoritative correction outside the measurement window.

Renderer selection should use a Pareto frontier over latency, memory,
bandwidth, sustained power, and visual error.

## Measurement Model

### CPU

Record wall-clock spans and thread CPU where practical for:

- event dispatch;
- document transaction;
- brush evaluation;
- scene query/culling;
- geometry/strip/tile preparation;
- allocation and cache management;
- command encoding;
- queue submission;
- result readback performed only for measurement.

Count allocations and bytes in hot paths. A timing without work counters cannot
explain why a case changed.

### GPU

Label and timestamp:

- uploads/copies;
- binning/culling;
- geometry expansion;
- coverage/density/stamp evaluation;
- filtering/simulation;
- layer compositing;
- final blit/presentation preparation.

Also record:

- dispatch/draw count;
- primitive/segment/strip count;
- touched tiles and pixels;
- temporary buffer high-water marks;
- bytes uploaded/copied;
- cache hits/misses;
- overdraw or candidate visits where cheaply available.

GPU timestamp availability and overhead are device-dependent. A run without GPU
timestamps remains useful for end-to-end behavior but cannot attribute pass
cost precisely.

### Latency vocabulary

Keep these measurements separate:

- **sample-to-app**: hardware/OS event delivery;
- **app processing**: event receipt to queue submission;
- **GPU completion**: submitted work to completion;
- **present interval**: application/display pacing;
- **input-to-photon**: physical input to visible display change.

The harness can measure the middle intervals. Full input-to-photon requires
external or platform-assisted measurement. Do not label sample-to-submit as
“input latency.”

### Percentiles and distributions

Store raw per-frame/per-event measurements and report at least:

- median;
- p90, p95, p99;
- maximum with surrounding trace context;
- missed target-frame deadlines;
- run-to-run variation.

Warm up explicitly, repeat runs, and randomize/interleave variant order where
possible to reduce drift. Google's
[benchmark guidance](https://google.github.io/benchmark/user_guide.html)
documents warmup, repetition, random interleaving, custom counters, memory
reporting, and manual GPU timing. The Sketchpad harness still needs custom
trace-aware behavior; a microbenchmark library alone is not enough.

## Result and Artifact Storage

Keep raw evidence, not only dashboards.

Each run produces:

- machine-readable context and configuration;
- raw frame/event/pass records;
- aggregate summary;
- output image or selected frames;
- oracle comparison and difference images;
- logs and validation errors;
- optional platform profiler/capture;
- source trace/snapshot identifiers;
- failure or early-termination reason.

JSON Lines is convenient for append-only raw records. SQLite is convenient for
querying results across devices, revisions, scenes, and variants. Either is
acceptable if the schema is versioned and raw artifacts remain addressable.

Do not store benchmark results inside the artwork document.

## Regression Policy

Use two distinct gates.

### Absolute product budgets

Examples include:

- active update meets the chosen 60/90/120 Hz budget;
- finalization never blocks interaction longer than the product limit;
- memory remains within the minimum-device budget;
- recovery provides useful pixels within the product limit;
- image error remains below the brush contract.

These budgets must be chosen from product and device requirements, not from the
speed of the current implementation.

### Relative regression alerts

Compare a revision with a statistically stable baseline on the same machine and
configuration. Alert only when both the relative change and practical absolute
change matter. A 20% regression from 10 µs is usually less urgent than a 5%
regression from 7 ms.

Automatic gates should initially detect and report. They should become blocking
only after run variance and machine control are understood.

## Hardware Matrix and Cadence

### Every relevant change

- deterministic semantic tests;
- image/error smoke suite;
- small CPU/kernel benchmarks where stable;
- one short offscreen renderer replay.

### Scheduled on Atlas

Run the canonical and sweep suites on:

- Intel UHD 630 as the meaningful integrated floor;
- GTX 1650 Mobile as the discrete comparison.

Record the selected adapter explicitly. Never treat whichever adapter the
runtime chose as an adequate result label.

### Continuous constrained checks on Apollo

Apollo is the lower-power x86/integrated-GPU target:

- Intel Pentium Silver N6000, 4 cores, 1.10 GHz reported base;
- Intel Jasper Lake UHD integrated graphics;
- 7.5 GiB usable shared system memory;
- Debian 13, Mesa 25.0.7, X11, and Vulkan through the Intel Mesa ICD;
- `rustc 1.97.1` as of 2026-07-24.

Use `scripts/apollo run ...`; it performs the same one-way synchronization and
Zellij capture as `scripts/atlas`, but defaults Cargo compilation to two jobs
so a cold wgpu build does not consume the machine's whole memory budget.
Compilation throttling does not apply to the program being benchmarked.

The initial setup passed all 42 tests and the offscreen GPU smoke test on the
hardware adapter. The smoke result contained 15,178 dark pixels and 312 cursor
pixels across eight resident tiles and four deliberately tiny cache pages.
This proves functional wgpu/Vulkan rendering; it is not a performance result.

Apollo should run short CPU kernels, offscreen GPU replays, and interactive
latency captures for every performance-sensitive change. Before recording a
number, close unrelated high-load applications and record load, memory, swap,
temperature, and frequency state. The first setup found that an otherwise
valid cold build could exhaust swap while unrelated desktop applications were
active, which is exactly the interference the laboratory metadata must expose.

Hardware-counter profiling is not ready on Apollo: the `perf` executable is
absent and `perf_event_paranoid` is 3. Enabling it requires an explicit
administrator-side setup; wall-clock benchmark and application tracing remain
available without that change.

### Periodic device laboratory

At minimum:

- one current Apple mobile-class GPU;
- one constrained/older Apple device if supported;
- at least two materially different Android GPU families and memory tiers;
- a CPU/software path.

Run sustained and platform-profiler scenarios here. Desktop results cannot
stand in for power, thermal, memory-pressure, or driver diversity.

## Anti-Patterns

Do not accept:

- one SVG scene as the benchmark;
- only average FPS;
- debug builds in comparisons;
- CPU submission time presented as GPU time;
- a forced GPU readback in one backend but not another;
- hidden cache warmup;
- different image quality or missing operations;
- random scenes without saved seeds;
- only empty-canvas drawing;
- only append-at-end drawing;
- only one zoom;
- only Atlas's NVIDIA adapter;
- a five-second mobile burst as sustained performance;
- hand-tuned device overrides without a portable baseline;
- benchmark summaries without raw results and context.

## First Laboratory Milestone

The first useful milestone does not need every platform tool. It needs:

1. one versioned document recipe with empty, sparse, and dense variants;
2. one recorded pressure stroke with a separate prediction track;
3. one pan/zoom trace;
4. cold-document and warm-viewport protocols;
5. a backend-neutral offscreen target and named color/compositing contract;
6. CPU spans, GPU pass timestamps when available, uploads, allocations, tiles,
   and candidate counts;
7. golden/reference images and difference output;
8. raw versioned result records;
9. forced Intel/NVIDIA runs on Atlas;
10. a report that puts latency, work, memory, and visual error together.

That milestone would already answer whether a custom `wgpu` path, Vello Hybrid,
or another backend is actually better for drawing a new stroke over a real
existing scene.

## Bottom Line

The benchmark framework is not secondary tooling. It is the experimental
foundation of the renderer architecture.

Its core rule is:

> Reproduce an artistic state transition, control scene/view/cache/quality
> state, measure every important boundary, preserve raw evidence, and compare
> distributions on the hardware that matters.

Without that, “blazing fast” will mean whichever demo looked good most
recently. With it, Sketchpad can safely own custom algorithms and specialize
per target without losing correctness or fooling itself.
