# Performance and Correctness Laboratory

Status: active specification and implementation ledger, 2026-07-24. This
defines the laboratory Sketchpad needs before selecting or optimizing renderer
backends and records the first implemented trace/replay slice.

The laboratory's immediate priority is the sparse-raster painter defined in
[first-usable-product.md](first-usable-product.md), especially active drawing
and color mixing over existing dense tiles. Renderer and multiscale comparisons
remain later suites.

The code being measured should follow the control-plane/pixel-plane split,
allocation policy, and instrumentation rules in
[performance-aware-code.md](performance-aware-code.md). The laboratory tests
those rules; it does not replace them.

The current measurements, exact-quality gate, unresolved attribution, and
ordered optimization experiments are consolidated in
[performance-proof-plan.md](performance-proof-plan.md). That plan governs the
next implementation milestone; this note remains the broader benchmark and
hardware protocol.

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

#### Natural brush-family baseline, 2026-07-28

`brush_bench` version 2 extends the same real transaction replay to the flat
nib, graphite pencil, palette knife, and bristle brush. The natural brushes
receive a smoothly rotating 0.72-magnitude tilt vector. Knife and bristle
painted cases exercise per-lane active-layer pickup. The timed region still
includes distance resampling, all footprint/lane/grain work, sparse raster
mutation, gesture snapshots, damage accumulation, final cap, and commit.

Apollo ran two warmups followed by five measured release runs per case:

```text
cargo run --release --locked --bin brush_bench -- --runs 5
```

| Brush/state | Tile | Median | p95 | Tile edits | Conservative pixels | Snapshot traffic |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Hard round/empty | 128 | 12.287 ms | 12.615 ms | 905 | 0.770 Mpix | 0 MiB |
| Hard round/painted | 128 | 7.586 ms | 7.694 ms | 905 | 0.770 Mpix | 4.375 MiB |
| Eraser/painted | 128 | 6.993 ms | 7.433 ms | 905 | 0.770 Mpix | 4.375 MiB |
| Flat/empty | 128 | 39.987 ms | 40.539 ms | 2,334 | 1.869 Mpix | 0 MiB |
| Pencil/empty | 128 | 39.889 ms | 40.670 ms | 4,154 | 1.952 Mpix | 0 MiB |
| Knife/empty | 128 | 57.824 ms | 59.363 ms | 2,678 | 1.977 Mpix | 0 MiB |
| Knife/painted | 128 | 51.580 ms | 51.829 ms | 2,678 | 1.977 Mpix | 4.586 MiB |
| Bristle/painted | 128 | 107.715 ms | 109.357 ms | 4,684 | 3.842 Mpix | 4.723 MiB |

Dividing the whole-stroke medians by 256 input samples gives a coarse average
of 0.156 ms for flat and pencil, 0.226 ms for the empty knife, and 0.421 ms for
the bristle brush. This is not an input-tail measurement: an event spanning a
long distance can emit more dabs than the average event. It does show that all
four CPU references are plausible for interactive evaluation on Apollo before
prematurely moving them to GPU compute.

Every case had zero checksum spread across measured runs. Equivalent cases
also produced identical checksums at tile sizes 128 and 256. Content-bound
recovery scanned zero pixels for every additive natural brush. The natural
brushes are slower primarily because their tighter spacing causes 2.6–5.2×
more tile edits and 2.4–5.0× more conservative pixel visits than hard round;
the pencil also evaluates two deterministic hashes per covered pixel, while
knife and bristle retain and sample persistent lane state.

At 256-pixel tiles, natural-brush medians were 48.315 ms flat, 47.649 ms
pencil, 64.441/50.707 ms knife empty/painted, and 106.305 ms bristle. The
current 128-pixel application tile remains materially better for flat, pencil,
and empty knife, approximately neutral for painted knife and bristle, and
retains finer sparse allocation. There is no evidence here to change the
application default.

The next performance work should measure per-event tail latency during a live
Wacom stroke and split lane sampling, pixel coverage, raster transaction, and
presentation costs. The bristle brush is the first optimization candidate.
Reducing spacing or strand count solely to improve a benchmark would change
the authored mark and is not allowed without a visual corpus comparison.

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

#### Implemented recorded-trace slice

The first backend-neutral workload begins with a committed physical trace:

