# Pigment Color Mixing Research

Status: first-product brush research. Pigment-like interpolation is a candidate
inside the painterly brush's bounded pickup/deposit reservoir. It is not the
default layer compositor, and it does not determine the geometry architecture.
See [first-usable-product.md](first-usable-product.md).

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
