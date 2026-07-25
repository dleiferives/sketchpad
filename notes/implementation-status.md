# Implementation Status

Status: live implementation ledger, 2026-07-24. This note records what the
current executable actually does. Product intent remains in
[first-usable-product.md](first-usable-product.md); research claims and future
possibilities belong in the subject notes.

## Current Executable

Run on Atlas:

```text
cargo run --release --bin sketchpad
```

The application now uses the sparse raster path rather than the original SDF
prototype:

- 4096×4096 defined canvas;
- 128×128 sparse canonical CPU tiles;
- linear premultiplied `f32` RGBA reference pixels;
- hard round source-over brush;
- distance-based deterministic dab resampling;
- native Atlas/XInput2 pen and eraser device discovery;
- normalized pressure, tilt, physical source ID, tool type, and source
  timestamp delivery;
- mouse fallback at pressure 1;
- pressure-sized pen ink and destination-out erasing;
- GPU brush-footprint cursor that distinguishes pen and eraser and follows
  contact pressure;
- between-stroke keyboard controls for brush size, opacity, mouse
  pen/eraser mode, and six built-in colors;
- one persistent tile transaction from pointer contact to release;
- one before-image per touched tile per gesture;
- exact canvas and per-tile damage rectangles;
- additive nonempty-bound maintenance without commit-time tile rescans;
- subtractive nonempty-bound maintenance with boundary-only shrinking;
- incremental GPU uploads for resident dirty subrectangles;
- lazily paged `Rgba32Float` GPU tile cache, with 256 array layers per page
  and a 1,024-tile ceiling for the current 32×32-tile canvas;
- nearest-sampled instanced visible-tile rendering;
- visible paper boundary and dark pasteboard;
- pan and cursor-centered zoom;
- undo and redo;
- versioned/checksummed sparse-raster recovery checkpoints with atomic
  temporary-file replacement and directory synchronization;
- automatic recovery on startup and autosave two seconds after the last
  committed edit;
- cancellation on focus loss or Escape during an active stroke;
- one-second live mean/p95/max CPU input-handler and frame-submit timing,
  upload traffic, page/capacity/residency, deferred-visible, eviction, and CPU
  tile counters;
- event-loop sleep while no redraw is requested.

Controls:

- left drag: draw;
- Wacom pen contact: pressure-sensitive draw;
- Wacom eraser contact: pressure-sensitive coverage erase;
- middle drag: pan;
- wheel: zoom at cursor;
- `[` / `]`: decrease/increase the hovered tool size;
- Shift-`[` / Shift-`]`: decrease/increase the hovered tool opacity;
- E: toggle mouse pen/eraser mode;
- 1–6: select a built-in pen color;
- Control/Command-Z: undo;
- Control/Command-Shift-Z or Control/Command-Y: redo;
- Control/Command-S: force the recovery checkpoint;
- Control/Command-O: reload the last recovery checkpoint;
- Escape while drawing: cancel the active stroke;
- Escape while idle: exit.

## Ownership Path

```text
window samples
    ↓
HardRoundStroke resampler
    ↓
active RasterLayer gesture
    ├── one before-image on first tile write
    ├── canonical contiguous CPU tile mutation
    └── incremental per-tile damage
                    ↓
           resident dirty-subrect upload
                    ↓
          bounded GPU texture-array cache
                    ↓
             instanced tile draw
```

The CPU raster layer remains canonical. The GPU cache can be reconstructed
from it. The old SDF modules remain in the repository as historical prototype
code but no longer drive the application window.

Native tablet collection is described in
[tablet-input.md](tablet-input.md). It is a platform adapter feeding the same
brush transaction path as mouse input; XInput2 types do not enter the brush,
raster, document, or GPU layers.

## Automated Coverage

### Unit tests

The current test suite covers:

- sparse allocation and absent-tile transparency;
- clipped edge tiles;
- one snapshot per tile per gesture;
- persistent gestures across multiple update calls;
- incremental damage draining;
- exact per-tile damage union;
- cancellation and dropped-guard rollback;
- undo/redo state swapping;
- empty-tile reclamation;
- hard-brush parameter validation;
- immutable brush size/opacity/color adjustment with relative-spacing
  preservation;