- `traces/canonical-wacom-v1.json`;
- Wacom Intuos Pro S Pen stylus on Atlas/XInput2;
- 68 down/move/up samples;
- 385.124 ms application-arrival duration and 332 ms X11 source duration;
- normalized pressure range 0.0–0.6873;
- normalized tilt ranges x=0.5556–0.7143 and y=0.1111–0.4444;
- content hash `46da823fd749864d`.

Recording mode uses a blank transient document, never reads or overwrites the
recovery checkpoint, writes atomically, exits on the first pen-up, and fetches
only the resulting artifact:

```text
scripts/atlas record traces/canonical-wacom-v1.json
```

The CPU runner normalizes the recorded geometry and deterministically places
it in a 2048×2048 reference canvas. For each selected scene it replays one
canonical stroke unpaced and at 1×, 2×, and 4× arrival cadence. Defaults are
three repetitions and these initial states:

| Scene | Deterministic seed strokes |
| --- | ---: |
| empty | 0 |
| sparse | 12 |
| dense | 128 |
| stress | 1,000 |

It then runs 1,000 independently transformed target strokes per scene by
default. Every target is undone immediately so the initial scene remains
constant rather than becoming an accidental progressive workload. A full
scene checksum is verified every 100 corpus strokes and after the final
stroke.

Run the release suite and fetch its result and machine context:

```text
scripts/atlas benchmark cpu
scripts/apollo benchmark cpu
```

The GPU runner uses the same trace, brush, scene recipes, timing modes, seed,
and CPU-canonical raster result. It creates a named 1280×720 offscreen target
and requires explicit adapter selection in normal use:

```text
scripts/atlas benchmark gpu intel
scripts/atlas benchmark gpu nvidia
scripts/apollo benchmark gpu intel
```

One GPU frame is submitted per input sample. Each versioned JSON Lines row
keeps the raw per-sample arrays and distributions for:

- brush/document processing;
- resident damage synchronization and upload preparation;
- visible-scene preparation;
- command encoding and queue submission;
- total application receipt-to-submit work;
- scheduled-deadline lateness;
- hardware-timestamped render-pass execution.

It also retains exact upload counts/bytes, dabs, damaged tiles, residency,
visible instances, allocated pages/capacity, deferrals, evictions, output
checksum, adapter IDs, driver information, trace hash, scene seed, revision,
and host. The CPU runner adds raster allocation, before-image, snapshot-byte,
tile-lookup, conservative-pixel, and content-bound-scan counters. Optional PPM
references and mismatch images are available from the CPU runner.

The GPU timestamp pair brackets the render pass only. `queue.write_texture`
may schedule upload work outside that pair, so upload counts and CPU
synchronization time must not be interpreted as upload GPU execution.
Offscreen results also exclude compositor/presentation and physical
input-to-photon latency.

The convenience command records a sibling `.context.txt` containing UTC time,
kernel/host, load, CPU topology, frequency policy, memory/swap, sensors,
Vulkan summary, toolchain, and NVIDIA state where present. Both files live
beneath ignored `.artifacts/results`; benchmark evidence is not silently added
to the source tree.

Smoke validation has passed with hardware timestamps on Atlas's Intel UHD 630,
Atlas's NVIDIA GTX 1650 Max-Q, and Apollo's Intel Jasper Lake UHD. Exact
same-machine comparisons require repeated controlled runs; these one-run
checks validate the measurement paths and are not durable performance
baselines.

The implemented slice deliberately does not yet include a view trace,
prediction/correction track, GPU upload-pass timestamps, presentation,
image readback in the timed GPU runner, random interleaving of variants, or
mobile thermal protocols. The existing `gpu_smoke` readback test remains the
GPU-presentation correctness oracle while those pieces are added.

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

This live-window instrumentation is useful for detecting CPU and traffic
amplification, but it still has no GPU timestamps, compositor/presentation
signal, or photon measurement. The offscreen recorded-trace runner now has
render-pass timestamps; those should not be conflated with the missing live
boundaries.

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

Hardware-counter profiling is ready on Atlas. CPU `perf` events work with
`perf_event_paranoid=2`; `intel_gpu_top` has `CAP_PERFMON`; Vulkan diagnostics,
CPU-frequency inspection, sysstat, and temperature sensors are installed. The
discrete path also has `nvidia-smi` and NVIDIA Nsight Systems (`nsys`). The
portable trace harness should identify a hot case before these tools are used
to explain it.

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

The initial setup passed all 42 then-current tests and the offscreen GPU smoke
test on the hardware adapter. The smoke result contained 15,178 dark pixels
and 312 cursor pixels across eight resident tiles and four deliberately tiny
cache pages. This proves functional wgpu/Vulkan rendering; it is not a
performance result.

