# Pigment Color Mixing Research

Status: first-product brush research. Pigment-like interpolation is a candidate
inside the painterly brush's bounded pickup/deposit reservoir. It is not the
default layer compositor, and it does not determine the geometry architecture.
See [first-usable-product.md](first-usable-product.md).

## 2026 Reservoir and Smudge Findings

The painterly problem is not only which color interpolation formula to use.
Brush-to-canvas exchange, repeated sampling, spatial reservoir resolution, and
edge selection can dominate the result.

Chu et al.'s
[Detail-Preserving Paint Modeling for 3D Brushes](https://www.microsoft.com/en-us/research/publication/detail-preserving-paint-modeling-for-3d-brushes/)
describes smearing as simultaneous bidirectional pickup and deposition. It
identifies two quality/performance failures in repeated exchange: paint that
was just deposited is picked up and resampled again hundreds of times, and a
brush representation sampled at a different resolution from the canvas loses
detail. Its remedies are a canvas snapshot buffer and a 2D,
resolution-matched pickup map under the brush.

This matters even though Sketchpad's first control brush is 2D. Sampling the
live output of every previous dab would bake excessive feedback and
spacing-dependent blur into the reservoir before we have an artistic reason
to want it.

Krita's official
[Color Smudge Brush Engine documentation](https://docs.krita.org/en/reference_manual/brushes/brush_engines/color_smudge_engine.html)
separates color rate from smudge length/radius and reports that spacing affects
trail length and, in dulling mode, effect strength. This is product evidence
that “mix amount” is not one scalar and that a saved corpus must vary spacing.

Jiang et al.'s 2024/2025
[Region-Aware Color Smudging](https://yingjiang96.github.io/projects/smartsmudge/)
shows a different limitation: uniformly smudging every color under a footprint
can destroy intended boundaries or pull unwanted regions into a gradient.
Real-time region selection is valuable later, but it is separate from the
first reservoir and pigment questions.

## First Conventional Control Decision

Before selecting any pigment interpolation, implement and preserve a versioned
linear-RGB control:

- authoritative canvas pixels remain premultiplied linear RGBA;
- pickup reads the active layer only;
- each stroke lazily snapshots only sampled tiles before depositing into them;
- a uniform three-`f32` reservoir starts at the foreground color;
- alpha-weighted footprint sampling produces one straight linear-RGB color and
  a sample-strength scalar;
- pickup exponentially moves the held color toward that sample;
- color rate independently moves it back toward fresh foreground color;
- deposition source-overs the current held color into the live active layer;
- transparent samples do not inject meaningless hidden RGB;
- the reservoir and snapshots die at stroke end; finalized pixels and ordinary
  tile undo remain authoritative.

The exact ordered reservoir update is:

```text
held = lerp(held, sampled, pickup * sample_strength)
held = lerp(held, foreground, color_rate)
```

This is not claimed to be physical pigment mixing. It is the deterministic
control against which spatial tip reservoirs, current-canvas feedback,
perceptual interpolation, and properly licensed pigment-like kernels can be
judged. The uniform reservoir cannot preserve bristle streaks; a later fixed
tip grid is the intended extension if the control proves artistically useful.

## Implemented CPU Reference, 2026-07-27

The first CPU reference now implements that control as `MixingBrushV1` and
`MixingStrokeV1`. It shares the hard-round brush's exact pressure footprint and
distance resampler. For every emitted dab it:

1. enumerates only footprint-intersecting sparse tiles;
2. lazily records each tile's pre-stroke pixels, or a zero-byte transparent
   sentinel when the tile did not exist;
3. computes coverage- and alpha-weighted straight linear RGB plus mean covered
   alpha as sample strength;
4. advances the uniform reservoir in the declared pickup-then-color-rate
   order;
5. source-overs the held color through the same analytic round coverage into
   the live layer.

The snapshot payload is deliberately simple: one full `f32` RGBA tile for each
preexisting tile sampled by the stroke. This can coexist with the raster
transaction's undo before-image, so it is a bounded but potentially duplicated
temporary cost. Transparent tiles use a marker rather than allocating a pixel
array. `MixingStats` exposes dabs, tile visits, coverage-positive sampled and
deposited pixels, unique snapshot tiles, and snapshot payload bytes. This
reference shape makes the cost visible; it does not prejudge whether a measured
optimization should share undo storage or use smaller fixed blocks.

The controlled tests establish:

- transparent pickup is pixel-exact to the ordinary hard-round brush;
- overlapping dabs continue to sample stable pre-stroke blue pixels rather
  than their own newly deposited color;
- collinear input batching preserves pixels, reservoir state, and counters;
- cancel and undo restore exact pre-stroke pixels;
- a four-tile footprint over one preexisting 64×64 tile reports four snapshot
  entries but exactly 65,536 snapshot payload bytes.

These are correctness and work-accounting findings, not throughput results.
The next evidence step is a deterministic mixing corpus and Apollo release
measurement against the hard-round control.

### Deterministic performance corpus

`mixing_bench` defines corpus version 1: one 256-input, pressure-varying,
four-cycle stroke on a 2048×2048 canvas with 128×128 tiles. It runs the ordinary
hard-round and linear-mixing engines over both transparency and a deterministic
opaque six-color swatch field. The four initial/result raster checksums are
saved as compile-time golden values. Every first and warm run must also agree
on the complete raster and mixing work counters, and undo must restore the
golden initial checksum.

```text
scripts/apollo run cargo run --locked --release --bin mixing_bench -- \
  --warm-runs 7
```

The JSON Lines output separates first and warm latency distributions and
reports transaction lookups, undo snapshots, conservative pixel bounds,
mixing sample/deposit pixels, unique pickup snapshots, and pickup payload
bytes. Timing excludes deterministic scene construction and post-stroke
checksum/undo verification.

The first controlled Apollo attempt on 2026-07-27 was rejected before
measurement. Preflight found Java/Xic, Syncthing, ActivityWatch, FluidSynth,
and RustDesk, with only 85% CPU idle. The user-level applications were stopped,
but the root `rustdesk.service` immediately respawned its processes and needs a
fresh sudo authorization to stop. Preflight continued to fail, so the
exploratory one-warm-run output is intentionally not a baseline and no timing
decision was accepted. The next controlled run starts with:

```text
sudo systemctl stop rustdesk.service
scripts/apollo run scripts/benchmark-preflight
```

### Temporary application control

The live application exposes the reference with `M`, which toggles pen strokes
between ordinary hard-round paint and `LinearMixingV1`. The Wacom eraser and
mouse eraser remain destination-out hard-round tools. The mixing recipe reuses
the current pen's color, diameter, opacity, and relative spacing; its first
fixed control parameters are pickup `0.65` and color rate `0.08`. The window
title identifies `Pen`, `Mix`, or `Eraser`, and a completed mixing stroke logs
its work counters and final held linear RGB.

This keyboard toggle is an evaluation surface, not the future brush editor or
a commitment to these parameter defaults.

## Problem Being Addressed

Straight interpolation between two RGB triples models a path through an RGB
color space, not the spectral absorption and scattering of physical paint. It
can produce desaturated or artistically unexpected mixtures. Pigment models can
produce more familiar secondary hues and nonlinear hue/value changes.

This does **not** mean every RGB painting operation is wrong. Display
compositing, geometric coverage, transparent layers, wet paint, and palette
mixing are different operations with different desired math.

## Kubelka–Munk Background

Kubelka–Munk models a diffusely scattering paint layer with wavelength-dependent
absorption `K(λ)` and scattering `S(λ)`. For idealized mixtures, pigment
coefficients are combined by concentration and converted to a reflectance
spectrum, then to a display color under an illuminant and observer model.

The model is useful but expensive and awkward for a conventional RGB painting
pipeline:

- spectra or pigment concentrations require more than RGB channels;
- arbitrary RGB colors are not all realizable by a selected pigment palette;
- conversion and inverse fitting are costly;
- layer thickness, substrate, and transparency require additional assumptions.

## Practical Pigment Mixing / Mixbox

The paper [Practical Pigment Mixing for Digital
Painting](https://dcgi.fel.cvut.cz/en/publications/2021/sochorova-tog-pigments/)
is designed to preserve an RGB(A) painting representation while replacing
selected linear color interpolations with a pigment-like operation.

### Latent interpolation

For two RGB colors, the method conceptually:

1. maps each RGB color to a latent value;
2. interpolates latent values;
3. maps the mixture back to RGB.

The latent representation has:

```
four surrogate-pigment concentrations
three additive RGB residuals
```

The concentrations sum to one, so only three are independent. Together with
three residuals, a stored interpolation state still needs six independent
values. Calling this a `vec3` latent value is incorrect.

The residual lets the mapping represent colors outside the gamut of the
surrogate pigment set. Those residual components interpolate additively while
the pigment components follow the fitted pigment model.

### RGB remains the painting representation

The central practical result is RGB-in/RGB-out `kmerp`-style interpolation. The
paper does not require a canvas to store N pigment channels at every pixel. The
public [Mixbox implementation](https://github.com/scrtwpns/mixbox) exposes
operations on ordinary RGB values and uses LUT/polynomial machinery internally.

This corrects the earlier proposal to store a three-channel latent cache and
decode it only during display. Such a cache is neither the complete latent
representation nor required by the paper.

### Licensing

The public Mixbox implementation and assets are offered under CC BY-NC 4.0 for
noncommercial use, with separate commercial licensing. That license is a
product constraint, not a footnote.

An independent implementation based on the scientific literature would still
need a legal review. “The paper is public” does not by itself establish that
reusing fitted coefficients, LUTs, source code, trademarks, or packaged assets
is permitted.

## Where Pigment Mixing Could Apply

### Palette interpolation

Use pigment interpolation for gradients and color selection between paint
colors. This is the least stateful integration and a useful visual experiment.

### Wet brush deposition

This is the first-product priority. When new paint interacts with existing
paint in the same active raster layer,
convert the two RGB colors to the mixing representation, combine according to a
paint/medium amount, update a bounded brush reservoir, and store the resulting
premultiplied color.

This requires more than color interpolation:

- pickup and deposition;
- brush reservoir state;
- opacity/coverage behavior;
- optional bounded wetness/drying state;
- spatial transport only when a later smudge/wet mode requires it.

The first product may bake the result at stroke finalization. It does not
require persistent pigment concentrations or full-canvas fluid state.

### Smudge/blend brush

Replace some RGB averaging inside a smudge kernel with pigment interpolation.
This is close to the paper’s motivating use and does not require pigment-aware
geometry.

### Explicit pigment layer blend

The paper demonstrates alpha-layer mixing that treats both layers as thick wet
paint. This is a deliberate special effect. It should be a named mode because
users normally expect separate layers to composite rather than physically mix.

## What Should Remain Separate

### Coverage and pigment

Coverage answers how strongly a mark affects a sample. Pigment interpolation
answers which color results when two paint colors mix. One does not replace the
other.

### Normal layer compositing

The default layer path should first be specified with:

- a named working RGB color space;
- linear-light versus encoded operations;
- premultiplied alpha;
- Porter-Duff source-over;
- layer opacity, masks, and group behavior.

Pigment interaction can then be inserted at an explicit point.

### Geometry representation

SDF, vector mesh, and raster tiles determine where a mark is and how coverage
is evaluated. None forces pigment mixing, and pigment mixing does not solve
deep zoom or stroke storage.

## Storage Options

### Store the resulting RGB(A)

For a raster paint layer, the simplest authoritative state is premultiplied
RGB(A) tiles after each paint interaction. Undo uses tile snapshots/mementos.
This matches the paper’s practical intent.

Pros:

- conventional image pipeline;
- no persistent high-dimensional pigment state;
- easy display and export.

Cons:

- past paint cannot be re-mixed under changed pigment parameters;
- resolution remains raster;
- nondestructive editing needs additional source/history.

### Store procedural strokes and regenerate

Save stroke input, brush recipe, selected mixing model, seed, and relevant
interaction parameters, then replay into derived RGB tiles.

Pros:

- deterministic rebuild and possible nondestructive editing;
- caches are disposable.

Cons:

- replay can be expensive;
- smudge/wet brushes depend on prior canvas state;
- algorithm/version changes complicate deterministic files.

### Store full persistent latent paint

Persisting pigment concentrations/residuals or spectra could support richer
future simulation, but channel count, filtering, alpha, file size, and export
semantics become substantially harder. The current research does not justify
this choice.

## Questions to Answer Before Integration

1. Which exact user action should look pigment-like: color gradient, wet
   deposition, smudge, or layer blend?
2. Is the canvas paint state ordinary RGB(A), persistent pigments, or replayable
   procedural strokes?
3. What does opacity mean: less paint, thinner layer, partial coverage, or
   transparency?
4. Do separate layers physically mix or normally composite?
5. What happens when paint dries?
6. Is the Mixbox license acceptable for the intended distribution?
7. What open or independently generated alternatives are credible?
8. Which working color space and transfer function surround the operation?
9. How does pigment behavior interact with HDR and wide gamut?
10. What reference mixtures define success beyond a single blue/yellow demo?

## Recommended Research Sequence

1. Specify correct conventional premultiplied-alpha compositing.
2. Define representative brush interactions and expected results.
3. Compare RGB interpolation, perceptual interpolation, and pigment
   interpolation on a controlled swatch set.
4. Test Mixbox only as an isolated RGB-in/RGB-out operation.
5. Evaluate licensing and independent alternatives.
6. Decide whether a wetness/transport model is actually part of the product.
7. Only then choose storage and GPU integration.

## Primary References

- Sochorová and Jamriška,
  [Practical Pigment Mixing for Digital Painting — project
  page](https://dcgi.fel.cvut.cz/en/publications/2021/sochorova-tog-pigments/)
- Sochorová and Jamriška,
  [paper PDF](https://dcgi.fel.cvut.cz/wp-content/wpallimport-dist/publications/pdf/publications-2021-sochorova-tog-pigments-paper.pdf)
- [Mixbox implementation and license](https://github.com/scrtwpns/mixbox)
