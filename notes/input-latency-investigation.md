# Physical Pen Latency Investigation

Status: live internal latency probe implemented; Immediate/1 presentation and
non-blocking recovery physically accepted on Apollo, 2026-07-27.

## Observed Failure

On Apollo's integrated Wacom pen display, the GPU-rendered brush footprint
trails the physical pen by a visibly large distance. The delay is present
during hover and remains approximately the same during contact, although
drawing may be slightly worse.

That observation narrows the first investigation:

- hover does not execute the brush raster or document-composite path, so brush
  work cannot explain the large delay shared by hover and contact;
- contact-only paint, damage, and upload work may explain the smaller
  difference between the two states;
- a separation that grows with pen speed and closes when the pen stops is
  temporal latency or backlog;
- a nearly constant separation at every speed is more consistent with tablet
  calibration or coordinate mapping.

The common hover/contact path is:

```text
Wacom hardware/driver
    -> X server and XInput2
    -> Sketchpad XInput backend thread
    -> winit EventLoopProxy queue
    -> Sketchpad tablet handler
    -> redraw scheduling and CPU frame construction
    -> GPU queue submission
    -> present queue / compositor / scanout / panel
```

Sketchpad draws its own brush cursor in the same GPU pass as the canvas. It is
therefore subject to frame pacing and presentation latency; it is not an OS
hardware cursor.

## Probe Boundaries

Once per active second, the live application reports hover and contact
separately:

| Field | Start -> end | What it proves |
| --- | --- | --- |
| `source_excess_us` | X event timestamp progression -> backend receipt progression | Variable delay before the backend relative to the best delivery observed in this process |
| `backend_queue_us` | backend thread receipt -> winit tablet handler entry | Time spent crossing `EventLoopProxy` and waiting for the event loop |
| `latest_to_submit_us` | newest handled tablet sample used by a frame -> CPU call to present that frame | Redraw scheduling plus CPU frame construction for the latest cursor state |
| `samples_per_submit` | tablet samples accumulated -> one submitted frame | Whether multiple source samples are being drained into one display opportunity |
| existing `input_us` | handler entry -> completed semantic input work | Cursor state update alone for hover; cursor plus painting/compositing for contact |
| existing `frame_us` | redraw handler entry -> CPU present call | Surface acquisition, upload preparation/encoding, render encoding, and submission |

`frame_stages` further divides `frame_us` into surface acquisition, visible
scene/uniform preparation, command encoding, and queue submission/present-call
CPU time.

`Down`, `Move`, and `Up` are grouped as contact. `Hover` remains separate.
Each series retains up to 2,048 recent samples and reports count, mean, p95,
and maximum. Collection uses fixed storage and monotonic timestamps; it does
not allocate per tablet sample.

The source timestamp has one-millisecond resolution and an unrelated clock
origin. `source_excess_us` therefore is **not absolute hardware-to-process
latency**. The probe aligns source and receipt clocks and subtracts the minimum
offset observed so far. It detects changing delay and backlog, not a stable
delay that was already present at the first sample.

`latest_to_submit_us` ends at the CPU presentation call. Neither it nor
`frame_us` measures GPU completion, compositor scheduling, display scanout, or
photons. A low value does not prove low visible latency.

Startup also records:

- requested and supported wgpu present modes;
- requested maximum queued-frame latency;
- the monitor refresh rate reported by winit;
- adapter name and backend.

The configured `AutoVsync` value is a request, not proof of the backend's
selected swap behavior.

## Running on Apollo

From the local worktree:

```text
scripts/apollo latency
```

The command syncs the worktree, builds the release application on Apollo, and
opens it on Apollo's X display. The Apollo wrapper supplies `DISPLAY=:0`
explicitly because that variable is absent from the Zellij SSH shell. Close
the Sketchpad window to return the captured logs to the caller. The same
output remains visible in the `apollo` Zellij tab while the application is
running.

Presentation experiments pass application options through the same launcher:

```text
scripts/apollo latency --present-mode mailbox --max-frame-latency 1
scripts/apollo latency --present-mode immediate --max-frame-latency 1
```

