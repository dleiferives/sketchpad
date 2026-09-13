# Pencil, charcoal and knife: second pass

The first versions failed the visual review: isolated pixel holes in the pencil,
a broad blurred charcoal edge, and regular comb stripes in the knife. Hard round
and alcohol marker were accepted and keep their geometry and material formulas.

## Additional research and visual references

David Revoy describes his charcoal-pencil presets as subtle, smooth paper grain
with a tuned pressure curve. I downloaded his CC BY 4.0 bundle and inspected the
actual preset, rather than only reading a feature description. The thin pencil
embeds `10_drawed_dotted.png`, samples it at 0.35 scale, and varies opacity with
pressure. Sketchpad now adapts that paper asset with attribution; its contact
geometry and deposit equations remain our implementation. [Artist’s description
and download](https://www.davidrevoy.com/article326/krita-brushes-charcoal-pencils).

Krita’s illustrated preset overview distinguishes fine pencil lines, broad tilted
shading, and textured dry charcoal. I inspected both the pencil and charcoal
example sheets at full resolution. They provide actual linework and tonal
examples to compare against our output. [Krita preset overview](https://docs.krita.org/en/reference_manual/krita_4_preset_bundle.html).

Faber-Castell demonstrates varying line depth/width with pressure and combining
pencil marks for shading. This motivates checking fine lines and layered patches,
not only giant full-pressure swatches. [Graphite techniques](https://fabercastell.com/blogs/creativity-for-life/graphite-pencil-tips-and-techniques).

Gamblin’s knife guide distinguishes crisp edge marks, bold thick deposits and
broad blocking-in. The prior comb mask represented none of those convincingly.
The revised blade keeps a dense body with irregular scraping at its perimeter
and much subtler interior variation. [Gamblin Studio Knives](https://gamblincolors.com/studio-knives-2/).

## What changed

- **Pencil:** connected graphite deposition instead of scattered binary holes;
  a narrower light-pressure contact, a denser central line and restrained paper
  grain. Tilt still widens the point for side shading.
- **Charcoal:** a dense, grainy contact with a short irregular edge. Removed the
  radius-scaled airbrush falloff and reduced the coarse mottling after inspecting
  the first new render. Light pressure remains a connected light tone.
- **Knife:** a wider loaded blade, a mostly solid deposit, nonperiodic scraped
  edges and subtle internal variation. No pixel-column slots.
- **Paper:** one 512×512 grayscale pattern, bilinear wrapped sampling shared by
  CPU recovery and GPU rendering. Four texels per storage word; 256 KiB of GPU
  storage per mask target. The same document coordinates are used across atlas
  tiles. Full attribution is in the library, gallery and
  [asset provenance](../assets/brushes/README.md).

## Visual and correctness checks

The native app renders a new study sheet: fine curves, fixed-nib pressure ramps,
one/three/six-pass shading patches and overlapping curved hatching. I inspected
both this sheet and the updated five-brush swatches. The gallery provides a
previous/reworked toggle: http://localhost:8058/brushes.html.

The existing app test checks one-contact undo/redo, save/load pixel equality and
512px long strokes for every preset. An optional `SKETCHPAD_BRUSH_BASELINE` points
to the previous saved swatch document and checks the accepted marker/round bands
within 0.00005 float error (shader compilation can change last-bit rounding).
The GPU mask oracle compares CPU/GPU coverage with reversed atlas placement,
ordinary and subpixel tips, changing pressure/tilt, and stationary input updates.
New tests require connected interior tone, a dense charcoal/knife body, and
continuous paper wrapping.

Each contact still uses maximum coverage, so build-up is across separate passes.
These remain dry deposition brushes: solvent blending, smudging and physical
wet-paint transfer are not implemented. The studies exercise synthesized pen
input; physical tablet feel requires the artist’s judgment.

Atlas validation completed: 359 ordinary tests passed, along with explicit native
media/save/history, drawing-study, capacity-limited-contact and 4096px shared-layer
stroke tests. The GPU mask oracle, exact mask clearing and all four mixed-media
history storage controls passed. Release build, Clippy (existing repository lint
allowances) and remote rustfmt checks passed. Apollo SSH timed out on repeated
connection checks, so this revision has not been built or tested there.
