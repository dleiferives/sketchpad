# Immediate-Mode UI Overlay Architecture

Status: architecture accepted, cached overlay and first functional controls
implemented, 2026-07-28.

The dependency compatibility gate has passed on Apollo. Sketchpad pins egui,
`egui-winit`, and `egui-wgpu` to revision
`2cb071f7f6d71e0f888ba31bec9b3eb5ed5428fe`, which uses the same `wgpu` 30 and
`winit` 0.30 versions as the application. `cargo check --bin sketchpad`
completed successfully with that exact revision on 2026-07-28.

## Recommendation

Use an immediate-mode UI over Sketchpad's existing canvas, but do not build a
complete widget system first.

Implement an embedded egui overlay behind a narrow Sketchpad-owned boundary:

- keep the exact compatible development revision pinned until a compatible
  release is available;
- depend on `egui`, `egui-winit`, and `egui-wgpu`, not `eframe`;
- retain Sketchpad's existing `winit` event loop, `wgpu` instance/device/queue,
  surface, present mode, and command encoder;
- render the canvas first and the UI overlay last in the same surface frame;
- route UI input before canvas input;
- turn widget results into existing application commands rather than allowing
  widgets to own document state;
- preserve event-loop sleep and cache unchanged prepared UI work;
- measure each integration slice on Apollo before expanding the UI surface.

