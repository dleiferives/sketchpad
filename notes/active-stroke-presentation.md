# Active-Stroke Presentation and Retained Compositing

Status: research decision and experiment specification, 2026-07-28. No new
presentation architecture is approved by this note.

## Decision

Do not implement “Immediate mode for the current layer” as if it were a
portable `wgpu` feature. Presentation mode belongs to a surface swapchain, not
to a document layer, texture, shader, or draw call. Every draw encoded into one
Sketchpad surface frame reaches the same `present()` operation and therefore
the same presentation mode.

The underlying idea is still worth investigating, but it contains three
different proposals with different costs:

1. **Layer recomposition caching** can avoid repeatedly reading many unchanged
   layers while a small active region changes.
2. **A retained final-frame cache plus a transient overlay** can avoid
   re-extracting or resampling the stable scene, but still presents a complete
   surface and may add GPU memory traffic.
3. **A true front-buffer/compositor overlay** can reduce scanout latency for a
   small active region, but is platform-specific and is not exposed by an
   ordinary portable `wgpu` surface.

Keep the current direct-tile renderer as the application control. Finish the
existing full-precision display-cache comparison before adding another cache.
Then measure layer-count scaling separately. Only prototype a viewport cache
if those results identify stable-scene sampling or many-layer recomposition as
a material cost.

Do not retain “the last N strokes” merely because they are recent. Stroke count
does not describe work: one stroke can touch one pixel or the whole canvas.
Bound transient state by dirty area, bytes, age, and the next presentation
deadline. The current in-progress stroke and replaceable predicted tail are
useful semantic boundaries; an arbitrary recent-stroke count is not.

## What Sketchpad Actually Does Today

The motivating concern was that Sketchpad might redraw all prior strokes for
every new sample. It does not.

For each real input update:

1. the brush edits only damaged pixels in the active sparse raster layer;
2. `RasterDocument::recompose_damage` recomputes only those damaged pixels in
   the authoritative visible composite;
3. recomposition reads only visible source layers that own the affected tile;
4. `RasterDisplayPipeline::sync_damage` queues only the dirty tile subregions;
5. the GPU uploads those subregions into persistent tile-array pages; and
6. the display pass clears the acquired surface, draws the background, issues
   one instanced draw per nonempty texture page, and draws the cursor.

Consequently, Sketchpad already retains document pixels, the flattened
composite, GPU tile residency, visibility, and instance buffers. It does not
issue one draw per tile or stroke. On the current Apollo stress scene, the
roughly 1,000 visible tiles occupy only a few texture pages.

The work that still scales with the displayed frame is the final render target:
the surface is cleared, visible canvas pixels are shaded, the cursor is drawn,
and the complete acquired swapchain image is presented. The current direct
path therefore has two distinct possible scaling problems:

- CPU recomposition grows with damaged pixels times the number of intersecting
  visible layers;
- GPU display work grows primarily with covered screen pixels, source-texture
  access/locality, and final attachment traffic—not necessarily tile draw
  count.

Existing Apollo isolation supports the second distinction. Reducing visible
tiles from 253 to 128 did not change the measured render-pass p95, while a
further change to 32 tiles did. The latter also changed texture locality and
texel reuse, so this is evidence for a sampling/locality cost, not proof.

## Terms That Must Stay Separate

| Mechanism | What it retains or changes | What it does not guarantee |
| --- | --- | --- |
| Dirty CPU recomposition | Recomputes only affected document pixels | Lower scanout latency |
| Dirty texture upload | Transfers only changed texels | Partial swapchain presentation |
| Retained viewport texture | Reuses a stable final image inside the app | Avoiding a full-screen sample/copy and present |
| Swapchain damage hint | Tells a compositor which rectangles changed | That stale pixels in acquired images are valid |
| Immediate present mode | Changes queueing for an entire surface | Different present modes for individual layers |
| Front-buffer overlay | Presents a small transient compositor layer directly | Portable availability through raw `wgpu` |

