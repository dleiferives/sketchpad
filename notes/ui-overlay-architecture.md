# Immediate-Mode UI Overlay Architecture

Status: research recommendation and bounded integration experiment, 2026-07-28.
No UI dependency has been added yet.

## Recommendation

Use an immediate-mode UI over Sketchpad's existing canvas, but do not build a
complete widget system first.

The first implementation should be a time-boxed embedded `egui` experiment:

- pin one exact development revision whose workspace uses `wgpu` 30;
- depend on `egui`, `egui-winit`, and `egui-wgpu`, not `eframe`;
- retain Sketchpad's existing `winit` event loop, `wgpu` instance/device/queue,
  surface, present mode, and command encoder;
- render the canvas first and the UI overlay last in the same surface frame;
- route UI input before canvas input;
- turn widget results into existing application commands rather than allowing
  widgets to own document state;
- preserve event-loop sleep and cache unchanged prepared UI work;
- measure the experiment on Apollo before declaring the toolkit permanent.

If the experiment cannot preserve one `wgpu` version, low pen latency, idle
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

These are reasons to measure a small integration, not reasons to fork the
canvas architecture around egui.

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

## First Vertical Slice

The spike should prove the integration, not design the whole product:

1. a fixed anchored panel with custom colors and spacing;
2. a label and one icon/image;
3. one button that changes an existing session-local setting;
4. one toggle that shows and hides a second rectangle/panel;
5. one slider backed by a brush setting;
6. mouse and direct-pen hover/contact ownership;
7. correct DPI scaling and resize;
8. event-loop sleep when idle;
9. canvas drawing underneath without changed canonical pixels;
10. UI, canvas, and presentation timings reported separately.

The first real product slice after the spike is the layer panel because layer
commands, IDs, order, visibility, opacity, undo, save, and recovery semantics
already exist. Start with select, visibility, add, delete, and reorder. Inline
rename, thumbnails, drag-and-drop polish, and animated transitions can follow.

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

Otherwise remove it and implement the glyphon-backed custom fallback using the
same `UiAction` and input ownership rules.

