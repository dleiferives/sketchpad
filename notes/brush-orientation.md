# Directional brush orientation — September 13, 2026

The user narrowed this change to orientation for all brushes. Paint buildup and
scraping interactions are deferred; this change adds no scrape action or mode.
Development and validation for this change run on Apollo only.

## Input behavior

Pencil, alcohol marker, charcoal and palette knife share one tip-orientation
resolver. A clearly tilted pen controls the long tip axis, perpendicular to its
projected azimuth. Screen tilt Y is converted to the document's upward Y axis.
With an upright pen, mouse or touch input, the broad axis follows across the
travel direction. Hard round and the ordinary eraser remain circular.

The resolver retains heading through pauses and pressure-only samples. Movement
smaller than 0.25 document pixels accumulates before changing the heading. Travel
turns use distance-based filtering with a 4px scale, rather than event-count
smoothing. With negligible tilt, a long segment interpolates its tip turn over
the first 4px instead of stretching the turn across the entire segment. Pressure
and real tilted-pen interpolation still use the full segment. This avoids a
wedge on long mouse updates without extra segments or render passes. Pen-tilt control engages at normalized magnitude 0.12 and disengages
below 0.08 to avoid switching repeatedly near the threshold. A tip axis has
180-degree symmetry: a reversal or opposite azimuth does not spin it around or
interpolate through a zero vector. Stationary tilt changes still rotate the tip.

The active stroke is seeded from the hover orientation. The cursor reads the
active renderer's resolved axis. Pressure, footprint size and tilt-dependent
aspect keep their existing behavior. This is orientation of the tip, not rotation
of the paper texture. Stylus barrel roll is not currently supplied by the input
path; the tilt-derived azimuth should not be mistaken for barrel-roll support.

## Rendering and recovery

Each segment instance now carries two resolved tip axes in addition to the raw
pressure and tilt: 96 bytes rather than 80. The package host exposes
`tip_axis_start` and `tip_axis_end`; `media_surface` uses them for directional
footprints. The legacy GPU shader understands the same optional axes. No texture
plane, render pass, simulation state or deposition channel was added.

Scheduling retains orientation across input-batch boundaries and rolls it back
with the scheduler if allocation/encoding fails. Old replay controls retain
their prior interpretation unless orientation is explicitly enabled. Directional
application strokes retain exact pixel recovery, including when using the native
fallback pipeline, so their new orientation cannot be lost by replaying an older
raw-tilt recipe. File formats and already painted pixels do not change.

## Reference distinction

[Procreate's Brush Studio documentation](https://help.procreate.com/procreate/handbook/brushes/brush-studio-settings)
separates stroke-following rotation from azimuth input.
[Sketchbook's stylus tilt documentation](https://help.sketchbook.com/docs/setting-stylus-tilt)
also treats stylus orientation as an input to brush directionality. These support
making the input behavior explicit. The shared fallback, thresholds and filtering
above are Sketchpad implementation choices, not claimed copies of either engine.

## Checks

- Resolver tests cover movement, pauses, reversals, stationary pen rotation,
  subpixel accumulation and invalid inputs.
- Scheduler test compares identical commands delivered in one batch versus
  separate batches, including a turn and stationary tilt change.
- GPU media oracle compares native and package shaders with the CPU equations,
  both with and without resolved orientation, including reversed physical atlas
  slots, small tips and stationary changes.
- Native application test checks pencil, marker, charcoal and knife footprints
  under orthogonal tilt, stationary rotation, matching cursor axes on a mouse
  turn, retained heading during a pressure-only pause, and exact undo/redo.
  Existing material preview, layer and file checks remain in the same fixture.

Native image output is generated beneath `.artifacts/material-engine/` on Apollo.
Apollo validation passed: 302 library tests, the expanded GPU media oracle,
native application checks above, all-target Clippy (with the repository's existing
argument-count/while-let allowances), changed-file rustfmt check and the release
application build. The non-ignored Cargo suite also passed during this change.
The native sheet was inspected to catch and correct long-update rotation wedges.
Synthetic input verifies the software path; physical-tablet feel remains an
artist check. No Atlas commands were run for this change.