`wgpu::PresentMode` is explicitly part of `SurfaceConfiguration` and controls
the timing of `SurfaceTexture::present()`. `Immediate` swaps a complete
presented image without queueing; it is not a render-pass or texture option
([wgpu `PresentMode`](https://wgpu.rs/doc/wgpu/enum.PresentMode.html),
[wgpu `Surface`](https://docs.rs/wgpu/latest/wgpu/struct.Surface.html)).

Native APIs can carry damage metadata farther than `wgpu` currently does.
Vulkan incremental present accepts changed rectangles, but defines them as an
optimization hint and still requires every pixel in the presented image to be
correct if the presentation engine ignores the hint
([`VkPresentRegionsKHR`](https://docs.vulkan.org/refpages/latest/refpages/source/VkPresentRegionsKHR.html)).
DXGI `Present1` can pass dirty rectangles to the Desktop Window Manager, but
multi-buffer incremental rendering must keep stale backbuffers coherent across
overlapping damage
([DXGI presentation improvements](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-1-2-presentation-improvements),
[`Present1`](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgiswapchain1-present1)).
Wayland likewise accepts buffer damage as compositor state, not as a per-draw
present mode
([`wl_surface.damage_buffer`](https://wayland.app/protocols/wayland#wl_surface:request:damage_buffer)).

This is why changing Sketchpad's surface pass from `Clear` to `Load` would not
make a safe partial renderer. Swapchain images rotate; the newly acquired image
is not necessarily the image drawn last frame.

## Candidate Architectures

### A. Current direct composite tiles

This remains the control:

```text
active-layer dirty pixels
        |
        v
dirty final document composite
        |
        v
dirty GPU tile-atlas uploads
        |
        v
background + visible page instances + cursor
        |
        v
one surface-wide Immediate present
```

Advantages:

- exact canonical `f32` premultiplied-linear pixels;
- sparse memory follows painted content;
- dirty CPU and upload work already exists;
- a single surface render pass is friendly to tile-based mobile GPUs;
- camera changes reuse the same document-space tiles.

Cost:

- every redraw shades the visible final canvas again;
- a damaged pixel is recomposited through all intersecting visible layers;
- the surface still incurs drawable store and presentation work.

### B. One GPU texture per document layer

This sounds close to “make only the current layer immediate,” but is not the
recommended first experiment. The surface still has one presentation mode, and
the final frame must still composite the visible layers. A straightforward
implementation changes a cached single-composite sample into multiple texture
samples and blends, with cost growing with layer count and screen coverage.

It may move CPU compositing to the GPU, but that is not inherently a win on an
integrated or mobile GPU. It also complicates opacity, future blend modes,
masks, erasing, layer reordering, color management, and exact export
consistency.

If many-layer CPU recomposition becomes a measured bottleneck, prefer a sparse
compositing tree or cached groups. Ordinary premultiplied source-over
compositing is associative, so unchanged layer subtrees can be retained and
only the path containing the edited layer recomputed
([W3C compositing model](https://www.w3.org/TR/compositing-1/)).
That provides logarithmic group invalidation without requiring every layer to
be sampled by every final frame. It is still a hypothesis until the many-layer
trace demonstrates a problem.

#### Exact multiple-layer cache model

The final image must change where the active layer changes, but unchanged
layers do not have to be read individually each time. For an ordinary
source-over layer stack, split the stack around the active layer:

```text
top
┌──────────────────────────┐
│ cached composite: above  │  U
├──────────────────────────┤
│ changing active layer    │  A
├──────────────────────────┤
│ cached composite: below  │  B
└──────────────────────────┘
bottom

final dirty pixels = U over (A over B)
```

`B` contains the composite of every visible layer below the active layer. `U`
contains every visible layer above it, composited against transparent. If the
active layer is topmost, `U` is empty. If it is bottommost, `B` is empty.

Painting modifies a dirty rectangle of `A`; erasing modifies its color and
alpha in the same way. Recomputing `U over (A over B)` for only that rectangle
produces exact final pixels. An eraser on `A` reveals the already-cached `B`,
then the unchanged `U` is reapplied. No unchanged layer is redrawn
individually, and ordinary layer erasing still affects only the selected layer.
A future “erase through all layers” tool would be a different document
operation, not a presentation optimization.

The simplest useful cache is therefore not one GPU texture for every layer. It
is:

- canonical sparse tiles for every layer, which already exist;
- one sparse or visible-working-set cache for layers below the active layer;
- one for layers above it;
- the authoritative final composite/root tiles, which already exist; and
- revision and damage metadata per cached tile.

On an active-layer pixel edit:

1. mark the active layer's exact tile rectangle dirty;
2. leave `B` and `U` valid;
3. recompute the same rectangle in the final root from `B`, `A`, and `U`;
4. upload only that final root subrectangle; and
5. render/present the normal final root plus cursor.

Changing layer selection rebuilds or lazily requests a different split.
Visibility, opacity, reordering, insertion, deletion, masks, and blend-mode
changes invalidate the affected range caches. Camera motion does not invalidate
document-space composite tiles, although it changes which tiles should be made
resident first.

For arbitrary repeated edits and many layers, a balanced compositing tree is
the general form:

```text
                         final root
                       /            \
                 layers 0..3      layers 4..7
                  /      \          /      \
               0..1      2..3     4..5     6..7
```

An edit to one leaf invalidates only the dirty rectangle along that leaf's path
to the root. Each level combines two cached children, making recomposition work
proportional to dirty area times tree depth rather than dirty area times all
intersecting layers. Layer reorder or structural edits invalidate a larger
range.

The tree has a real memory cost. Caching every internal tile for every painted
coordinate can approach another copy of content per tree level in a badly
overlapping document. Use sparse, revisioned, lazy caches:

- allocate an internal tile only when both the requested view and damage need
  it;
- prioritize visible tiles and the active brush neighborhood;
- evict derived tiles freely because canonical layer tiles remain authoritative;
- cap cache bytes explicitly;
- keep the root final composite and the active split hotter than cold
  intermediate nodes;
- collapse empty or single-child ranges without materializing a texture.

This model can run on the CPU, GPU, or both. Sketchpad currently needs the CPU
final composite for color pickup, export, recovery semantics, and
destination-dependent brushes, so moving only display compositing to the GPU
would either duplicate work or make those operations asynchronous. Optimize
the current CPU dirty compositor first if the layer-count benchmark exposes
it. A later GPU compositor can use the same tile revisions and tree plan
without changing document semantics.

### C. Portable retained viewport plus exact dirty replacement

The strongest portable experiment is a viewport-sized cache of the final
display result, not a second canvas-sized copy of all canonical pixels:

```text
stable final viewport cache
          +
exact final-composite dirty replacement regions
          +
cursor / replaceable predicted tail
          |
          v
one surface-wide present
```

The dirty overlay should contain **final visible replacement pixels**, not
merely alpha-blended geometry for the current stroke. This distinction keeps
the result correct when:

- the active layer is below another layer;
- an eraser makes active-layer pixels transparent and reveals lower content;
- opacity or later blend modes affect the result;
- a destination-dependent brush mixes with existing pixels.

At stroke end, the changed regions become part of the stable cache and the
transient region is cleared. Camera, viewport, color-transform, layer-order,
visibility, or opacity changes invalidate the cache or fall back to direct
tiles.

The cache format must not reduce document precision. A candidate may cache the
already color-converted viewport in a surface-compatible format because the
display is quantized there anyway, but it is acceptable only if direct and
cached presented pixels are byte-exact. The canonical document remains `f32`.
A fixed 4096×4096 `Rgba32Float` canvas cache costs 256 MiB. In contrast,
viewport-bounded RGBA8 storage costs about 7.9 MiB at 1080p and 31.6 MiB at 4K,
before any extra buffers. Those smaller numbers are a reason to test—not proof
that the extra pass is faster.

The fundamental risk is bandwidth. The cache must be stored to external memory
when updated and sampled or copied into the current surface. Tile-based mobile
GPUs are optimized to keep one render pass in on-chip tile memory; extra render
targets and pass boundaries can force external-memory round trips. Khronos
identifies attachment bandwidth as a primary tile-GPU concern and recommends
avoiding unnecessary loads/stores
([Vulkan tile-based rendering guidance](https://docs.vulkan.org/guide/latest/tile_based_rendering_best_practices.html),
[Vulkan subpass sample](https://docs.vulkan.org/samples/latest/samples/performance/subpasses/README.html)).
Apple gives the same guidance for Apple GPUs: render-target load/store actions
move attachments through tile memory, and unnecessary passes increase system
bandwidth
([Apple Metal optimization](https://developer.apple.com/videos/play/wwdc2020/10632/),
[Metal load/store actions](https://developer.apple.com/documentation/Metal/setting-load-and-store-actions)).

This candidate may win on a dense desktop scene through contiguous sampling
and stable work, lose on a mobile tiler through additional texture traffic, or
be neutral because a full-screen copy replaces a full-screen tile-atlas sample.
There is no responsible default without device measurements.

### D. True platform front-buffer active overlay

Android demonstrates that the original idea is valid when supported below the
ordinary swapchain abstraction. `GLFrontBufferedRenderer` renders active
content into a front-buffered layer while stable content remains in a
traditional multi-buffered layer. `commit()` renders the complete scene into
the multi-buffered layer. Android explicitly limits front-buffer rendering to
a small area because it can tear and is not intended to refresh the complete
screen
([`GLFrontBufferedRenderer`](https://developer.android.com/reference/androidx/graphics/lowlatency/GLFrontBufferedRenderer),
[Android advanced stylus features](https://developer.android.com/develop/ui/views/touch-and-input/stylus-input/advanced-stylus-features)).

Android Ink exposes the corresponding wet/dry lifecycle at a higher level:
in-progress strokes remain in an overlay until a finished immutable stroke is
handed to the retained renderer
([`InProgressStrokesView`](https://developer.android.com/reference/androidx/ink/authoring/InProgressStrokesView)).

This is the closest match to “only the current stroke is immediate.” It should
be a later Android backend experiment, not a portable `wgpu` assumption.
Equivalent native opportunities must be evaluated per platform. Apple, for
example, exposes low-latency event dispatch and an immediate-presentation
request through `UIUpdateLink`, while predicted touches remain temporary and
are replaced by real input
([`wantsImmediatePresentation`](https://developer.apple.com/documentation/uikit/uiupdatelink/wantsimmediatepresentation),
[predicted touches](https://developer.apple.com/documentation/uikit/minimizing-latency-with-predicted-touches)).

Multiple transparent desktop windows or generic compositor surfaces should not
be the baseline. They add synchronization, alpha/color-management, resizing,
input, and compositor behavior that `wgpu` cannot make uniform. Consider them
only after a target platform exposes a documented low-latency layer primitive.

## What Could Actually Improve

These are independent goals and must not be credited to one another:

| Goal | Plausible mechanism | Primary measurement |
| --- | --- | --- |
| Reduce many-layer CPU work | Sparse prefix/suffix or compositing-tree cache | recomposition p95 and source-pixel reads versus layer count |
| Reduce stable-scene GPU work | Viewport final-frame cache | GPU pass time, fragment work, texture bytes, total bandwidth |
| Reduce input-to-photon delay | Platform front buffer, immediate-presentation scheduling, prediction | high-speed-camera latency and tear rate |
| Reduce power while hovering/drawing | Fewer presents and/or lower per-present bandwidth | sustained package/GPU power and thermals |
| Avoid active-stroke handoff artifacts | Explicit wet/dry ownership protocol | visual continuity and exact handoff tests |

Immediate/1 already made Apollo physically acceptable. A new overlay is
therefore not an emergency latency fix. Its near-term value would be
scalability, power, and many-layer behavior. Presentation cadence is a separate
lever: lowering the cost of one frame does not prevent a 400 Hz input stream
from provoking roughly 180–200 presents per second on a 60 Hz display.

## Required Experiments

### 1. Establish the missing controls

Complete the existing `direct` versus `cache-rgba32` matrix on Apollo before
writing new rendering code:

- sparse, dense, and stress scenes;
- fixed camera at 1×, 2×, and 4× input playback;
- cold creation, steady dirty updates, static hover/cursor, erasure, and cache
  invalidation;
- GPU render p50/p95/max, app-to-submit p95, upload bytes, and allocation;
- exact direct-versus-cache readback.

The cache implementation and exact smoke oracle exist, but no complete timing
matrix is recorded in the repository.

### 2. Isolate layer recomposition

Add a trace family with 1, 8, 32, and 128 intersecting visible layers, with the
active layer at the bottom, middle, and top. Hold damaged area and brush output
constant. Report:

- recomposition time;
- damaged pixels;
- source tiles and source pixels read;
- allocations and retained bytes;
- total input-event and frame-stage p95.

If cost is not material at a realistic layer count, do not build a
compositing tree.

### 3. Prototype a viewport cache behind an experiment flag

Only after the first two controls:

- bound storage to viewport dimensions rather than canvas dimensions;
- preserve the current direct path as a same-revision control;
- update exact final-composite dirty rectangles;
- draw cursor and any future prediction after stable content;
- rebuild or fall back on camera and document-structure invalidation;
- record cache build, hit, patch, fallback, and copied/sampled-byte counters.

Run this on Apollo and at least one tile-based mobile-class GPU before
promotion.

### 4. Test correctness before interpreting speed

The oracle must cover:

- active layer at every relevant z position;
- paint, eraser, opacity, visibility, reorder, undo, and cancel;
- hard and destination-dependent mixing brushes;
- overlapping dirty rectangles and tile edges;
- zoom/pan/resize during or immediately after a stroke;
- active-to-stable handoff with no missing or double-rendered frame;
- device/surface loss and cache reconstruction;
- exact final canonical pixels and byte-exact displayed output.

Predicted pixels, when added, may be visibly temporary but must never enter
canonical history or color pickup before real input replaces them.

### 5. Promotion gate

Promote no cache merely because it reduces draw calls. It must:

- preserve the exact output and document oracle;
- improve the measured limiting resource on a realistic dense or many-layer
  case;
- avoid a material p95/max regression on Apollo's constrained integrated GPU;
- remain bounded by visible working set or viewport rather than canvas size;
- avoid new cache-build or handoff spikes on the active input path;
- survive sustained mobile bandwidth, power, and thermal measurement.

If the portable viewport cache does not pass, retain direct tiles. The wet/dry
semantic boundary remains useful for prediction and native front-buffer
backends even when the portable renderer draws the authoritative composite
directly.