Accepted modes are `auto-vsync`, `auto-no-vsync`, `fifo`, `fifo-relaxed`,
`mailbox`, and `immediate`; requested frame latency is restricted to 1–3. An
unsupported explicit mode falls back to `AutoVsync` with a warning rather than
configuring an invalid surface.

For a useful first capture:

1. leave the pen completely still for a few seconds;
2. hover slowly for at least five seconds;
3. hover quickly back and forth for at least five seconds;
4. draw slowly for at least five seconds;
5. draw quickly for at least five seconds;
6. stop the pen after each fast motion and observe whether the cursor catches
   up or remains spatially offset;
7. close the window and preserve the log with the finding.

Do not compare a debug build, a build still compiling, or a machine under
unrelated load against a controlled baseline.

## Interpreting the First Capture

| Result | Leading explanation | Next controlled experiment |
| --- | --- | --- |
| `source_excess_us` rises during fast hover | X/driver delivery arrives in bursts or accumulates before Sketchpad's backend | inspect packet cadence and XInput configuration |
| `backend_queue_us` rises | one-proxy-event-per-packet or event-loop work is accumulating | bounded input queue with one wake and latest-cursor late latching |
| hover `latest_to_submit_us` is high while queue time is low | redraw scheduling, surface acquisition, or CPU frame construction delays the cursor | instrument acquire/prepare/encode/submit separately and test pacing |
| contact is much worse than hover only in `input_us` or `frame_us` | synchronous paint/composite/upload adds contact-only delay | batch semantic samples while coalescing display work |
| all internal segments are low but the cursor visibly trails | stable pre-backend delay or post-submit presentation dominates | high-speed-camera test, compositor/present inspection, and an OS-cursor control |
| separation is constant rather than speed-dependent | coordinate transform or tablet calibration error | compare physical corners/center and XInput coordinates |

The first architectural rule remains: preserve every real input sample in
source order for drawing correctness, but do not require one rendered frame
per sample. If backlog is proven, exact document input and presentation should
be decoupled: a bounded queue drains all semantic samples, the cursor uses the
newest available position, and at most one frame is submitted per display
opportunity. Prediction, if ever added, belongs in a replaceable transient
overlay rather than committed stroke data.

## Missing Boundaries

The probe deliberately does not yet add speculative fixes. Depending on the
first physical result, the next measurement may need:

- timestamps around surface acquisition, visible-tile preparation, upload
  encoding, and queue submission;
- GPU timestamp queries and submitted-work completion;
- platform presentation feedback where available;
- a native/system cursor control to isolate app-rendered-cursor latency;
- a high-frame-rate camera measuring pen motion against emitted light;
- compositor-bypass or front-buffer techniques on platforms that expose them.

Every change to present mode, queued-frame count, event batching, or cursor
strategy requires an A/B physical capture. A visually plausible tweak is not
evidence by itself.

## First Apollo Capture: 2026-07-27

The first optimized physical run used:

- Intel UHD Graphics (Jasper Lake) through Vulkan;
- a 60.076 Hz integrated display;
- wgpu `AutoVsync` with desired maximum frame latency 2;
- the integrated ELAN pen source, producing about 200 contact samples/second
  and up to about 394 hover samples/second;
- a recovered drawing containing 778 tiles before the test.

Apollo reported support for `Immediate`, `Mailbox`, `Fifo`, and `FifoRelaxed`
presentation. `AutoVsync` does not reveal which supported mode was selected.

Representative steady one-second windows were:

| Path | Samples/s | Source excess p95 | Backend queue p95 | Latest-to-submit p95 | Input handler p95 | Frame p95 | Samples/submit p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| fast hover | 374–394 | 1.03–1.04 ms | 15.2–15.4 ms | 16.6–16.8 ms | 0.001 ms | 16.5–16.8 ms | 8 |
| first contact stroke | 202–203 | 1.03–1.07 ms | 14.6–15.1 ms | 16.5–16.7 ms | 0.28–0.37 ms | 16.3–16.6 ms | 4 |
| second contact stroke | 202–203 | 1.02–1.08 ms | 13.2–14.1 ms | 15.7–16.5 ms | 0.76–1.11 ms | 15.2–15.9 ms | 4 |