Apollo should run short CPU kernels, offscreen GPU replays, and interactive
latency captures for every performance-sensitive change. Before recording a
number, close unrelated high-load applications and record load, memory, swap,
temperature, and frequency state. The first setup found that an otherwise
valid cold build could exhaust swap while unrelated desktop applications were
active, which is exactly the interference the laboratory metadata must expose.

The `scripts/<host> benchmark ...` convenience path now performs a preflight
before every recorded run. It requires AC power, the performance power
profile, at least 3 GiB available memory, and at least 90% CPU idle over a
one-second sample. It rejects known development/media applications, RustDesk,
and running Docker containers rather than preserving a misleading result.
Context artifacts also record the power profile, AC state, Intel GPU
frequency range/current state, top user processes, and relevant service state.

Apollo's first controlled cleanup demonstrated why this is required.
Minecraft, Firefox, Motrix, RustDesk, Flaresolverr, Vivado helpers, sync and
activity monitors collectively changed CPU corpus p95 by roughly 25–29%.
Low-power mode produced a separate multi-fold regression. Conversely,
Minecraft could keep the integrated GPU out of its low-frequency state and
make isolated GPU pass timestamps look faster while worsening CPU scheduling
and memory pressure. Cold, warm, and sustained GPU protocols must therefore
remain distinct rather than relying on whichever DVFS state preceded a run.

#### Controlled Apollo baseline, 2026-07-24

Commit `762a9f4` was measured after stopping all identified applications,
RustDesk, and Docker containers while retaining the X11/GNOME/Wacom session
and SSH control path. Apollo was on AC power in the performance profile with
about 6.4 GiB available memory, no active swap traffic, and 96–100% idle CPU
during the settling sample.

The 1,000-stroke CPU corpus produced:

| Scene | Full-stroke median | Full-stroke p95 |
| --- | ---: | ---: |
| empty | 1.113 ms | 2.740 ms |
| sparse | 1.307 ms | 2.605 ms |
| dense | 1.321 ms | 2.611 ms |
| stress | 1.432 ms | 2.644 ms |

These p95 values were 24.6–28.5% lower than the first
Minecraft/background-application-contaminated run.

Three 1× GPU repetitions produced these per-sample p95 medians:

| Scene | Application to submit | GPU render pass |
| --- | ---: | ---: |
| empty | 0.950 ms | 1.556 ms |
| sparse | 0.866 ms | 2.801 ms |
| dense | 0.951 ms | 3.866 ms |
| stress | 1.000 ms | 4.430 ms |

The stress GPU result was tightly grouped at 4.414–4.485 ms. Apollo's Intel
GPU idles around 200–350 MHz and can boost to 850 MHz. Faster-paced and
unpaced playback lowered individual pass times by sustaining GPU frequency;
continuous unrelated GPU work can therefore make a pass appear faster rather
than simply adding contention. The portable baseline must retain natural DVFS,
while separate cold, warmed, fixed-frequency diagnostic, and sustained suites
explain that behavior.

The strict 250 µs scheduling-lateness count sometimes increased on the clean,
otherwise idle system even as all processing spans improved. This is consistent
with timer wake-up and CPU idle-state behavior, so lateness must be reported
separately from computation time and eventually use a platform-appropriate
absolute-deadline protocol.

#### First region-dominated CPU transaction baseline, 2026-07-25

Committed revision `d9fce8b` added `cpu_profile_replay`. A clean Apollo dense
run prepared 128 seed strokes, proved all 68 intermediate canonical states and
damage under 1/2/4/8/all-sample drains, warmed 128 transactions, and then ran
10,000 precomputed paint-plus-undo transactions. Checksums and serialization
were outside the hot interval.

The aggregate hot interval was 13.506209 seconds: 1.350621 ms per transaction,
740.4 transactions per second. This is an aggregate profiling boundary, not a
per-stroke latency distribution. It processed 1,457,690 dabs and restored the
prepared checksum `480a63807d651ccf` exactly.

The interval recorded:

- 2,051,164 tile lookups/bulk tile edits;
- 128,257 whole-tile before-images;
- 33,047,445,504 snapshot bytes (30.778 GiB total, 3.152 MiB per stroke);
- 1,050,131,532 conservatively visited pixels;
- 2,191 allocations for tiles absent from the prepared scene.

