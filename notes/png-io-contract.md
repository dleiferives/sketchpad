# PNG Import and Export Contract

Status: implemented codec and temporary application surface, 2026-07-27.
Semantic document undo and graphical dialogs remain to be wired.

This note fixes the color, alpha, placement, failure, and bounded-work
semantics for the first PNG slice. The narrow contract is intentional: an
unsupported color-managed image fails visibly instead of being imported with a
plausible but wrong appearance.

## Source Facts

- PNG stores unassociated (straight), not premultiplied, alpha. Alpha is linear
  even when color samples use a nonlinear transfer function. The PNG
  specification says premultiplied source data must be divided by alpha before
  encoding, with black emitted when alpha is zero.
- PNG color samples are not necessarily proportional to light. Color space can
  be declared by CICP, ICC, sRGB, or a gamma/chromaticity pair, in precedence
  order. An `sRGB` chunk states that samples conform to sRGB.
- The Rust `png` decoder defaults to identity transformations. `EXPAND` is
  required to expand palette, transparency, and sub-8-bit grayscale inputs
  while retaining 16-bit samples. Decoder limits cover decoder-owned
  intermediate allocation, but not the caller-provided output frame.
- The Rust `png` encoder can mark output as sRGB and can stream image data
  without constructing a full uncompressed output frame.

Primary sources:

- [PNG Third Edition: color spaces, alpha representation, and encoder
  guidance](https://www.w3.org/TR/png-3/)
- [`png` 0.18.1 decoder and resource limits](https://docs.rs/png/0.18.1/png/struct.Decoder.html)
- [`png` 0.18.1 transformations](https://docs.rs/png/0.18.1/png/struct.Transformations.html)
- [`png` 0.18.1 streaming writer](https://docs.rs/png/0.18.1/png/struct.Writer.html)
- [`png` 0.18.1 image metadata](https://docs.rs/png/0.18.1/png/struct.Info.html)

## Working Representation

Sketchpad continues to use full-precision `f32` premultiplied linear RGBA.
Import performs these operations in order:

1. expand the PNG into direct grayscale/RGB samples with optional alpha;
2. interpret explicitly sRGB and untagged input as sRGB;
3. apply the exact piecewise sRGB electro-optical transfer function to color
   channels;
4. leave alpha linear;
5. premultiply linear color by alpha.

Export reverses those operations:

1. select the visible premultiplied-linear composite;
2. divide color by nonzero alpha and emit black RGB at zero alpha;
3. apply the exact piecewise linear-to-sRGB transfer function;
4. round to straight-alpha RGBA8;
5. emit an `sRGB` chunk.

RGBA8 export is a declared interchange boundary, not a reduction in native
document quality. Native checkpoints retain exact sparse `f32` pixels.

## First Import Contract

- Accept static grayscale, grayscale-alpha, RGB, RGBA, and indexed PNG after
  decoder expansion, at 1/2/4/8/16-bit source depths supported by PNG.
- Accept an explicit sRGB declaration.
- Treat an image with no color metadata as sRGB and report that assumption in
  `ImportSummary`.
- Reject animation, ICC profiles, CICP/HDR metadata, and gamma/chromaticity
  metadata without sRGB. Future color-management support can widen this
  contract without changing existing results.
- Cap either side at 8,192 pixels, total source pixels at 32 MiPixels, the
  caller-owned decoded frame at 128 MiB, and decoder-owned working memory at
  64 MiB. Validate dimensions and caller allocation before allocating the
  decoded frame.
- Center without resampling. Clip source pixels outside the fixed document
  canvas. Odd differences use Euclidean floor division, making placement
  deterministic for both smaller and larger images.
- Convert directly into sparse destination tiles. Fully transparent tiles are
  reclaimed by canonical raster storage.
- Insert the completed raster as a new active layer only after decode succeeds.
  A decode or geometry failure leaves the document unchanged.

## First Export Contract

- Support explicit full-canvas and exact nontransparent content-bounds regions.
  Content-bounds export rejects an empty composite rather than inventing a
  size.
- Walk sparse tiles by row span, not by per-pixel hash lookup.
- Stream one RGBA8 row to the PNG encoder at a time. Temporary uncompressed
  output storage is therefore proportional to exported width, not area.
- Write to a uniquely created sibling temporary file, flush and synchronize
  it, rename atomically, and synchronize the parent directory on Unix.
- Report exported dimensions, pixel count, final encoded byte count, and let
  the application report elapsed time.

## Tests and Remaining Work

Implemented deterministic tests cover straight-alpha sRGB conversion,
grayscale alpha, centered clipping without resampling, export metadata,
RGBA8 quantization, exact content bounds, truncated input, dimension limits,
and unsupported transfer metadata. Document tests prove successful raster
insertion becomes a distinct active layer and failed geometry validation does
not mutate the document.

### Apollo release smoke, 2026-07-27

Setup: release build on Apollo, no recovery checkpoint, full 4096×4096
transparent document. The first command exported the visible composite. The
second command imported that PNG into a new layer and exported the resulting
visible composite. This is an integration smoke run, not a performance
baseline: it was not isolated or repeated, and the first command included a
fresh release build.

Observed application counters:

- first export: 16,777,216 pixels, 75,598 encoded bytes, 224 ms;
- import: 67,108,864 decoded bytes, 16,777,216 placed pixels, zero allocated
  sparse tiles, 397 ms;
- second export: 16,777,216 pixels, 75,598 encoded bytes, 231 ms;
- both PNG files had the identical SHA-256
  `36149bcbe783a9e6e5858431fd728b550ffa0e93149828d10e5ed74dbb67cd14`.

Interpretation: the no-window CLI traverses recovery, export, decode,
centered placement, layer insertion, compositing, and atomic export
successfully. A fully transparent decoded frame is canonicalized back to zero
CPU tiles, and the static encoder is byte-deterministic for this controlled
case. The 64 MiB decoded-frame allocation also confirms why import needs a
streaming or tile-row decoder experiment before substantially raising current
limits. No default or optimization decision should be derived from the single
timings.

Still required:

- import as one document-level semantic undo command;
- static indexed and 16-bit golden fixtures;
- an Apollo cold/warm import and export timing utility with source bytes,
  decoded bytes, placed/exported pixels, and sparse tile counts;
- graphical open/import/export path selection;
- eventual ICC/CICP support through a deliberate color-management dependency,
  not handwritten partial profile parsing.