The physical report remained “really laggy” for slow/fast hover and
slow/fast drawing.

### What the capture establishes

The X source-to-backend progression is not accumulating material delay:
steady p95 relative excess is about one millisecond. Hover input handling is
effectively free. Contact handling is measurably more expensive but remains
roughly 0.3–1.1 ms p95 in these strokes; it does not explain the large lag
shared with hover.

The dominant measured common path is frame-shaped:

- the event-loop queue reaches almost one 60 Hz interval at p95;
- a newest handled sample then reaches the CPU presentation call in about one
  more 60 Hz interval at p95;
- hover frames with no painting or tile uploads themselves take almost exactly
  one display interval;
- 4–8 real tablet packets are commonly accumulated per submitted frame.

The stage probe does not yet split surface acquisition from render encoding,
but an otherwise idle hover frame taking approximately 16.7 ms strongly points
to blocking frame acquisition/presentation pacing in the common render path.
The event loop cannot receive proxy events while blocked there. The
app-rendered cursor then still has GPU queue, compositor, scanout, and panel
latency after the measured CPU endpoint.

The two p95 segments must not simply be added as an end-to-end percentile:
`backend_queue_us` covers every packet, while `latest_to_submit_us` covers only
the newest packet selected for each submitted frame. They do show two
frame-sized scheduling effects in the pipeline.

### Synchronous recovery stall

The capture also exposed a separate severe bug. A recovery checkpoint after
the first stroke took 474 ms on the event-loop thread. The following hover
window recorded:

```text
backend_queue_us mean=172106 p95=456063 max=490586
samples_per_submit mean=12 max=198
```

A later checkpoint took 669 ms. Synchronous checkpoint encoding and disk I/O
can therefore freeze hover independently of brush rendering. Recovery must
move off the interactive thread while snapshot semantics remain exact.

The first frame's 778 cache-tile upload took 264 ms, including about 116 ms in
fallback upload API calls. This startup/recovery cost was excluded from the
steady hover/contact comparison.

### Next controlled experiments

1. Add runtime-selectable present mode and queued-frame count; compare the
   current `AutoVsync`/2 baseline with `Mailbox`/1. Use `Immediate`/1 only as a
   tearing diagnostic that tests whether synchronized presentation is the
   dominant visible cost.
2. Split live frame timing into surface acquisition, scene/upload
   preparation, encoding, submission, and CPU present-call stages.
3. Re-run the exact slow/fast hover and contact sequence for each presentation
   configuration, preserving both the internal distributions and the physical
   judgment.
4. Design recovery around an immutable/snapshotted document state written by
   a worker. Do not merely disable recovery to hide the stall.
5. If low-latency presentation leaves substantial visible lag while internal
   stages remain low, add an OS-cursor control and high-speed-camera
   input-to-photon measurement.

## Presentation A/B: 2026-07-27

The follow-up physical runs used the same Apollo display, pen, recovered
drawing, and optimized build. `Mailbox` with maximum frame latency 1 removed
the two frame-shaped queues seen in the baseline:

- fast-hover backend queue p95 fell from about 15.3 ms to about 0.17 ms;
- fast-hover latest-sample-to-submit p95 fell from about 16.7 ms to about
  0.94 ms;
- contact latest-sample-to-submit was generally about 1.2--1.8 ms p95;
- steady frames were about 0.9 ms p95 for hover and remained low for contact;
- the application submitted roughly 180--200 frames/second and normally used
  only one or two tablet samples per submission.

The physical result was clearly faster but still not fast enough. Because the
measured path ended at the CPU present call, the remaining common delay is
after submission: synchronized presentation, compositor scheduling, 60 Hz
scanout, and panel response are the leading boundary.

`Immediate` with maximum frame latency 1 was the fastest physical result so
far. Between outliers it retained the low internal values of `Mailbox`:
hover queue time was about 0.1 ms p95, latest-sample-to-submit was about 1 ms
p95, and frames were normally sub-millisecond to about 1.5 ms. This mode may
tear and therefore is not automatically the final default, but it proves that
the synchronized presentation path accounts for user-visible latency that the
CPU-side probe cannot see.

