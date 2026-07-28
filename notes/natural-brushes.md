# Natural Brush Composition

Status: implementation design, 2026-07-28.

This note defines the first natural-media preset family. The product goal is
not a large collection of cosmetic hard-round presets. It is four brushes with
meaningfully different contact and material behavior, composed from a small,
deterministic, measurable foundation:

- a flat rectangular nib whose contact rotates with pen tilt;
- a graphite pencil with canvas-anchored tooth and side shading;
- a palette knife with fixed cross-blade transfer variation;
- a bristle brush with fixed, separated strands.

The initial implementation is a CPU prototype over the canonical sparse `f32`
raster. It makes the first presets undoable and replayable, but its dab shapes,
spacing, lane model, and pixels are not a permanent visual specification. A
new brush representation may deliberately produce different pixels when it
gives better marks, dynamics, or performance. Every production hot loop must
still be bounded by useful visual work and must not allocate per dab.

There are two distinct correctness classes:

- document operations such as undo, save/load, damage batching, and a backend
  implementation of an already selected brush model require exact state
  transition tests where practical;
- brush-model research requires controlled reference images, difference
  images, continuity and dynamics fixtures, artist evaluation, and performance
  distributions. It does not require reproducing the prototype's checksum.

## Evidence and Consequences

Wacom's ink model reports position, pressure, altitude, and azimuth. Azimuth is
the pen's yaw around the surface normal; altitude is its elevation above the
surface. Wacom's rendering guidance explicitly treats tilt as changing the
contact surface and derives shape, size, and orientation from altitude and
azimuth. It also warns that sensor quality varies by device. Our Linux adapter
already normalizes the two tilt axes, so the brush core receives that
device-independent vector rather than platform events.

The practical consequences are:

- tilt direction controls contact orientation;
- tilt magnitude controls how far a pencil moves from point contact toward
  side contact;
- pressure controls contact/deposition, but does not replace tilt;
- near upright, azimuth is physically underdefined and often noisy, so the
  brush retains its last meaningful orientation;
- mouse input supplies zero tilt and falls back to stroke direction where
  orientation is useful.

### Live Wacom axis calibration

Apollo testing on 2026-07-28 showed that neither using the normalized tilt
vector directly nor the first 90-degree calibration produced the intended
physical contact orientation. The current conversion is retained only to avoid
mixing an unverified input change into the brush-performance work. Before the
orientation is treated as product behavior, add a calibration view that shows
the raw X/Y values, both candidate axes, and an asymmetric labeled tip. Record
the physical lean direction and chosen mapping instead of inferring it from an
unlabeled rectangle.

Sousa and Buchanan's graphite model represents paper as a height field and
models deposition from tip shape, pressure, and hardness. The visual grain
comes from interaction with the paper tooth rather than fresh random noise.
For this app, a deterministic world-coordinate tooth function is the correct
first representation: it cannot swim when samples are replayed, requires no
canvas-sized texture, and gives repeated passes stable graphite buildup.

Paint-brush research commonly computes a two-dimensional contact footprint
from an oriented, deforming brush and then imprints it along a trajectory.
This implementation preserves cross-width detail with fixed lane/strand
strengths. It deliberately does not sample destination pixels, carry pigment,
or mix colors. Those material interactions are a separate deferred system.

Full shallow-water paint height, impasto, and pigment-fluid simulation can
produce richer oil behavior, including on constrained mobile hardware, but
would introduce new canonical material channels and document semantics. It is
not a prerequisite for useful tactile brushes. This phase preserves a clean
upgrade path but does not silently encode pseudo-height into RGBA.

The current pixel-centric dab implementation is now the control for a
continuous-contact replacement, not the planned production architecture. The
research, transient contact commands, visual/performance gates, and ordered
experiment are defined in
[Continuous brush contact and physical paint](continuous-brush-contact.md).
That work is explicitly about opaque/dry contact and future optional
oil/impasto state, not watercolor diffusion.

Primary references:

