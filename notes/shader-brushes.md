# Shader brush packages (coverage API v1)

Sketchpad's five built-in paint brushes now use individual WGSL packages. Eraser
and standalone replay controls retain the legacy pipeline. Drop a directory in
`brushes/`, or set `SKETCHPAD_BRUSH_DIR` to an authoring directory before launching.
The built-in packages are also embedded in the executable, so running from another
working directory still provides the standard brushes.

The local checkout remains the editing authority for Atlas/Apollo. Edit packages
locally, then use `scripts/atlas sync`; the running application discovers the
synchronized files. Do not edit the remote source mirror.

## Make a brush

Copy `brushes/stipple/` to `brushes/my-brush/`. Change the manifest's `id` to
`my-brush` (matching the directory name), then edit `brush.wgsl`:

```json
{
  "api_version": 1,
  "id": "my-brush",
  "name": "My brush",
  "description": "A procedural brush",
  "footprint": "constant",
  "extent": 1.0,
  "diameter": 32.0,
  "opacity": 1.0
}
```

```wgsl
fn brush_coverage(b: BrushInput) -> f32 {
    let distance = distance_to_variable_capsule(b.point, b.start, b.end, b.radii);
    let grain = brush_noise(vec2<i32>(floor(b.point / 2.0)));
    return clamp(0.5 - distance, 0.0, 1.0) * (0.5 + 0.5 * grain);
}
```

Find the brush under **Brushes** in the side palette (or click the current brush name), or use the existing next-brush shortcut. A scan runs about once per second.
File reads and shader compilation run on a background worker. Unchanged sources
reuse pipelines; unchanged failed edits reuse diagnostics instead of recompiling.
The library publishes accepted changes between contacts. The shader and recipe
used by an active contact stay fixed through lift, including during reload.
Invalid edits leave the last working package active; the library exposes the
compilation error. New invalid packages are not selectable. Temporarily missing
packages retain their last working version until restart.

## Contract

`BrushInput` provides `point`, `start`, `end` in document pixels; `radii` for the
segment's two endpoints; `pressures` (0–1); and `tilt_start`/`tilt_end` (normalized
X/Y tilt). A stationary contact has equal endpoints. Shapes must handle it.
Coordinates are independent of physical atlas placement.

`footprint: "constant"` reserves diameter × extent / 2 at each endpoint,
regardless of pressure. Use pressure inside the shader to control shape or
coverage. The shader may use the segment's conservative bounding rectangle,
including a 3px fringe. Anything outside that envelope is clipped: increase
`extent` when your design needs more reach. Extent accepts 0.125–4.0. The cursor
shows the reserved footprint, not a traced outline of arbitrary shader shapes.

The `round`, `pencil`, `marker`, `knife`, and `charcoal` footprint profiles retain
the built-in pressure/tilt behavior. New brushes do not need a new Rust profile:
use `constant` and implement dynamics within its reserved envelope.

Coverage is clamped to 0–1, with NaN/negative output treated as zero. Maximum
coverage accumulates within a contact; selected color and opacity are applied
by the engine. Repeated overlap in the same contact doesn't build opacity;
separate contacts do. `brush_noise(cell)` gives stable coordinate-based noise.
`paper_sample(point, scale)` samples the shared David Revoy paper texture, with
[CC BY 4.0 attribution](../assets/brushes/README.md). The host also provides
continuous capsule/box distance helpers and `media_surface` for the built-ins.
See [the host shader](../src/shaders/brush_host.wgsl) for exact signatures.

V1 intentionally has one coverage output and fixed host resources. It does not
expose per-pixel RGB, arbitrary texture bindings, custom sliders, elapsed time,
per-stroke seeds, underlying layer sampling, or persistent simulation state.
Color-mixing, smudge and wet paint will need a richer versioned contract.
WGSL validation catches invalid programs; it does not guarantee a shader is fast.
Avoid expensive loops and excessive per-pixel work, particularly at large sizes.

## Pixels, history and performance

A package stroke uses the existing sparse mask pages, one instanced pass per
page, an 80-byte segment instance, and the existing exact undo/mirror captures.
There is no per-pixel plugin dispatch, added render pass, or shader compilation
on the drawing thread. Package strokes also stop retaining/cloning the CPU
replay command list. Compilation can still compete for driver/CPU resources;
shader complexity and the reserved footprint dominate drawing cost.

The engine records an explicit `AwaitingPixels` revision until its existing
asynchronous mirror capture finishes. Recovery before that point returns an
error instead of substituting hard-round pixels or silently dropping the stroke.
The exact mirror then advances the recovery base and supplies exact history
regions. Save/export, history operations and layer duplication drain the mirror before consuming pixels. Saved
paintings contain pixels and layers and load without any installed brush code.

This is not a guarantee against GPU loss before a readback completes. A shader-only
stroke has no CPU replay fallback in that brief window; previously completed
recovery checkpoints remain available. Nor is it a performance ranking against
other painting apps: the previous implementation was already GPU accelerated.

## Verification

- `gpu_media_mask_smoke`: original and package shaders against the CPU oracle,
  ordinary/subpixel radii, stationary pressure/tilt changes, reversed atlas slots;
  asserts unchanged per-page pass count and instance upload format.
- `shader_packages_reload_without_changing_active_strokes_or_saved_pixels`:
  hidden native window, discovery, 512px custom stroke, edits during a contact,
  invalid WGSL fallback, exact undo/redo, and save/load after removing the package.
- Existing built-in brush, full-canvas contact, history/archive, file and unit
  regressions remain applicable. Functional tests are not isolated benchmarks.

Validation on Atlas (12 September 2026): 361 ordinary tests passed, with seven
hardware-only tests excluded from the ordinary run. Explicit custom-package,
built-in media, full 4096px shared-layer stroke and file workflow tests passed;
the 20-case legacy/package GPU mask comparison and mixed-media exact archive
checks passed. Release build, Clippy (existing argument-count/loop allowances)
and remote rustfmt checks passed.

Apollo follow-up (12 September 2026): release build and all 361 ordinary tests
passed with two Cargo build jobs. Its Intel UHD Graphics (JSL) passed the 20-case
GPU mask comparison, custom-package live reload, built-in media, file workflows,
the full 4096px shared-layer stroke, and exact GPU/RAM/disk/pressure-limited undo
checks. Logs are retained under `.artifacts/shader-brushes/apollo-validation/`
on Apollo and fetched locally.

The first Apollo suite run exposed a test-fixture race: parallel cache tests
could acquire the abandoned session's cleanup lease before the test's own
cleaner, invalidating its immediate-deletion assertion. That test now uses a
private parent directory. All six cache tests passed 20 consecutive runs on
Apollo and a separate Atlas run; Atlas also passed the formatter check.