This is the first clean evidence for the new measurement boundary. It does not
separate paint from undo, and hardware counters still require attachment after
the emitted `profile-ready` marker. Its immediate role is to quantify the
whole-tile undo amplification before selecting a shadow-block representation.

#### Undo shadow-block traffic matrix, 2026-07-25

Committed revision `5abeaf3` measured 64 identical deterministic target-stroke
recipes against sparse, dense, and stress scenes. Each run passed the standard
Apollo preflight at 94–97% CPU idle, retained the exact intermediate oracle,
and restored its initial checksum. The measurement compares final before/after
pixels after undo, which is equivalent to first-write coverage for the current
monotonic hard-round paint kernel.

The modeled candidate contains pixel payload plus a fixed per-existing-tile
block bitmap. It does not yet include vector capacity/allocator metadata.
Newly allocated tiles require an allocation marker but no before-pixel payload,
matching the current whole-tile byte counter.

| Scene | Current whole tile | 8×8 | Reduction | 16×16 | Reduction | 32×32 | Reduction |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| sparse | 101.25 MiB | 20.26 MiB | 5.00× | 26.12 MiB | 3.88× | 37.55 MiB | 2.70× |
| dense | 201.75 MiB | 40.18 MiB | 5.02× | 51.91 MiB | 3.89× | 75.38 MiB | 2.68× |
| stress | 207.50 MiB | 41.77 MiB | 4.97× | 54.03 MiB | 3.84× | 78.14 MiB | 2.66× |

The dense run found 41,118 existing-tile 8×8 blocks, 13,288 16×16
blocks, and 4,824 32×32 blocks. The stress run found 42,747, 13,829, and
5,001 respectively. Thus 8×8 saves a further 22–23% of modeled bytes versus
16×16 but creates roughly 3.1 times as many block records. Bitmap bytes were
negligible; copy grouping, lookup cost, allocation layout, undo/redo speed, and
retained heap capacity are not.

Decision: advance both 8×8 and 16×16 to exact implementation tests. Drop 32×32
from the first implementation comparison because it is consistently
byte-dominated by 16×16. Do not select 8×8 solely from this table; first measure
the real capture/restore representation and add large-brush and eraser cases.

Revision `158798b` added explicit mode/diameter controls and completed those
rows on the stress scene:

| Brush | Current whole tile | 8×8 | Reduction | 16×16 | Reduction | 32×32 | Reduction |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 48 px erase | 207.50 MiB | 41.40 MiB | 5.01× | 53.70 MiB | 3.86× | 77.88 MiB | 2.66× |
| 192 px paint | 287.00 MiB | 123.03 MiB | 2.33× | 133.78 MiB | 2.15× | 155.11 MiB | 1.85× |

The eraser agrees with ordinary 48 px paint. The large brush narrows the byte
gap: 8×8 saves only 8.0% versus 16×16 while producing 125,944 payload blocks
instead of 34,246, or 3.68 times as many records. A 16×16 canonical block also
has a convenient 4 KiB pixel payload, although page-sized alignment is not
itself evidence of speed.

Implementation order: prototype 16×16 first, retain 8×8 as the direct
challenger, and continue to exclude 32×32. The prototype must report actual
allocated bytes and capture/undo/redo time; modeled traffic alone does not
select the final representation.

#### Exact 16×16 implementation comparison, 2026-07-25

Revision `bb5f7c8` implemented pure 16×16 storage behind an explicit profiler
switch while retaining whole-tile storage as the default/control. It captures
every conservative block once, keeps per-tile payload contiguous, swaps row
slices for undo/redo, and separately times paint/capture and undo. The
recorded-trace oracle, cancellation, new-tile, reclaimed-tile, undo, and redo
tests are exact.

Two reversed-order 10,000-stroke dense pairs were exceptionally stable:

| Dense 48 px paint | Whole tile | Pure 16×16 |
| --- | ---: | ---: |
| paint/capture per stroke | 1.3473 ms | 1.1449 ms |
| explicit undo per stroke | 0.0072 ms | 0.2057 ms |
| immediate paint+undo per stroke | 1.3547 ms | 1.3509 ms |
| captured before-pixels per stroke | 3.152 MiB | 0.917 MiB |

Thus pure blocks improve the active paint/capture boundary by 15.0% and retain
3.44 times fewer before-pixels. Undo is about 28.5 times slower because blocks
must preserve the after-state for redo, but remains 0.206 ms per operation on
Apollo. The synthetic loop's immediate-undo total is effectively tied; normal
drawing does not undo every committed stroke.

The edge cases were:

