# Native Tablet Input

Status: implemented Atlas/X11 path and portability plan, 2026-07-24.

## Decision

Treat tablet collection as a platform boundary and expose one normalized event
model to brushes. Do not make the brush engine understand XInput2, Wayland,
Windows Ink, Wacom device names, or synthesized mouse events.

The current Atlas path is a narrow XInput2 backend because:

- Atlas runs X11 on `DISPLAY=:0`;
- the X server exposes the Wacom pen and eraser as distinct physical source
  devices;
- winit provides the application loop and mouse fallback but does not expose a
  first-class cross-platform pen event;
- winit device IDs may represent virtual devices that aggregate physical
  sources, as its own
  [`DeviceId` documentation](https://docs.rs/winit/0.30/winit/event/struct.DeviceId.html)
  warns;
- XInput2 motion events retain the physical `sourceid`, sparse valuator values,
  surface-relative position, buttons, and X-server timestamp.

This backend is not a decision to use X11 forever. It is the first concrete
adapter behind a portable input contract.

## Verified Atlas Device Contract

`tablet_probe` queries XInput2 metadata rather than assuming Wacom ranges:

```text
cargo run --bin tablet_probe
```

On the current Atlas setup it finds:

```text
Wacom Intuos Pro S Pen stylus  device 18  tool Pen
Wacom Intuos Pro S Pen eraser  device 19  tool Eraser
```

Both report:

| Axis | XInput label | Declared range | Sketchpad form |
| ---: | --- | ---: | --- |
| 0 | `Abs X` | 0…31,496 | X server maps to window X |
| 1 | `Abs Y` | 0…19,685 | X server maps to window Y |
| 2 | `Abs Pressure` | 0…65,536 | clamped 0…1 |
| 3 | `Abs Tilt X` | −64…63 | signed −1…1 |
| 4 | `Abs Tilt Y` | −64…63 | signed −1…1 |

The stylus also reports an absolute wheel. Pad, touch, puck/cursor, wheel, and
side-button behavior are not part of the first drawing path.

## Current Event Path

```text
XInput2 client connection
    ├── query enabled device classes and valuator ranges
    ├── select motion and tip-button events for the Sketchpad X window
    ├── filter by physical sourceid
    ├── merge sparse valuator packets per physical tool
    ├── normalize pressure and tilt
    └── unwrap 32-bit X-server milliseconds
                         ↓
             winit EventLoopProxy wakeup
                         ↓
TabletEvent { phase, device, tool, position, pressure, tilt, time }
                         ↓
        active HardRoundStroke transaction
                         ↓
       pressure-sized paint or coverage erase
```

The second X connection only subscribes to events for Sketchpad's window. It
does not open `/dev/input`, take an exclusive grab, change Wacom settings, or
replace winit's mouse/keyboard loop.

Each incoming XInput packet is retained as one real sample. Valuator masks can
omit unchanged axes, so the backend carries previous pressure and tilt values
forward. Tip down/up define contact; pressure alone does not define contact.
This follows the general tablet distinction documented by
[libinput](https://wayland.freedesktop.org/libinput/doc/latest/tablet-support.html):
near-zero or offset pressure can exist outside logical contact.

## Tool and Gesture Semantics

- Pen contact selects the hard paint brush.
- The physical eraser end selects a destination-out coverage eraser.
- Pressure changes brush/eraser diameter.
- Tilt and source timestamps are normalized and preserved at the input
  boundary, but the current round brush does not yet consume tilt or time.
- One contact is one raster transaction and one undo entry.
- Focus loss, backend failure, or Escape cancels the active transaction.
- Mouse drawing remains available at pressure 1.
- Wacom's pointer-emulated mouse stream is suppressed briefly after native
  tablet activity; if it races first, the native down event cancels that mouse
  transaction before starting the tablet transaction.
- The window title shows throttled live tool, pressure, and tilt values for the
  first hardware test. Down/up summaries are logged without logging every
  sample.
- Once per active second, the app logs mean/p95/max CPU input-handler and
  frame-submit times plus upload and residency counters. These are internal CPU
  timings, not input-to-photon measurements.

The eraser explicitly reduces premultiplied destination RGBA. Painting
transparent color would not be equivalent. Empty-space erasing skips absent
tiles, while destructive edits of existing tiles retain before-images and
maintain nonempty bounds subtractively so fully erased tiles can be reclaimed.
Interior removals do not scan for new bounds; boundary removals search inward
from the prior exact rectangle.

## Correctness Coverage

Automated tests cover:

- range clamping and asymmetric signed tilt normalization;
- XInput 32.32 fixed-point conversion;
- 32-bit server timestamp wraparound;
- pen/eraser discovery and puck rejection;
- merging sparse axis packets without losing prior pressure;
- pressure-dependent paint footprint;
- destination-out erasing;
- undo restoration after erasing;
- no allocation or history for erasing empty space.

The backend also has a live startup check proving that a second XInput client
can query both Wacom devices, select events, coexist with winit, and coexist
with the Vulkan surface.

The missing proof is an actual physical trace. The next Wacom session must
verify:

1. pen down/move/up delivery;
2. pressure reaching a useful low-to-high range;
3. tilt signs matching physical direction;
4. eraser-end switching;
5. no doubled mouse/native marks;
6. no lost release when leaving the window;
7. sample cadence and timestamp deltas during fast strokes.

## Portability Boundary

The normalized model is intentionally richer than the current brush:

```text
device and tool identity
hover | down | move | up
surface-local position
monotonic/unwrapped source timestamp
pressure
tilt
distance
future twist, tangential pressure, buttons, and provenance
```

Future platform adapters should map native semantics into this model:

- Wayland: the stable
  [`tablet-v2` protocol](https://wayland.app/protocols/tablet-v2) provides
  independent tool objects, explicit proximity/down/up, surface-local motion,
  normalized pressure/distance, physical tilt, and frame grouping. Applications
  should use the compositor protocol rather than consume libinput devices
  directly.
- Windows: Pointer Input provides pressure, rotation, and tilt, and
  [`GetPointerPenInfoHistory`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getpointerpeninfohistory)
  can recover coalesced history rather than accepting only the latest window
  message.
- macOS: AppKit tablet-point events expose location, pressure, tilt, and
  rotation through
  [`tabletPoint(with:)`](https://developer.apple.com/documentation/appkit/nsresponder/tabletpoint%28with%3A%29).

Platform adapters may have different capability sets. Missing values stay
explicit; they must not be fabricated merely to make every backend look alike.

## Immediate Follow-up

1. Record the first real Atlas Wacom trace with raw normalized samples and
   contact boundaries.
2. Add input receipt, brush completion, upload, submit, and presentation
   timestamps to measure the real latency chain.
3. Consume event batches rather than one proxy wakeup per sample if profiling
   shows dispatch overhead or backlog.
4. Validate the new GPU brush cursor against pen hover, contact pressure,
   eraser proximity, zoom, and canvas edges.
5. Decide pressure transfer curves from real use rather than hard-coding a
   stylus-specific curve.
6. Add side buttons and pad mappings only after the drawing path is reliable.
7. Implement Wayland tablet-v2 before claiming general Linux tablet support.