### Immediate's apparent multi-second regression was recovery, not present

The `Immediate` run also appeared unusable because of literal-second freezes.
Every freeze matched a synchronous recovery checkpoint on the event-loop
thread:

| Checkpoint duration | Corresponding queue evidence |
| ---: | --- |
| 1.154 s | backend queue maximum 1.169 s, p95 1.130 s |
| 1.069 s | several hover packets queued for about 676 ms |
| 1.362 s | backend queue maximum about 1.198 s, p95 1.153 s |
| 1.364 s | backend queue maximum about 1.389 s |
| 1.299 s | same checkpoint-shaped interruption |

The document contained 996--998 allocated tiles and encoded to roughly
168--172 MB. There were no staging-buffer waits, and Immediate remained stable
between checkpoint boundaries. Presentation mode was therefore exonerated as
the cause of these extreme outliers.

## Non-blocking recovery design

Recovery now separates snapshot capture from checkpoint production:

1. Raster tile pixels use immutable reference-counted storage.
2. At the autosave deadline, the event loop copies document metadata and
   reference-counted tile handles. It does not scan or copy pixel arrays.
3. A named worker thread scans pixels, encodes the checkpoint, writes the
   temporary file, flushes it, and atomically renames it.
4. The interactive document uses copy-on-write. If drawing overlaps an active
   checkpoint, only tiles actually modified after the snapshot are copied;
   untouched tiles remain shared.
5. Every document edit advances a revision. Completion of an older snapshot
   cannot mark newer work as recovered; another checkpoint remains due.
6. Explicit close/open safety may wait for the worker and synchronously ensure
   the latest revision. Routine autosave never waits on the event-loop thread.

The checkpoint format is unchanged. A regression test proves that snapshot
encoding is byte-for-byte identical to live document encoding and remains
unchanged after the live document mutates. The complete Apollo test suite
passes.

The physical acceptance run used the recovered large document after it had
grown to 1,018 tiles. Capturing its immutable snapshot took 0.356 ms on the
event-loop thread. The worker then spent 1.705 seconds producing and atomically
installing a 215,940,531-byte checkpoint containing 13,313,315 stored pixels.
The old design would have blocked input for that full interval; the new run
did not reproduce the freeze, and the physical result was reported as fast
enough. Steady contact during the preceding Immediate/1 stress pass remained
around 0.9--1.3 ms p95 from latest handled sample to CPU submit, with most
frames below 1 ms and no checkpoint-sized outlier.

The accepted `Immediate` mode and maximum frame latency 1 are now the ordinary
application defaults, not just benchmark flags. If a platform lacks
`Immediate` but supports `Mailbox`, startup falls back to `Mailbox`; otherwise
it uses wgpu's synchronized automatic fallback. Explicit command-line options
remain available for controlled comparisons and for users who prefer
tear-free presentation over the lowest measured latency.

## Natural-brush event-loop collapse: 2026-07-28

The first live natural-brush evaluation used Immediate/1 on Apollo with the
same Wacom path. Hover and light contact were healthy: input handling was
usually tens of microseconds for hover and roughly 1 ms p95 for simple contact.
Heavy palette-knife/bristle contact caused a qualitatively different failure.

Representative one-second windows included:

```text
input_us mean/p95/max = 8094/15950/83216
damage_regions = 18225
upload_kib = 133394
backend_queue_us contact mean/p95/max = 2494865/4813506/5055843
samples_per_submit mean/p95/max = 73/4/1423
```

Another stroke produced 3,875 damage regions, about 100 MiB of upload traffic,
23.9 ms input p95, and 1.37 seconds maximum backend queue delay. Immediately
after contact stopped, hover handling returned to roughly 35 microseconds p95.
The device and XInput collection thread are therefore not the source of the
stall. CPU contact work blocks winit's event-loop consumer, the independent
XInput producer continues to enqueue samples, and the app later processes a
large stale burst.