| Workload | Whole paint | Blocks paint | Change | Whole undo | Blocks undo | Pixel reduction |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| empty 48 px paint | 1.4538 ms | 1.4450 ms | −0.6% | 0.0321 ms | 0.2944 ms | both zero |
| stress 48 px erase | 1.1509 ms | 1.1182 ms | −2.8% | 0.0061 ms | 0.2456 ms | 3.41× |
| stress 192 px paint | 3.6495 ms | 3.6760 ms | +0.7% | 0.0100 ms | 0.6096 ms | 1.90× |

Those rows use a one-paint/one-undo loop. That remains useful as an adversarial
transaction and allocator control, but it is not representative of continuous
drawing. It also made the cost of new-tile ownership depend on the artificial
immediate rollback.

The first hybrid experiment was rejected. It used the whole-state `None`
marker for new tiles and chose a whole snapshot when the first edit to an
existing tile covered at least 32 blocks. Even a 192 px round brush normally
enters a tile through a small leading intersection, so the threshold did not
fire: snapshot counts and bytes stayed identical to pure blocks while dense
and large-brush timings regressed. The implementation was removed.

Profiler result format version 2 now paints a burst before restoring the exact
prepared raster. The default burst is 64 varied strokes; batch 1 remains
available through `--transaction-batch-strokes 1`. It separately reports
active paint/capture, explicit undo, redo-history destruction, and residual
harness overhead.

Clean Apollo batch-64 results at revision `ed948f3`:

| Workload | Storage | Paint/stroke | Undo/stroke | Cleanup/stroke | Before bytes |
| --- | --- | ---: | ---: | ---: | ---: |
| dense 48 px paint, 2,000 strokes | whole | 1.3824 ms | 0.0017 ms | 0.0071 ms | 6,321.5 MiB |
| dense 48 px paint, 2,000 strokes | blocks16 | 1.1698 ms | 0.2711 ms | 0.0079 ms | 1,836.5 MiB |
| stress 192 px paint, 1,000 strokes | whole | 3.6932 ms | 0.0022 ms | 0.0065 ms | 4,490.0 MiB |
| stress 192 px paint, 1,000 strokes | blocks16 | 3.8112 ms | 0.6439 ms | 0.0160 ms | 2,368.1 MiB |

For the ordinary 48 px case, block snapshots make the user-facing drawing
interval 15.4% faster and retain 3.44× fewer before-pixels. Explicit undo is
slower but remains 0.271 ms per stroke on Apollo. At 192 px, blocks make active
drawing 3.2% slower and retain only 1.90× fewer before-pixels. The evidence
therefore rejects a global mode.

A stress-scene 128 px crossover row measured 2.6742 ms/stroke for whole
capture and 2.6459 ms/stroke for blocks: blocks remain 1.1% faster while
reducing before-pixels from 3,955.25 MiB to 1,767.53 MiB, or 2.24×. The first
safe boundary is therefore the tile width rather than a fitted magic number.

Revision `1678118` implements `adaptive16`. The hard-round brush selects
blocks once at gesture start when its diameter is at most the 128 px tile
width, and whole storage above it. That is information the brush actually
owns; it avoids trying to infer a completed stroke from its first tile
intersection. General raster gestures without a brush hint conservatively use
whole tiles. Concrete history entries self-describe their representation, so
exact mixed-policy undo does not require a document-format change.

Clean adaptive validation reproduces the selected controls:

| Workload | Adaptive choice | Paint/stroke | Undo/stroke | Before bytes | Final checksum |
| --- | --- | ---: | ---: | ---: | --- |
| dense 48 px paint, 2,000 strokes | blocks16 | 1.1688 ms | 0.2714 ms | 1,836.5 MiB | `480a63807d651ccf` |
| stress 192 px paint, 1,000 strokes | whole | 3.6842 ms | 0.0022 ms | 4,490.0 MiB | `50ed3172c1cd83a5` |

Hardware-counter profiling is ready on Apollo. `linux-perf` can capture
per-process userspace cycles, instructions, branches, and cache events with
`perf_event_paranoid=2`. `intel_gpu_top` has `CAP_PERFMON` and has been
validated against the Jasper Lake PMU, including frequency, Render/3D
utilization, residency, and per-process activity. Vulkan diagnostics, CPU
frequency inspection, sysstat, and temperature sensors are also installed.

#### Frame-coalesced upload baseline, 2026-07-25