- Wacom, [Universal Ink Model encoding](https://developer-docs.wacom.com/docs/specifications/uim/encoding/)
- Wacom, [Ink Geometry Pipeline](https://developer-docs.wacom.com/docs/sdk-for-ink/tech/pipeline/)
- Wacom, [Rendering ink](https://developer-docs.wacom.com/docs/sdk-for-ink/guides/rendering/)
- Sousa and Buchanan,
  [Computer-Generated Graphite Pencil Rendering](https://visgraf.impa.br/Courses/npr07/materials/2%20lapis/computer-generated-graphite-pencil%20Rendering%20of%203D.pdf)
- Chu, Baxter, Wei, and Govindaraju,
  [Detail-Preserving Paint Modeling for 3D Brushes](https://www.microsoft.com/en-us/research/wp-content/uploads/2010/06/PaintModel_NPAR_2010.pdf)
- Baxter et al.,
  [A Versatile Interactive 3D Brush Model](https://diglib.eg.org/items/4810e389-c4bd-4e2e-a53b-0d4145a612f8)
- Stuyck, Da, Hadap, and Dutré,
  [Real-Time Oil Painting on Mobile Hardware](https://diglib.eg.org/items/d37a6d8c-1ea9-47f0-a39c-49bddbd67e5f)
- MyPaint,
  [libmypaint brush engine](https://github.com/mypaint/libmypaint)

## Shared Sample and Orientation Model

`BrushSample` contains:

```text
position: canvas-space point
pressure: normalized contact pressure
tilt: normalized signed x/y tilt
```

Distance resampling linearly interpolates all three. It must remain independent
of collinear input event batching.

Each oriented stroke owns an orientation tracker:

1. If tilt magnitude exceeds a small dead zone, normalize the tilt vector and
   use its angle.
2. Otherwise, if movement exceeds a positional dead zone, use movement
   direction.
3. Otherwise retain the prior direction, initialized to a fixed value for
   deterministic taps.
4. Interpolate unit direction vectors and renormalize instead of interpolating
   wrapped scalar angles.

The first shared footprint is an anti-aliased oriented box. It computes a
conservative rotated axis-aligned bound for tile enumeration, transforms each
pixel center into local nib coordinates, and uses the signed distance to a box
for a one-pixel fringe. Later ellipse and lane footprints use the same clipped
damage protocol.

## Brush Recipes

### Flat nib

The flat nib is the foundation and simplest complete brush:

- oriented rectangular contact;
- broad axis follows meaningful pen tilt, then stroke direction;
- pressure changes the short-axis contact thickness without collapsing it to
  zero;
- fixed spacing is derived from the shorter contact dimension;
- source-over deposition uses the selected linear RGB color and opacity.

The cursor must eventually show the oriented contact, but correct ink and
tablet propagation land first. A circular cursor is acceptable only as a
short-lived implementation limitation recorded in the status note.

### Graphite pencil

The pencil has two contact regimes blended continuously by tilt magnitude:

- upright: a small, nearly round point with pressure-sensitive diameter;
- tilted: a longer, wider oriented side-contact patch;
- a fixed canvas-coordinate paper-tooth function modulates deposition;
- pressure increases both contact and graphite transfer;
- repeated passes accumulate through ordinary premultiplied source-over;
- the selected color is honored, allowing graphite-gray and colored pencil.

The tooth function must be deterministic from integer canvas coordinates and a
recipe seed. It is evaluated only for covered pixels. No random generator is
advanced by event count, because that would make replay output depend on input
packet batching.

### Palette knife

The palette knife uses a broad, narrow oriented box subdivided across its width
into a fixed number of lanes. Each lane owns:

```text
strength: fixed deposition multiplier
```

Every lane deposits the selected brush color. It never reads destination
color. Fixed strength differences create cross-blade streaks, while
orientation comes from pen tilt with stroke-direction fallback. The lane count
and deposit table are fixed and inline, so footprint diameter cannot create
unbounded brush state.

This is a paint-transfer model, not a geometric scraper yet. True scraping
requires a canonical paint-height/material layer and is deferred.

### Bristle brush

The bristle brush uses many narrow, separated strand contacts:

- each strand has a stable cross-width offset and deposition strength;
- strand gaps remain visible instead of being filled by one uniform footprint;
- low-amplitude deterministic path-relative wobble avoids a synthetic comb
  while remaining replay exact;
- deposition varies by strand;
- pressure widens the bundle and raises contact.

The first preset uses a bounded fixed strand count. A later brush editor can
expose strand density, stiffness, and grain only after the recipe format is
versioned. Pickup, reload, and pigment mixing belong to the separate deferred
material-system TODO.

## Correctness and Performance Contract

All four brushes must satisfy:

- one raster gesture and one undo entry per pointer contact;
- cancellation restores exact pre-stroke tiles;
- exact deterministic output for a saved input trace;
- no canvas-wide, layer-wide, or allocated-tile-wide scan per dab;
- conservative damage restricted to touched footprint tiles;
- no heap allocation inside the per-pixel or per-dab loop;
- state bounded by a compile-time or validated recipe maximum;
- finite-input validation before raster mutation;
- zero-pressure samples do not deposit;
- output is independent of collinear event batching where the interpolated
  sample field is equivalent.

Add focused unit tests for footprint rotation, tilt interpolation, world-fixed
grain, fixed lane variation, cancellation, and batching. Add replay cases for
upright/tilted pencil, rotating flat nib, knife pull, and separated bristle
marks. Record CPU time, dabs, visited pixels, and changed pixels before
considering a GPU implementation.

## Delivery Slices

1. Extend brush samples and trace-to-app plumbing with tilt; add orientation
   tracking and the oriented box reference kernel.
2. Ship the flat nib and expose it through the existing brush selection
   boundary.
3. Ship the deterministic graphite pencil.
4. Introduce fixed lane deposits and ship the palette knife.
5. Specialize the lane model into separated bristles.
6. [Complete] Replace the temporary selector with a compact preset popover and
   add oriented cursor shapes.
7. Build an authored visual/replay corpus and use Apollo measurements to decide
   which kernels, if any, justify moving to compute shaders.
8. Replace the pixel-centric knife control with the validated continuous
   blade-sweep backend, then reuse that contact boundary for flat, pencil, and
   bounded-strand recipes.

The brush editor remains a later product feature. These presets should first
become coherent enough that an editor would expose useful, stable parameters
rather than leaking implementation accidents.