The document had also grown from 2,012 to more than 3,000 allocated tiles.
Immutable snapshot capture remained below 1 ms, so the earlier synchronous
autosave bug has not returned. However, full recovery workers wrote
552--632 MiB checkpoints in 4.5--12.3 seconds while some stress strokes were in
progress. That background encoding and I/O competes for memory bandwidth and
worsens the constrained-machine case. It does not explain the failure alone:
heavy contact stalls also occur outside checkpoint completion boundaries.

The application currently performs this sequence for every tablet packet:

```text
distance-resample and mutate brush dabs
    → drain active gesture damage
    → recompose active-layer damage
    → merge/schedule GPU tile damage
    → request redraw
```

When the brush emits many dabs, repeating the entire presentation boundary per
packet creates thousands of intermediate damage regions even though only the
latest accumulated image can be presented. The next probe splits the existing
input timing into brush mutation, gesture-damage drain, layer recomposition,
and GPU-damage scheduling distributions. This is required before changing the
boundary: quality, exact canonical pixels, final damage, undo, and input sample
order must remain unchanged.

Likely corrective architecture, pending the split evidence:

1. Keep canonical brush mutation and every input sample in order.
2. Drain/recompose/schedule once for a bounded group of already-queued tablet
   samples or once before presentation, not after every packet.
3. Maintain a strict time/sample budget so catch-up cannot monopolize the event
   loop and hover/up semantics remain prompt.
4. Coalesce final per-tile damage before upload preparation.
5. Measure checkpoint-worker contention separately and move recovery toward
   incremental/delta storage rather than weakening durability.

### Stage split and bristle CPU profile

The stage probe isolated one light stroke and one sustained heavy natural-brush
stroke. The light stroke remained healthy:

```text
input_us mean/p95/max = 283/1066/2229
mutation_us mean/p95/max = 79/329/843
recompose_us mean/p95/max = 62/313/1133
```

The sustained heavy stroke produced:

```text
input_us mean/p95/max = 5620/11375/25236
mutation_us mean/p95/max = 4302/9152/22945
damage_drain_us mean/p95/max = 0/1/2
recompose_us mean/p95/max = 1318/2746/7023
damage_schedule_us mean/p95/max = 6/12/60
backend_queue_us contact mean/p95/max = 66615/197300/211213
samples_per_submit mean/p95/max = 13/140/140
```

Brush mutation is the primary event-loop cost. Per-packet layer recomposition
is material but secondary. Damage draining and GPU scheduling are not useful
optimization targets. Moving recomposition to a bounded presentation boundary
should remove roughly one quarter of the current handler cost and eliminate
many intermediate damage/upload regions, but it cannot repair the brush kernel
by itself.

The deterministic brush harness can now select one brush, tile size, run count,
and warmup count. Linux `perf` on Apollo attributed 77.1% of all sampled cycles
in an isolated bristle replay to `LaneDabKernel<24>::run`. Pickup sampling used
1.8%; tile edit bookkeeping and damage insertion were each below 1%. The
machine retired 26.35 billion instructions at 1.26 instructions/cycle over 50
profiled runs, while last-level-cache misses were about 1% of LLC loads. The
first-order problem is scalar work and dependency chains in the conservative
per-pixel loop, not DRAM capacity misses.

Disassembly exposed two avoidable inner-loop operations:

- the oriented-box signed-distance expression performed a square root for
  solid interior pixels and for obviously empty AABB corner pixels;
- `f32::fract()` became an out-of-line `truncf` call for every covered pixel.

The kernel now uses exact solid-interior/empty-exterior coverage fast paths and
derives the fractional lane coordinate from the already bounded integer lane
index. It does not change dab spacing, bristle count, pickup, deposition,
blending, or the antialiasing fringe. A dense reference test compares the
coverage fast paths with the original signed-distance expression. The
deterministic replay checksum remained `85403.150316`.

On the same Apollo 128-pixel-tile case, 20-run median bristle time fell from
116.123 ms to 85.517 ms, a 26.4% reduction. A second sampled run fell from
20.93 to 14.76 billion cycles and from 26.35 to 19.67 billion instructions;
`truncf` disappeared from the profile. The lane kernel still owns 73.2% of
remaining cycles, so this is a useful first reduction rather than completion.
The next live trace must determine how much of the deterministic gain survives
large-document recomposition and recovery-worker contention.