Revision `9417f45` moves live dirty upload from input-event handling to frame
preparation. Pending resident-tile regions are unioned by tile; removals clear
pending work; a newly visible tile still receives a full upload of its current
state. The hardware smoke test queued duplicate resident damage and verified
that it coalesced before upload, left no pending work, and preserved the exact
GPU readback.

The current offscreen replay remains the one-submit-per-input-sample control,
so its dense trace has no opportunity to coalesce:

| Timing | Regions | Coalesced | Uploads | Logical | Source span | Packed+padded candidate | `write_texture` CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| unpaced | 128 | 0 | 128 | 3.607 MiB | 9.303 MiB | 4.183 MiB | 28.29 ms |
| scheduled 1× | 128 | 0 | 128 | 3.607 MiB | 9.303 MiB | 4.183 MiB | 5.74 ms |
| scheduled 2× | 128 | 0 | 128 | 3.607 MiB | 9.303 MiB | 4.183 MiB | 5.07 ms |
| scheduled 4× | 128 | 0 | 128 | 3.607 MiB | 9.303 MiB | 4.183 MiB | 6.42 ms |

All four rows end at checksum `7d45a406ea4f3667`. “Source span” is the
address range implied by the current full-tile row stride, not measured
internal wgpu traffic. “Packed+padded” is the byte count an explicit compact
staging layout would use with 256-byte row alignment. API CPU time can include
allocation or backpressure and is not GPU-copy execution.

The next replay version must process every ready document sample but prepare
and submit at most one frame per display opportunity. That will quantify
actual coalescing before comparing `write_texture` with a reusable staging
ring and explicitly timestamped buffer-to-texture copies.

Revision `e428e8a` adds that display-paced replay. The result schema is version
4 because deliberate sample wait until late-latched frame preparation is now
separate from actual frame-deadline miss.

Dense Apollo results:

| Rate/display | Frames | Regions coalesced | Uploads | Logical | Padded candidate | `write_texture` CPU | Frame-late p95 | Sample wait p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1×, per sample | 68 | 0 | 128 | 3.607 MiB | 4.183 MiB | 11.57 ms | 15.19 ms | 15.19 ms |
| 1×, 60 Hz | 22 | 53 | 75 | 4.065 MiB | 4.452 MiB | 2.99 ms | 0.45 ms | 15.92 ms |
| 1×, 120 Hz | 27 | 47 | 81 | 3.946 MiB | 4.356 MiB | 3.21 ms | 0.43 ms | 8.15 ms |
| 2×, 60 Hz | 12 | 66 | 62 | 4.216 MiB | 4.563 MiB | 2.16 ms | 0.43 ms | 16.15 ms |
| 2×, 120 Hz | 22 | 53 | 75 | 4.065 MiB | 4.452 MiB | 3.00 ms | 0.42 ms | 8.00 ms |
| 4×, 60 Hz | 6 | 76 | 52 | 4.361 MiB | 4.627 MiB | 1.63 ms | 0.42 ms | 16.48 ms |
| 4×, 120 Hz | 12 | 66 | 62 | 4.216 MiB | 4.563 MiB | 2.19 ms | 0.41 ms | 8.30 ms |

Every row preserves all 68 samples and ends at checksum
`7d45a406ea4f3667`. At 60 Hz, coalescing reduces upload calls by 41%–59% and
`write_texture` CPU by 74%–86% relative to this run's per-sample 1× control.
The price is 13%–21% more logical dirty bytes because a union rectangle can
enclose unchanged pixels. Therefore the next coalescer candidate is a small
per-tile rectangle set with a merge cost based on added padded bytes versus
measured call overhead. A universal one-rectangle rule is not justified.

The GPU render-pass p95 is not used to select the coalescer: uploads occur
outside the timestamp pair, and DVFS/queue ordering can change the following
pass. Explicit staging-copy timestamps are still required.

#### Cost-aware dirty rectangle set, 2026-07-25

Revision `e8f56c4` adds a fixed-capacity alternative to single union. A tile
stores at most four pending rectangles inline. Pair selection minimizes:

```text
padded_bytes(union) - padded_bytes(a) - padded_bytes(b)
```

where rows use WebGPU's 256-byte copy alignment and the current
`Rgba32Float` 16-byte pixels. A pair merges when the added bytes do not exceed
the configured call-equivalent cost. A fifth region forces only the cheapest
merge. This bounds work and storage without allocating a `Vec` per tile.

The replay exposes `union` and `rect4`, records the threshold and forced
merges, and preserves the final checksum for every tested policy. Clean
three-run Apollo medians at 60 Hz were:

