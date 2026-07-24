# Pigment Color Mixing — Realistic Paint Behavior on GPU

## The Problem

In every major painting app (Photoshop, Procreate, CSP, Krita, etc.), blue + yellow = gray. This is wrong. In real life, blue and yellow paint make green.

The reason: these apps represent color as RGB, which models **additive mixing of colored lights** (how your monitor works). Paint, however, mixes via **subsurface scattering** — light enters the paint layer, bounces between pigment particles, gets selectively absorbed and scattered at each wavelength, and whatever's left reflects back to your eye. This is the same physical process as subsurface scattering in 3D rendering.

RGB interpolation goes straight across the color cube. Real pigment mixtures follow curved, non-linear paths through color space. The result: RGB mixing loses saturation, produces mud, and fails to create secondary hues (green, orange, violet). Pigment mixing preserves and even *increases* saturation, produces natural hue shifts, and creates the secondaries you'd expect.

## The Science: Kubelka–Munk Theory

Kubelka & Munk (1931) published the model that predicts color from pigment mixtures. For each wavelength λ, each pigment has:

- **Absorption coefficient** K(λ): how much light the pigment absorbs
- **Scattering coefficient** S(λ): how much light the pigment scatters

For a mixture of pigments with concentrations c₁...cₙ (where Σcᵢ = 1):

```
K_mix(λ) = Σ cᵢ · Kᵢ(λ)
S_mix(λ) = Σ cᵢ · Sᵢ(λ)
```

The reflectance spectrum is then:

```
R(λ) = 1 + K/S − √((K/S)² + 2·K/S)
```

To get sRGB output, you integrate R(λ) with the CIE tristimulus curves, apply the D65 illuminant, and convert XYZ → sRGB.