- pressure-dependent footprint;
- input range and signed-tilt normalization;
- XInput fixed-point and timestamp-wrap conversion;
- sparse valuator-state merging;
- physical pen/eraser classification;
- duplicate XInput tip-packet rejection;
- deterministic checkpoint encoding and exact sparse-raster round trips;
- checksum, truncation, trailing-data, and atomic replacement rejection/tests;
- destination-out erasing and undo restoration;
- no tile allocation when erasing empty space;
- final stroke caps;
- one undo entry across many input updates;
- independence from collinear event batching;
- camera mapping and view bounds.

### CPU brush replay

`brush_bench` replays the real hard-round paint/erase and transaction paths over
empty and painted content:

```text
cargo run --release --bin brush_bench -- --runs 12
```

The current provisional Atlas result is recorded in
[performance-laboratory.md](performance-laboratory.md).

### GPU smoke replay

`gpu_smoke` runs without a window:

```text
cargo run --release --bin gpu_smoke
```

It:

1. paints a deterministic pressure-varying stroke into sparse CPU tiles;
2. uploads visible tiles through deliberately tiny two-layer GPU pages so the
   replay exercises multi-page binding and drawing;
3. paints a second dot into an already-resident tile;
4. verifies that update uses a dirty subrectangle smaller than a full tile;
5. draws the paper, tile instances, and a visible eraser cursor into an
   offscreen texture;
6. copies the image back to the CPU;
7. verifies that every resident tile is drawn with no deferred visible tiles;
8. fails if enough dark ink and cursor-colored pixels are not present.

On the Atlas Intel UHD 630 it currently finds 15,178 dark ink/outline pixels
and 312 cursor-colored pixels. The resident update transfers 23,104 bytes
rather than the 262,144 bytes required for a complete `f32` RGBA tile. Its
eight visible tiles span four forced two-layer pages.

## Known Limitations

This is an architectural integration checkpoint, not yet the usable painter:

- native tablet input currently supports Atlas/X11 only;
- the physical Wacom event path has only been observed interactively on one
  Atlas setup; a reproducible captured trace is still needed;
- no explicit proximity, twist, side-button, pad, coalesced-history, or
  prediction support;
- tilt and source timestamps are preserved but not yet consumed by the round
  brush;
- no smoothing beyond constant-distance resampling;
- only one layer, one hard-round brush family, six preset colors, and one
  eraser mode;
- no graphical brush/color UI, rotation, selection, or transforms;
- the recovery checkpoint is a single-canvas interim raster snapshot, not the
  future native semantic document; there is no Save As, file dialog, export,
  embedded preview, or migration beyond strict version rejection;
- checkpoint encoding and disk I/O are synchronous after the idle delay and
  still need large-document timing and disk-full/kill testing;
- no color management beyond the stated linear working assumption;
- `f32` RGBA costs 16 bytes per pixel and is explicitly a reference format;
- each lazily allocated 256-tile GPU page reserves 64 MiB in the reference
  format, up to 256 MiB for all 1,024 tiles in the current canvas;
- a future document with more than 1,024 simultaneously visible allocated
  tiles would defer the farther candidates, although that count is now logged;
- nearest tile sampling has no mipmaps, gutters, or zoom-out filtering;
- undo snapshots still copy whole touched tiles;
- arbitrary general edits still use full-tile content-bound rescans;
- brush work is CPU-only;
- repeated dab/tile intersections are not yet coalesced;
- GPU execution, display presentation, and input-to-photon latency are not yet
  instrumented; current live timings end at CPU queue submission.

## Immediate Engineering Order

1. Capture and analyze a real Atlas Wacom trace, including input cadence and
   pressure range.
2. Add GPU timestamps and a presentation/input-to-photon measurement protocol.
3. Decide whether arbitrary mixed/destructive kernels need a stronger
   nonempty-bound structure than the current full-scan fallback.
4. Compare an 8-byte working pixel representation with the `f32` reference.
5. Add Wayland tablet-v2 after the X11 trace is reliable.
6. Add GPU filtering/mip experiments for navigation quality.
7. Add a second textured brush only after hard-ink feel and latency are
   measured.
