# Physical Pen Latency Investigation

Status: live internal latency probe implemented; first physical Apollo capture
recorded, 2026-07-27.

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