If the integration cannot preserve one `wgpu` version, low pen latency, idle
sleep, predictable input capture, or a suitably custom visual result, remove
it and implement the deliberately narrow overlay described in
[Custom fallback](#custom-fallback). The experiment must leave the document
and canvas boundaries reusable.

This is preferable to immediately owning layout, scrolling, focus, text
editing, IME, clipboard, keyboard navigation, touch behavior, DPI, tooltips,
drag interaction, and accessibility. Sketchpad's visual interface may remain
small, but a layer list, brush controls, color controls, images, inline rename,
sliders, and popovers already exercise most of those systems.

## “Immediate” Has Two Meanings

Immediate-mode UI is unrelated to `wgpu::PresentMode::Immediate`.

- **Immediate-mode UI** declares widgets from current application state and
  receives responses during a UI update.
- **Immediate surface presentation** controls swapchain queueing for the
  complete window.

Sketchpad can use both, but neither implies that the UI must run continuously.
The event loop should still sleep when the application is idle. Egui's own
documentation says it only requests repaint for interaction or animation
([egui README](https://github.com/emilk/egui#why-immediate-mode)).

## Current Compatibility, 2026-07-28

### Egui

Released egui 0.35 is not directly compatible with Sketchpad's GPU ownership:
its workspace pins `wgpu` 29
([egui 0.35 manifest](https://raw.githubusercontent.com/emilk/egui/0.35.0/Cargo.toml)).
Different `wgpu` major versions produce different Rust device, queue, texture,
and render-pass types.

Egui's current development workspace has moved to `wgpu` 30 and retains
`winit` 0.30
([egui development manifest](https://raw.githubusercontent.com/emilk/egui/main/Cargo.toml)).
This makes an exact Git-revision experiment technically possible without
downgrading Sketchpad or linking two `wgpu` versions. It is not the same as a
released dependency. Pin the commit, record it, and move to the next compatible
release when available.

Egui is an appropriate shape for this experiment because it is a library rather
than an application framework, can integrate anywhere textured triangles can
be drawn, and already supplies buttons, images, text editing, sliders, panels,
scrolling, color picking, and AccessKit semantics
([egui features and integrations](https://github.com/emilk/egui#state)).
The development `egui-wgpu` renderer accepts existing `wgpu` resources and can
encode into an existing integration
([egui-wgpu renderer source](https://github.com/emilk/egui/blob/main/crates/egui-wgpu/src/renderer.rs)).

Risks:

- the pinned source is between releases and may change;
- egui explicitly expects breaking upgrades;
- its default appearance is not Sketchpad's desired identity;
- its own broad estimate is 1–2 ms of CPU work per typical UI frame, which is
  too large to dismiss on Apollo even though Sketchpad's first interface will
  be much smaller;
- direct XInput2 tablet events require deliberate integration rather than
  assuming synthesized mouse events are authoritative.

The exact pinned revision passed the initial dependency and type-compatibility
check on Apollo. The remaining adoption gates are input ownership, UI work
caching, visual control, idle behavior, and measured drawing latency. These are
reasons to keep the integration narrow, not reasons to fork the canvas
architecture around egui.

## Portability Boundary

The same egui declarations and custom components can be used on Linux,
Windows, macOS, Android, and iOS/iPadOS. Egui is not the portable application
shell:

- `winit` and `wgpu` provide the platform window and graphics backends;
- Sketchpad supplies platform packaging, lifecycle, file integration, safe
  areas, and keyboard/IME details;
- native Wacom, Apple Pencil, and Android stylus paths remain Sketchpad input
  backends so drawing samples do not acquire UI latency;
- egui receives the minimal position/contact events needed when a pen is
  interacting with UI rather than the canvas.

Use one in-window overlay and responsive panels. Do not make the product
depend on secondary native viewports, hover-only controls, right-click, or
desktop-sized hit targets. Those are the least portable parts of a desktop UI.
Desktop integration comes first; Android and iOS application shells follow
after the interaction and rendering boundary is stable.

The current `egui-winit` dependency features are intentionally the Linux
desktop features needed by Apollo and Atlas. Before another platform build,
move platform features into target-specific dependency sections and select the
appropriate Android activity backend. A successful Linux dependency check is
not presented as a mobile application build.

## Visual Design Boundary

Egui supplies interaction, layout, clipping, text, focus, scrolling, and
accessibility semantics. It does not define Sketchpad's visual identity.

Avoid assembling the product from default-themed widgets. Build a small
Sketchpad component set using egui allocation and response semantics with
custom painting:

- icon/tool button;
- segmented tool selector;
- continuous slider and numeric readout;
- color swatch;
- panel and popover;
- label, separator, and tooltip;
- layer row and scrollable layer list.

Central design tokens define color, spacing, type scale, corner radius,
borders, control size, and interaction states. Components must expose normal
egui semantic labels and focus behavior even when their pixels are fully
custom. Canvas overlays, brush previews, unusual blend effects, and other
latency-sensitive or graphics-specific visuals may stay in Sketchpad's direct
`wgpu` renderer.

This makes the UI visually unrestricted while avoiding ownership of text
editing, IME, focus traversal, clipping, and touch interaction. The structure
is immediate-mode; the appearance is Sketchpad's.

### Other candidates

| Candidate | `wgpu` 30 status | Assessment |
| --- | --- | --- |
| `glyphon` 0.12 | Released on `wgpu` 30 | Strong text renderer for a custom fallback; not layout, widgets, input, or accessibility |
| Yakui development branch | Uses `wgpu` 30 | Attractive game-style immediate/declarative model, but its own README calls it work-in-progress with sharp edges |
| Iced development branch | Uses `wgpu` 29 | Larger Elm-style application/runtime model and currently mismatched |
| `imgui-wgpu` 0.28 | Uses `wgpu` 29 | Mismatched and less suitable for the intended polished drawing UI |
| Vello development branch | Uses `wgpu` 29 | A renderer rather than widget/input semantics; not a UI shortcut |

Primary manifests:

- [`glyphon` 0.12 / `wgpu` 30](https://raw.githubusercontent.com/grovesNL/glyphon/main/Cargo.toml)
- [Yakui development dependencies](https://raw.githubusercontent.com/SecondHalfGames/yakui/main/Cargo.toml)
- [Yakui status and architecture](https://github.com/SecondHalfGames/yakui)
- [Iced development dependencies](https://raw.githubusercontent.com/iced-rs/iced/master/Cargo.toml)
- [`imgui-wgpu` dependencies](https://raw.githubusercontent.com/Yatekii/imgui-wgpu-rs/master/Cargo.toml)
- [Vello development dependencies](https://raw.githubusercontent.com/linebender/vello/main/Cargo.toml)

There is presently no released, mature, full immediate-mode Rust toolkit that
both clearly owns this exact integration use case and already releases against
`wgpu` 30. The practical options are a pinned egui revision, a pinned less
mature toolkit, waiting, or custom widgets.

## Ownership Boundary

Egui should own only transient UI mechanics:

- widget layout and hit regions;
- hover, active, focus, scroll, popup, and text-edit mechanics;
- tessellation and UI texture bookkeeping;
- cursor/icon and accessibility output.

Sketchpad continues to own:

- the document and all layer/brush/color semantics;
- canvas input, tablet provenance, and pressure;
- pointer ownership for the full duration of a contact;
- application commands and undo;
- dialogs, persistence, recovery, and background work;
- GPU surface configuration and presentation;
- frame scheduling and performance counters.

The dependency direction is:

```text
document/application snapshot
          |
          v
      build UI
          |
          v
       UiAction
          |
          v
existing Sketchpad command methods
```

Widgets may read a compact immutable view such as layer IDs, names, active
state, visibility, opacity, brush settings, and recent colors. They return
actions such as:

```text
SelectLayer(id)
ToggleLayerVisibility(id)
MoveLayer(id, destination)
SetLayerOpacity(id, value)
SelectTool(tool)
SetBrushDiameter(value)
SetColor(linear_rgb)
OpenDocument
SaveDocument
```

They do not mutate a shadow document or own undo transactions. Continuous
controls may preview a value while dragging, but the document command boundary
must define coalescing and final history semantics.

Do not create a generic `dyn UiBackend` hierarchy before a second
implementation exists. Keep the useful seam at `UiAction`, input ownership,
and the overlay draw boundary.

## Input Routing

UI gets first refusal on pointer input:

```text
winit or direct-tablet sample
          |
          v
UI input adapter / hit ownership
       | consumed
       +--------------------> UI
       |
       | unconsumed
       v
picker / pan / canvas stroke
```

Rules:

1. Hover may move between canvas and UI.
2. On contact/down, exactly one owner is selected.
3. The owner remains fixed until up/cancel/focus loss.
4. A contact that begins on UI cannot paint by sliding onto the canvas.
5. A stroke that begins on canvas cannot activate a button by sliding over it.
6. The brush cursor is hidden or replaced by the UI cursor while UI owns hover.
7. Direct XInput2 tablet samples and ordinary `winit` mouse events must not
   cause duplicate UI clicks.
8. UI cannot begin a modal action while a canvas stroke is active.

`egui-winit::State::on_window_event` returns an event response and accumulates
input for the next frame
([egui-winit `State`](https://docs.rs/egui-winit/0.35.0/egui_winit/struct.State.html)).
Sketchpad's direct tablet stream must either be translated into egui pointer
events when it is over UI or use an explicit UI hit/capture adapter. Treat
native pressure as canvas data; ordinary UI buttons need position, contact,
tool identity, and capture, not brush-pressure semantics.

## Rendering Order

Use the existing surface texture and command encoder:

```text
surface render pass
  1. paper/canvas
  2. visible document composite
  3. brush cursor when over canvas
  4. UI shapes, images, and text
present complete surface
```

The UI uses premultiplied-alpha blending and physical-pixel clipping derived
from logical DPI-scaled rectangles. It must not allocate another full-window
render target merely to be “on top.” Egui's paint output is clipped triangle
meshes plus texture deltas; its renderer can draw them after the canvas.

Prefer one render pass when the integration permits it. An unnecessary
store/load boundary can cost bandwidth on mobile tile GPUs. A separate pass
with `LoadOp::Load` is an acceptable first compatibility spike only if it is
measured and later folded into the surface pass.

## Repaint and Caching Policy

Immediate-mode declaration does not require full UI layout and buffer uploads
for every canvas redraw.

There are three kinds of frame:

1. **UI dirty**: input over UI, UI-visible application state changed, resize,
   DPI, animation, text editing, or texture change. Run UI logic, layout,
   tessellation, texture deltas, and GPU buffer preparation.
2. **Canvas-only dirty**: pen movement changes the canvas while the UI and its
   hover state cannot change. Reuse the last prepared UI paint data/GPU
   buffers and issue only its draw commands.
3. **Idle**: request no redraw and sleep.

The experiment must prove whether `egui-wgpu` safely supports replaying
unchanged prepared paint data without calling its upload path. Do not depend on
undocumented behavior silently. If it cannot, record that as an adoption cost
or add a small, tested retained mesh adapter.

During an active canvas stroke, freeze ordinary panel layout and interaction
until the contact ends. The displayed content may still reflect safe state
changes at stroke finalization. This avoids putting panel layout, text shaping,
and UI uploads on the latency-critical sample path.

Hidden panels are simply not declared. Their remembered session state may
remain, but they produce no hit regions or paint jobs.

## Implementation Plan

The UI is divided into independently reviewable and measurable slices. A
functional scaffold is not a commitment to the final layout.

### Slice 1: overlay foundation

1. Add a small `UiOverlay` that owns the egui context, `egui-winit` state,
   renderer, prepared paint jobs, and UI-only session state.
2. Feed it immutable `UiSnapshot` values and collect `UiAction` values.
3. Use Sketchpad's existing surface texture, device, queue, encoder, and
   surface format.
4. Draw the canvas first and transparent UI geometry last.
5. Rebuild disposable UI GPU resources after device/surface recreation.

Commit this slice only after the application builds, opens, draws, resizes,
and presents with the overlay both visible and hidden.

### Slice 2: input ownership

1. Route `winit` mouse/keyboard/touch events through the UI adapter before
   canvas commands.
2. Translate direct XInput2 tablet position/down/up events only while UI hover
   or capture requires them.
3. Select one owner on contact: UI, canvas drawing, color sampling, or panning.
4. Hold that owner through release, cancel, or focus loss.
5. Suppress duplicate synthesized mouse input after native tablet samples.
6. Keep pressure and high-rate drawing samples on the native canvas path.

Commit this slice after synthetic routing tests and physical Wacom tests prove
that UI presses never paint and strokes never activate controls.

### Slice 3: repaint and prepared-data caching

Track UI invalidation separately from canvas invalidation:

- UI input, UI-visible application state, resize/DPI, texture changes, and
  animation make UI preparation dirty;
- canvas-only input reuses the last prepared UI paint jobs and GPU buffers;
- hidden UI skips its pass;
- idle requests no redraw.

Instrument UI declaration/layout, tessellation, texture update, buffer update,
and overlay encoding separately. Verify and document the `egui-wgpu` prepared
buffer reuse behavior rather than assuming it.

### Slice 4: custom component foundation

Define design tokens and implement the minimal Sketchpad components: tool
button, segmented selector, slider, swatch, panel, popover, label, tooltip, and
layer row. Use custom painter output while retaining semantic labels, keyboard
focus, and appropriate touch target sizes.

### Slice 5: first usable controls

Create a neutral temporary layout:

- compact tool selection for pen, eraser, and mixing;
- brush diameter and opacity controls;
- active/recent color controls;
- undo and redo;
- hide/show UI;
- a collapsible layer panel.

These controls must call the same application command methods as keyboard
shortcuts. There is no second UI-owned copy of document or brush state.

### Slice 6: product panels

Expand through existing behavior before inventing new document semantics:

- layer selection, visibility, create, duplicate, delete, order, and opacity;
- open, save, import, and export actions;
- richer color and mixing controls;
- presets, followed later by the planned brush editor.

Inline layer rename, thumbnails, drag-and-drop polish, and animation follow
only when the basic controls and input latency are accepted.

### Slice 7: platform shells

After the desktop interaction boundary is stable, factor Cargo features and
platform adapters for Windows and macOS, then Android and iOS/iPadOS. Reuse the
same `UiSnapshot`, `UiAction`, design tokens, and component declarations.
Platform-specific stylus backends and lifecycle code remain outside egui.

## First Vertical Slice

The first implementation milestone combines slices 1 through 3. Rendering UI
without correct pointer ownership and caching would validate the wrong
architecture. It should visibly prove:

1. a fixed custom-painted control surface;
2. a label, vector icon, and custom button;
3. hide/show behavior that removes paint and hit regions;
4. a brush-diameter slider backed by the real brush setting;
5. mouse and direct-pen hover/contact ownership;
6. correct DPI scaling and resize;
7. event-loop sleep when idle;
8. canvas-only frames with no UI declaration, tessellation, or buffer upload;
9. canvas drawing underneath without changed canonical pixels;
10. UI, canvas, and presentation timings reported separately.

The next product slice is the layer panel because layer commands, IDs, order,
visibility, opacity, undo, save, and recovery semantics already exist.

## Performance and Correctness Gate

Before adoption, capture on Apollo:

- dependency tree proving exactly one `wgpu`, at version 30;
- cold UI initialization and first glyph/image upload;
- UI logic/layout, tessellation, texture-update, buffer-update, and render
  timing separately;
- cached canvas-only overlay draw timing;
- draw calls, vertices, indices, texture bytes, and buffer bytes;
- idle wake/redraw count over a fixed interval;
- current app-to-submit and GPU p50/p95/max with UI hidden, visible unchanged,
  hovered, and actively manipulated;
- physical pen feel and latency with a visible unchanged panel;
- memory before and after font/image atlas creation.

Correctness checks:

- a consumed UI press never paints;
- a canvas stroke cannot migrate to UI;
- hiding a panel removes its hit regions immediately;
- layer actions call the same command path as keyboard controls;
- resize/DPI changes update visual and hit rectangles together;
- focus loss cancels capture;
- no UI state enters document checkpoints unless it is actual document state;
- surface/device recreation rebuilds disposable UI GPU resources.

Do not accept “the UI is small” as performance evidence. Conversely, do not
reject a library merely because it rebuilds a small logical widget tree. The
measured input-path and GPU cost decides.

## Implementation Record

### Dependency and first overlay, 2026-07-28

- The pinned egui dependency graph contains exactly one `wgpu`, version 30.
- Apollo `cargo check --bin sketchpad` and the binary unit tests pass.
- The first overlay shares Sketchpad's device, queue, surface texture, command
  encoder, and render pass. It does not create a full-window intermediate.
- UI declaration/tessellation and `egui-wgpu` buffer updates run only when the
  UI snapshot or routed UI input is dirty. Canvas-only frames draw the retained
  paint jobs from the already prepared renderer buffers.
- The first custom-painted toolbar exercises typed actions, selected tool
  state, logarithmic brush-diameter control, brush-opacity control,
  linear-to-sRGB color display and selection, undo/redo, and hide behavior.
  Its custom buttons and sliders publish egui widget semantics, and focused
  sliders accept arrow-key adjustment. The color swatch opens a custom-painted
  palette/recent-color surface and dispatches the existing application color
  command.
- The toolbar and color surface retain separate hit rectangles. The adapter
  tests the two regions independently instead of replacing them with their
  bounding union, which would incorrectly consume canvas input in the empty
  space between or beside panels.

This remains an integration scaffold, not a final product layout.

The first runtime attempt exposed an idle-loop integration trap:
`egui_winit::State::on_window_event` reports `repaint: true` for
`WindowEvent::RedrawRequested`. Treating that notification as new UI input and
calling `Window::request_redraw` again produced a self-sustaining loop at
roughly 180 debug frames per second on Apollo. The adapter now excludes
redraw, close, destroy, move, and occlusion notifications from UI
invalidation. A regression test records the redraw rule.

The initial fix was too aggressive in the other direction: it also ignored
egui's explicit root-viewport `repaint_delay`. The first pass can be a sizing
pass with no paint jobs, so the toolbar sometimes stayed blank until unrelated
input arrived. `UiOverlay` now reports egui's next deadline to Sketchpad's
existing `ControlFlow::WaitUntil` scheduler. A due deadline marks only the UI
preparation dirty and requests one redraw. This preserves delayed UI work
without polling.

Egui's default `Area` fade-in requested approximately 24 debug redraws on
Apollo. Sketchpad's fixed tool surface does not need that animation, so it
disables the fade explicitly. The corrected startup performs three egui
settling passes, produces one paint job, and then sleeps; no additional frames
occurred during the remainder of the observed four-second smoke run.

Apollo also reports egui's warning that `Bgra8UnormSrgb` is an sRGB-aware
framebuffer while egui prefers an unorm framebuffer. The pinned renderer
explicitly selects its `fs_main_linear_framebuffer` shader for sRGB formats,
so it handles the conversion rather than blindly using the gamma-framebuffer
path. Do not change Sketchpad's established canvas surface format merely to
silence the warning; evaluate UI blending visually and with a color test
before considering a different view format.

The next slice added direct-tablet UI translation without sending native
pressure through egui. A small capture state machine:

- forwards tablet hover only while entering, crossing, or leaving a UI hit
  region;
- captures a device whose contact begins on UI;
- retains that capture outside the panel through release;
- refuses UI capture while canvas drawing, sampling, or panning owns the
  pointer;
- suppresses the duplicate synthesized mouse path after native tablet input;
- clears capture on focus loss or tablet-backend failure.

Unit tests cover UI-origin capture, canvas-origin ownership, and hover boundary
forwarding. Physical pen activation of each control and slider dragging still
require a hand test.

The wakeup test also exposed an upstream Vulkan correctness bug in the
published `wgpu-hal 30.0.0`. On non-Windows platforms it passed a real fence to
`vkAcquireNextImageKHR`, but the wait/reset logic for that fence was compiled
only on Windows. The second acquisition after idle therefore emitted
`VUID-vkAcquireNextImageKHR-fence-10066` on Apollo.

The wgpu project fixed this exact bug in
[PR 9918](https://github.com/gfx-rs/wgpu/pull/9918) and backported it to the
v30 line as commit `e904d2eac09a9494fb8a453b7e0278fb06e8693c`. Sketchpad
patches the coherent wgpu v30 crate family to that exact official revision;
patching `wgpu-hal` alone is invalid because Rust types from its Git-sourced
`wgpu-types` and Naga dependencies do not match crates.io copies. Cargo-tree
checks prove one `wgpu` and one `wgpu-types`, both revision `e904d2ea`.

Sketchpad also no longer reconfigures the surface while a suboptimal acquired
texture is still alive. It presents that frame and reconfigures afterward.
Before the upstream patch, the idle-wakeup reproducer emitted the fence
validation error on every run. After the patch, Apollo idled for eight seconds
and then processed a 271-sample, 108-present physical stroke without a Vulkan
validation error.

### Prepared-overlay measurement, 2026-07-28

The live metrics now keep UI work separate from canvas and presentation work:

- routed winit and direct-tablet event counts;
- declaration/tessellation count, cache hits, mean, and maximum CPU time;
- texture-delta count;
- `egui-wgpu` buffer-preparation count, cache hits, mean, and maximum CPU time;
- overlay draw-encoding count, mean, and maximum CPU time;
- retained paint-job count.

The first controlled Apollo cache smoke used a debug binary and an automated
11-sample mouse stroke outside the tool strip. During the stroke frame:

- UI declaration/tessellation: `0` calls, `1` cache hit;
- UI texture updates: `0`;
- UI GPU buffer preparation: `0` calls, `1` cache hit;
- retained overlay draw encoding: `17.6 µs`;
- canvas work still processed all 11 samples and uploaded 16 changed tiles.

This proves the intended ownership property: unchanged UI does not put layout,
tessellation, texture upload, or buffer preparation on the canvas-only frame.
It does not yet quantify GPU execution time.

The same test exposed two Linux event-filtering leaks. Raw winit
`AxisMotion` events have no UI position and duplicated the XInput/cursor
streams, while repeated no-op `ModifiersChanged(0)` notifications invalidated
the UI around synthetic clicks. The adapter now ignores raw axis motion and
deduplicates modifier state. Direct tablet position/contact remains the
authoritative pen-to-UI path.

Warm dirty debug passes for this one-control-strip scaffold were roughly
`1.4–1.8 ms` for egui declaration/tessellation, `0.17–0.36 ms` for
`egui-wgpu` buffer preparation, and `14–18 µs` for overlay draw encoding on
Apollo. Cold font/device startup was much larger. These debug smoke numbers
are diagnostic scale indicators, not release benchmarks or adoption results.
The remaining gate still requires release-mode distributions, GPU timestamps
where supported, memory/geometry counts, and a physical pen comparison.

### First layer panel, 2026-07-28

The next product slice projects only layer metadata into the UI:

- stable `LayerId`;
- borrowed name;
- visibility;
- opacity;
- active-layer ID and undo/redo availability.

The projection is created by a closure that `UiOverlay::prepare` invokes only
after its dirty check. Canvas-only frames therefore do not allocate a layer
vector, clone names, walk layers, or rebuild panel geometry. Raster data is
never exposed to the UI.

The first panel dispatches typed actions for layer selection, per-row
visibility, create, duplicate, delete, up/down ordering, and active-layer
opacity. Application methods validate the target ID and call the existing
`Document` commands; the panel has no second layer model and produces the same
undo/recovery behavior as keyboard commands. Undo and redo controls now reflect
real document history availability.

Layer opacity is intentionally a pair of ten-percent step controls in this
slice. Each click is one document edit and one undo entry. A continuous slider
requires an explicit preview/commit or history-coalescing contract; sending
one ordinary `set_layer_opacity` command per drag sample would create dozens
of undo entries and repeated full affected-layer recompositions. Do not add
that slider until this transaction boundary exists.

Toolbar, color-popover, and layer-panel hit rectangles remain disjoint. A
regression test proves that points in the gaps do not become UI-owned merely
because they lie inside the panels' combined bounding box.

The Apollo debug action smoke exercised:

1. layer creation;
2. active-layer opacity change;
3. per-row visibility;
4. undo of all three operations;
5. redo;
6. row selection;
7. duplication;
8. ordering;
9. deletion.

Panel state, active selection, row order, opacity/visibility, undo/redo
availability, and the title all followed the underlying document. The first
run found that recording mode returned from `mark_document_dirty` before
refreshing the title; the document and panel were correct but the title stayed
on the removed layer after undo. The non-persistent path now refreshes the
title before returning, and the repeated three-undo run ended consistently on
`Layer 1 (1/1)`.

A held-contact canvas smoke with two layer rows and the panel visible reported
six input samples, one canvas frame, zero routed UI events, zero UI
declaration/tessellation calls with one cache hit, zero UI buffer preparations
with one cache hit, zero texture updates, and `30.6 µs` of retained overlay
draw encoding. Mouse-up intentionally caused one dirty UI pass because the
committed stroke changed undo availability. This preserves the high-rate
contact path while keeping history controls correct at transaction commit.
These remain debug diagnostics, not release performance claims.

## Custom Fallback

If the egui spike fails, build only the control surface Sketchpad needs:

```text
UiState
  hot / captured / focused ID
  panel visibility and scroll
  last hit regions

UiFrame
  rectangle instances
  image instances
  text areas
  clip batches
  semantic/accessibility nodes
  UiActions
```

Rendering:

- one instanced rounded-rectangle/border pipeline;
- one atlas-backed image/icon pipeline;
- released `glyphon` 0.12 for shaped text inside the existing render pass;
- reusable grow-only vertex/instance buffers;
- draw grouping by clip and texture;
- no per-widget GPU allocation;
- cached draw data for unchanged UI.

`glyphon` is explicitly a `wgpu` 30 text renderer and renders cached glyphs into
an existing render pass
([manifest](https://raw.githubusercontent.com/grovesNL/glyphon/main/Cargo.toml),
[renderer source](https://github.com/grovesNL/glyphon/blob/main/src/text_render.rs),
[official example](https://github.com/grovesNL/glyphon/blob/main/examples/hello-world.rs)).
It does not solve widgets, input, editing, IME, or accessibility.

The custom API should initially expose only:

- anchored panel/overlay;
- row and column layout;
- label;
- button/toggle;
- image/icon;
- slider;
- separator/spacer;
- scrollable list;
- tooltip/popover when a real product control needs it.

Text editing and accessibility are not optional forever. If the custom path
reaches layer rename or editable numeric fields, integrate `winit` IME,
selection/caret/clipboard behavior, keyboard traversal, and an AccessKit
semantic tree deliberately. A bitmap-font button renderer is not a credible
long-term UI foundation.

## Decision Point

Proceed with the embedded pinned-egui vertical slice first. It is the shortest
path to learning whether a mature immediate-mode library fits Sketchpad's
actual tablet, rendering, style, and performance constraints.

Promote it when:

- one `wgpu` 30 device/queue/surface is shared;
- canvas ownership and physical pen latency remain correct;
- unchanged UI does not add repeated layout/upload work to canvas-only frames;
- idle remains asleep;
- appearance can be made intentionally Sketchpad-specific;
- the dependency and upgrade cost is lower than owning the missing widget
  semantics.

The exact-revision dependency gate has promoted the work from research to the
first integration milestone. Egui becomes permanent only after input
ownership, cached canvas-only drawing, idle behavior, custom appearance, and
Apollo latency pass their gates. If those fail, remove the integration and
implement the glyphon-backed custom fallback using the same `UiAction` and
input ownership rules.