This is **spectrally accurate** — it operates on 36 wavelength samples (380-750nm at 10nm increments). The K–M model has been known in computer graphics since Haase & Meyer (1992). It accurately reproduces real pigment behavior including hue shifts when mixing with white (e.g., Phthalo Blue shifts from purple to turquoise as white is added — RGB can't do this).

### Why It Wasn't Adopted

Despite 30 years of research, no painting app shipped with K–M until recently. Three reasons:

1. **Channel explosion**: You need to track pigment concentrations (N channels per pixel) or full spectra (36 channels). RGB is 3 channels and everything is built around that.
2. **Performance**: Converting pigment mixture → sRGB requires integrating 36 wavelength samples per pixel. Too slow for interactive painting.
3. **Gamut mismatch**: Real pigment mixtures don't cover the full sRGB gamut. You can't pick arbitrary RGB colors — some can't be represented as pigment mixtures.

## The Solution: Mixbox (Sochorová & Jamriška, SIGGRAPH 2021)

The paper "Practical Pigment Mixing for Digital Painting" solved all three problems with one insight: **a 7D latent space that maps to/from RGB via precomputed lookup tables.**

Paper: [Practical Pigment Mixing for Digital Painting](https://scrtwpns.com/mixbox.pdf)  
Implementation: [github.com/scrtwpns/mixbox](https://github.com/scrtwpns/mixbox)  
Authors: Šárka Sochorová & Ondřej Jamriška, Czech Technical University + Secret Weapons

### The Latent Representation

Instead of tracking full spectra, they define a 7-dimensional latent vector `z`:

```
z = [c₁, c₂, c₃, c₄, r_R, r_G, r_B]
     └── concentrations ──┘  └─ additive residuals ─┘
```

**Concentrations** (c₁...c₄): how much of each of the 4 primary pigments. c₄ = 1 − (c₁ + c₂ + c₃), so only 3 need to be stored.

**Residuals** (r_R, r_G, r_B): the difference between the original RGB color and what the closest pigment mixture can produce. This is the missing-RGB-light component, handled additively.

The default primary pigments:
- Phthalo Blue (PB15:4)
- Quinacridone Magenta (PR122)
- Hansa Yellow (PY73)
- Titanium White (PW6)

### Encoding: RGB → Latent

```
F(RGB) → z:
  c = unmix(RGB)            // solve for concentrations via least-squares
  r = RGB − mix(c)           // residual = whatever K-M can't match
  z = [c, r]
```

`unmix()` is the inverse of the K–M mixing procedure. It's a Newton-optimization problem (`argmin ||mix(c) − RGB||²`) — too slow to run live (100ms per color). The solution: **precompute it for all 256³ 8-bit RGB colors into a 3D LUT (lookup table)**. 48 MB, quantized to 8-bit concentrations.

### Decoding: Latent → RGB

```
G(z) → RGB:
  RGB = mix(c) + r
```

`mix()` also uses a precomputed 3D LUT (another 48 MB). Two trilinear table lookups = blazing fast.

### Linear Operations in Latent Space

The key property: **linear interpolation in latent space behaves like real pigment mixing.**

```
mixbox_lerp(RGB₁, RGB₂, t) = G((1−t)·F(RGB₁) + t·F(RGB₂))
```

Weighted average of N colors: encode each, take weighted sum, decode. This extends to bilinear interpolation (gradients), convolution (smudge/blend brushes), and any other linear color operation.

### The Shader Implementation

The Mixbox GLSL shader uses a **32-term 4th-order polynomial** to evaluate `mix(c)` — this is a polynomial fit to the K–M table, avoiding the LUT texture lookup in the fragment shader path for `mix`. The `unmix` step still uses the LUT texture (`mixbox_lut.png`, 512×512).

```
Encoding (GPU):
  1. Look up c from LUT texture via trilinear interpolation on RGB
  2. Compute r = RGB − eval_polynomial(c)    // polynomial, fast
  3. Return latent = mat3(c, r, 0)

Linear interpolation in latent space:
  latent_mixed = (1−t)·latent₁ + t·latent₂

Decoding (GPU):
  1. RGB = eval_polynomial(c) + r
  2. Clamp to [0,1]
```

Key GLSL function: `mixbox_eval_polynomial(c)` — evaluates the 32-term cubic polynomial that approximates the K–M mixing function. All terms are pre-derived coefficients, making it a series of multiply-adds — extremely GPU-friendly.

### Surrogate Pigments

Real pigment mixtures can produce colors outside sRGB (e.g., Phthalo Blue + Titanium White produces a turquoise too saturated for any monitor). To handle this, the paper computes **surrogate pigments Q*** that are tweaked to stay inside the sRGB cube while being perceptually as close as possible to the real pigments P*. These surrogates are what the LUTs are computed from.

The bias is minimal: histogram of ΔE differences peaks at ~1 (just-noticeable difference). Invisible in practice.

### Performance

Measured against RGB mixing on a recorded painting session (38 minutes, 2915 strokes):

- **Median overhead: 2.3×** compared to plain RGB mixing
- **99th percentile latency: < 16ms** (below 60Hz display refresh)
- **No noticeable lag** during painting

On GPU, the shader version is a few texture lookups + a polynomial evaluation — essentially free per fragment.

## Integration Into Sketchpad's SDF Canvas

Our canvas uses an SDF (signed distance field) to represent geometry. To support pigment mixing, we need **color at every sample alongside distance**.

### Expanded Tile Format

```
Current:  SDF tile = 128×128 × f32    = 64 KB per tile
With color: SDF + Color tile = 128×128 × (f32 + 3×f32) = 256 KB per tile
```

Or alternatively, store latent vectors directly in the tilestore:

```
SDF + Latent tile = 128×128 × (f32 + 3×f32) = 256 KB per tile
```

Storing latent vectors (concentrations + residuals encoded as a `mat3` in GLSL) lets us postpone the decode to display time, which is where color mixing across layers happens.

### Per-Layer Pigment Palette

Each layer could have its own set of primary pigments. Switching pigments changes the mixing behavior. An artist might want:
- Warm palette (Ultramarine Blue, Cadmium Red, Hansa Yellow)
- Cool palette (Phthalo Blue, Quinacridone Magenta, Cadmium Yellow)
- Custom palette loaded from spectral measurements

This requires precomputing separate LUTs per palette. At 96 MB per LUT pair (48 MB encode + 48 MB decode), you could support ~10 palettes in < 1 GB GPU memory. More practically, ship 2-3 default palettes and let users load custom ones.

### Brush Operations

When stamping a brush stroke into the SDF:

1. **Distance**: Compute the brush kernel SDF, `min` into canvas SDF (as before)
2. **Color**: At each tile sample where the brush SDF < 0 (inside the stroke):
   - Get current latent from tile: `z_canvas`
   - Get brush latent: `z_brush = F(brush_color)`
   - Mix based on brush opacity: `z_new = lerp(z_canvas, z_brush, opacity)`
   - Write back to tile

### Smudge / Blend Brush

A smudge brush blends the current canvas color with nearby colors. This is the operation where pigment mixing shines — RGB blends produce gray mud; latent-space blends produce natural transitions.

```
Smudge compute shader:
  for each affected tile sample:
    z_blended = weighted_average of z values in kernel radius
    z_new = lerp(z_current, z_blended, smudge_strength)
    tile[z] = z_new
```

### Display Shader

```
Fragment shader for display:
  for each layer (back to front):
    sdf_value = sample SDF at pixel position
    if sdf_value < 0:  // inside stroke
      z = sample latent at pixel position
      rgb = G(z)  // decode latent → RGB
      composite with layer blend mode
```

### Color Picker Implications

The color picker shows the full RGB gamut. Pick any color — the latent encoding handles it via the residual term. Colors outside the pigment gamut get a non-zero residual, which means they behave more like light than paint when mixed. This is a reasonable compromise: picking a pure RGB blue still works, but mixing it with yellow will be less "paint-like" than mixing a pigment-based blue.

Ideally, the color picker shows the pigment gamut as a subset of the RGB wheel, with a visual indication of "how paintable" a color is (smaller residual = more natural mixing behavior).

## Licensing

Mixbox is released under **CC BY-NC 4.0** — free for non-commercial use, requires a commercial license for shipping in a product. Contact: `mixbox@scrtwpns.com`. The paper's algorithm is published openly; the polynomial coefficients and LUTs are the licensed implementation.

For a fully open-source approach, you could implement the same algorithm from the paper description independently — compute your own K–M coefficients from publicly available pigment databases (e.g., Berns 2016 Artist Paint Spectral Database), generate your own surrogate pigments and LUTs, and fit your own polynomial. The science is public domain; the specific coefficients are the licensed part.

## References

- Sochorová, Š. & Jamriška, O. (2021). "Practical Pigment Mixing for Digital Painting." ACM Trans. Graph. 40(6). [PDF](https://scrtwpns.com/mixbox.pdf)
- Kubelka, P. & Munk, F. (1931). "Ein Beitrag zur Optik der Farbanstriche." Zeitschrift für Technische Physik 12.
- Haase, C.S. & Meyer, G.W. (1992). "Modeling Pigmented Materials for Realistic Image Synthesis." ACM Trans. Graph. 11(4).
- Baxter, W. et al. (2004). "IMPaSTo: A Realistic, Interactive Model for Paint." Proc. NPAR.
- Berns, R.S. (2016). "Artist Paint Spectral Database." Proc. CIC24.
- Mixbox implementation: https://github.com/scrtwpns/mixbox
- Mixbox Rust crate: `mixbox = "2.0.0"` on crates.io