| Threshold | 1× uploads / padded / API | 2× uploads / padded / API | 4× uploads / padded / API |
| --- | ---: | ---: | ---: |
| 16 KiB | 83 / 4.200 MiB / 3.019 ms | 74 / 4.204 MiB / 2.216 ms | 65 / 4.230 MiB / 1.720 ms |
| 64 KiB | 75 / 4.452 MiB / 2.845 ms | 62 / 4.563 MiB / 2.160 ms | 52 / 4.627 MiB / 1.615 ms |

Zero KiB produced 102, 96, and 91 calls at 1×, 2×, and 4×. The 32 KiB
candidate was a genuine intermediate but did not beat 64 KiB for
`write_texture` API CPU or preparation latency. Therefore 64 KiB was selected
for the `write_texture` control. On this contiguous trace it happens to
converge to the single-union call and byte totals, but unlike unconditional
union it can retain disconnected damage when the intervening padded bytes
exceed the threshold.

This value is evidence for Apollo's Intel UHD Graphics path, not a portable
hardware constant. The threshold represents the current `write_texture` call
tradeoff. It is not the threshold selected for the staged path below.

#### Reusable explicit upload staging, 2026-07-25

Revisions `61b2cd4` and `4335258` add a selectable three-slot staging ring.
Dirty rectangles are packed directly into persistently owned mapped buffers
with 256-byte-aligned compact rows. The buffers are unmapped, copied into tile
textures by the frame command encoder, and requested for remapping immediately
after submission. Reuse polls without blocking first. An 8 MiB per-frame cap
falls back to `write_texture`, so a cold 210-tile residency fill cannot create
a 64 MiB ring slot; the three-slot steady upper bound is 24 MiB.

Replay format version 6 reports CPU pack, copy-command encoding, actual ring
wait, allocations, retained capacity, oversized fallbacks, and an
encoder-timestamped GPU copy batch. The GPU copy timestamp is available on
Apollo Vulkan. It cannot be compared directly with the near-zero timestamp
gap in the `write_texture` control: `Queue::write_texture` transfer execution
occurs outside that control's command encoder.

With asynchronous remapping, all 60 Hz threshold runs recorded zero ring
waits and exact checksum `7d45a406ea4f3667`. Three-run medians:

| Threshold | Rate | Uploads | Padded | Pack + encode CPU | App-to-submit p95 | Explicit GPU copy p95 |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 KiB | 1× | 102 | 4.047 MiB | 0.901 ms | 1.201 ms | 0.227 ms |
| 0 KiB | 2× | 96 | 4.026 MiB | 0.864 ms | 1.597 ms | 0.318 ms |
| 0 KiB | 4× | 91 | 4.018 MiB | 0.803 ms | 2.160 ms | 0.584 ms |
| 32 KiB | 1× | 78 | 4.318 MiB | 0.977 ms | 1.349 ms | 0.197 ms |
| 32 KiB | 2× | 67 | 4.358 MiB | 0.873 ms | 1.771 ms | 0.285 ms |
| 32 KiB | 4× | 58 | 4.384 MiB | 0.950 ms | 2.487 ms | 0.470 ms |
| 64 KiB | 1× | 75 | 4.452 MiB | 0.981 ms | 1.365 ms | 0.200 ms |
| 64 KiB | 2× | 62 | 4.563 MiB | 0.894 ms | 1.833 ms | 0.283 ms |
| 64 KiB | 4× | 52 | 4.627 MiB | 0.926 ms | 2.496 ms | 0.464 ms |

The extra copy commands at 0 KiB cost at most 0.12 ms GPU p95 versus 64 KiB
in this matrix, while reducing transfer bytes and winning application CPU p95
at every rate. Against the separately measured 64 KiB `write_texture`
control, staged 0 KiB reduces measured upload-side CPU totals from
1.837/1.495/1.304 ms to 0.901/0.864/0.803 ms and application p95 from
1.391/1.875/2.454 ms to 1.201/1.597/2.160 ms. Revision `1f85a50` therefore
selects staged 0 KiB as the current Apollo-derived application default.

This result does not establish a universal threshold. The 8 MiB fallback,
24 MiB retained upper bound, mobile mapping behavior, format choice, and
different brush geometry all remain qualification dimensions.

#### Persistent visibility and instance state, 2026-07-25

Revision `609a92e` separates two invalidation domains:

- visibility: exact view bounds plus sparse layer allocation generation;
- per-page instances: visibility generation plus GPU residency generation.

