# Drawing media — research and implementation

Shader package authoring and the current recovery path: [shader brushes](shader-brushes.md).

Updated with the second-pass pencil, charcoal and knife work: [research, changes and visual review](brush-rework.md).

12 September 2026

## Research translated into brush behavior

- **Hard round:** preserve the existing continuous, antialiased round sweep, pressure-driven size and per-contact opacity. Krita distinguishes stroke opacity from paint deposited by individual dabs; its wash model avoids dark knots when samples overlap. We use maximum coverage within a contact and apply opacity once. [Krita: opacity and flow](https://docs.krita.org/en/reference_manual/brushes/brush_settings/opacity_and_flow.html).
- **Graphite pencil:** a small upright point, broader elliptical side shading when tilted, fine paper grain, and pressure that fills more of the grain. Procreate specifically describes upright pencil detail versus tilted shading and independent pressure/tilt controls for size and opacity. [Procreate: Brush Studio settings](https://help.procreate.com/procreate/handbook/brushes/brush-studio-settings).
- **Alcohol marker:** a square-ended chisel, relatively stable width under pressure, even translucent coverage and darker overlap between separate passes. Copic demonstrates the broad nib's wide and narrow marks and layered color. Real alcohol can also reactivate existing pigment; this implementation does **not** simulate that solvent behavior. [Copic: using the Classic nibs](https://copicmarkers.com/blogs/educational/intro-classic-2), [Copic: transparent alcohol ink and brush/chisel tips](https://copicmarkers.com/collections/sketch-marker-sets).
- **Palette knife:** a thin, square-ended blade with directional broken ridges and stronger contact under pressure. **Charcoal** is separate: a broad elliptical contact with coarser, irregular grain and softer edges. These are our procedural approximations, not physical paint reservoirs or smudge engines. Krita's texture documentation describes canvas patterns affecting alpha, subtractive tooth, and sensor-controlled texture strength. [Krita: texture](https://docs.krita.org/en/reference_manual/brushes/brush_settings/texture.html).

## Controls and rendering

The library contains Hard round, Graphite pencil, Alcohol marker, Palette knife, Charcoal and Eraser. Diameter defaults are 12, 10, 36, 80 and 48 px respectively. Size and opacity are remembered **per preset during the session**, with the current color shared. Existing Shift size adjustment, opacity controls, keyboard cycling and pen-only library selection continue to work. The cursor reflects each tip's footprint. Mouse input uses full pressure and a fixed useful chisel angle; tablet samples retain pressure and tilt, including changes without movement.

The resident GPU path renders every brush into the same sparse live mask. Round and ellipse sweeps are continuous; chisel sweeps minimize four affine edge distances instead of stamping spaced rectangles. Dry-media grain now uses a licensed 512px paper pattern sampled in document coordinates, independent of atlas slot placement and brush size. A contact uses maximum coverage, so pausing or receiving more identical input packets does not darken it. Lift and make another contact to build up pigment. Marker coverage is intrinsically translucent (about 38–50% before the opacity control).

App paint brushes now use shader packages and exact asynchronous pixel captures for recovery; plugin authors do not implement matching CPU equations. The legacy standalone renderer and replay controls retain pressure/tilt recipes and their CPU oracle. Saves and exact undo retain the existing GPU readback and lossless history paths. The second pass adds one 256 KiB grayscale paper asset (CC BY 4.0), packed into a read-only GPU buffer; no dependency is added. The mask instance grows from 40 to 80 bytes; texture allocation sizes stay unchanged. Brush geometry uses circular conservative bounds, with a three-pixel antialias fringe for anisotropic tips (half a pixel for hard round).

The new presets require the existing blendable float32 resident renderer. If the app falls back to its legacy CPU renderer, the library offers hard round and eraser instead of silently substituting a different brush. Both Atlas and Apollo support the resident path.

## Validation

- Unit tests: dry-media pressure/grain, tilted footprint, opaque round versus translucent marker, and invariance when a straight contact is split into more samples.
- `gpu_media_mask_smoke`: compares all five GPU masks to CPU evaluation over every pixel, with atlas tile allocation deliberately reversed. Exercises changing pressure, tilt, width and tile seams. Floating-point comparison uses a 0.002 maximum tolerance; this is not a claim of bit-identical CPU replay.
- `media_brushes_preserve_pixels_history_and_files`: hidden native window, selects every preset through the app, paints tablet-style and mouse-style swatches, checks exact undo/redo, checks saved pixel equality, exports actual GPU swatches, and paints 512px long contacts with a single undo for every preset.
- `undo_storage_smoke .artifacts/brushes/undo --media`: mixed-media history checked across GPU, compressed RAM, disk and pressure-limited tiers, including cache exhaustion and corruption handling. Exact history remains the authority; the existing experimental checkpoint-and-replay control remains non-exact.

Physical tablet feel still needs an artist's pass on attached hardware; automated tablet-style samples verify the input/render behavior, not ergonomics. Functional runs are not isolated latency benchmarks.

[View the GPU swatches](ui-concepts/brushes.html).

Final Atlas validation: 356 ordinary tests passed; four explicit native GPU tests passed (media, interrupted/capacity-limited contact, full 4096px shared-layer stroke, and file workflow). Release build, Clippy with the repository's existing argument-count/loop allowances, and remote rustfmt checks passed. The mask oracle also covers subpixel tips and stationary tilt/pressure updates. The first-pass maximum mask error was below 0.00004 for ordinary tips and below 0.002 for the subpixel control, including the unchanged hard-round control.

Apollo passed the initial media native test, mixed-media exact history tiers and ordinary-size mask oracle on Intel JSL. SSH became unreachable during the final release/check pass; the final edge and stationary-contact refinements are not yet verified or confirmed built on Apollo. Atlas has the final build. The gallery is served on Atlas at `http://localhost:8058/brushes.html`.