Existing-tile pixel edits do not advance allocation generation. Allocation,
reclamation, undo/redo structure changes, camera movement, eviction, and
explicit residency clearing invalidate the appropriate cache. The hardware
smoke test submits an unchanged second frame and requires a visibility hit, an
instance hit, and zero additional instance-buffer bytes.

Replay format version 7 adds the `cached` and exact `rebuild` controls plus
rebuild, hit, scan, sort, byte, and cached-count fields. Dense three-run Apollo
60 Hz medians:

| Rate | Mode | Scene prepare p95 | App-to-submit p95 | Visibility work | Instance bytes |
| ---: | --- | ---: | ---: | --- | ---: |
| 1× | rebuild | 0.406 ms | 1.329 ms | 22 rebuilds / 4,620 scans | 147,840 |
| 1× | cached | 0.268 ms | 1.150 ms | 22 hits / zero scans | 0 |
| 2× | rebuild | 0.487 ms | 1.844 ms | 12 rebuilds / 2,520 scans | 80,640 |
| 2× | cached | 0.369 ms | 1.534 ms | 12 hits / zero scans | 0 |
| 4× | rebuild | 0.546 ms | 2.582 ms | 6 rebuilds / 1,260 scans | 40,320 |
| 4× | cached | 0.455 ms | 2.036 ms | 6 hits / zero scans | 0 |

GPU render-pass medians remain statistically unchanged, confirming this is a
CPU preparation optimization. In a stable 253-tile stress scene, cached scene
preparation is 0.449 ms versus 0.610 ms for rebuild. In the sparse target, five
of six frames allocate tiles, producing five legitimate rebuilds and one hit;
that is the intended invalidation behavior.

Revision `230bf85` adds centered view zoom isolation. Stable stress results at
4× input and 60 Hz:

| View zoom | Cached visible tiles | App-to-submit p95 | GPU render p95 |
| ---: | ---: | ---: | ---: |
| 1× | 253 | 2.426 ms | 3.911 ms |
| 2× | 128 | 2.289 ms | 3.908 ms |
| 4× | 32 | 2.154 ms | 2.484 ms |

The first halving leaves GPU duration unchanged, ruling out visible-instance
count as the dominant cost across that range. Zoom 4 also changes source
texture locality and texel reuse, so its faster pass is evidence—not proof—of
`Rgba32Float` sampling bandwidth/cache pressure.

#### Full-precision display-cache control, 2026-07-25

Replay format version 8 adds `--presentation direct|cache-rgba32`. The cache
mode owns one canvas-sized `Rgba32Float` texture, copies only queued dirty
regions into their canvas-space positions, and presents it with one fullscreen
draw. Direct tile-array rendering remains the default and same-revision
control. Tile reclamation conservatively discards the cache and rebuilds
visible content, so deletion cannot leave stale pixels.

An initial `Rgba16Float` probe was rejected and removed. Although it reduced
display bandwidth, Apollo readback found 92 one-value differences among
1,048,576 output channels. That is small but violates the project rule that a
performance experiment must not silently change quality. It also introduced
conversion work that confounded the presentation-layout comparison.

The retained `Rgba32Float` cache has no conversion path or half-float
dependency. Apollo's hardware smoke test renders the same raster, paper, and
cursor through direct tiles and the cache, reads both outputs back, and
requires exact byte equality. The current result is zero differing channels,
zero total absolute error, and zero maximum error.

The full-precision timing matrix remains an experiment rather than an
application-default decision. It must determine whether contiguous texture
locality and one draw offset the additional canvas-sized allocation without
credit from reduced precision. Follow-up work should separately measure cold
cache creation, steady dirty updates, static dense presentation, erasure
rebuilds, canvas-size limits, and memory pressure.

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

## Current Performance-Proof Milestone

Before treating any implementation change as an optimization:

1. compare exact canonical checksums at intermediate replay boundaries;
2. prove input-drain batching does not change document semantics;
3. move scene construction, large checksums, and reporting outside a
   region-dominated CPU profiling interval;
4. measure candidate undo shadow-block traffic before changing undo storage;
5. compare transfer mechanisms with actual padded bytes and copy execution
   distinguished from render-pass time;
6. introduce display-paced replay that processes every sample but submits at
   most once per display opportunity;
7. isolate visible instance count from painted screen coverage and DVFS.

The acceptance gates and execution order live in
[performance-proof-plan.md](performance-proof-plan.md).

## Broader Laboratory Milestone

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
