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

### Controlled Apollo codec baseline, 2026-07-27

`png_bench` generates four deterministic sRGB RGBA8 sources outside the timed
region, then measures one first and seven warm in-memory imports and exports
per case. Every warm operation must retain the same checksum, and every
decode→export→decode must reproduce the exact canonical `f32` raster checksum.
The two independent release processes below used 2048×2048 images and an
otherwise interactive Apollo environment; these are codec baselines, not
isolated whole-application latency claims.

Warm median ranges across the two processes:

| Case | Source / output bytes | Tiles | Import | Export |
| --- | ---: | ---: | ---: | ---: |
| transparent | 21,154 | 0 | 95.3–99.1 ms | 58.6–65.4 ms |
| sparse center | 21,259 | 4 | 99.9–101.0 ms | 65.4–66.9 ms |
| opaque gradient | 176,364 | 256 | 378.1–380.5 ms | 546.9–566.3 ms |
| translucent noise | 16,785,076 | 256 | 381.3–383.1 ms | 1,120.9–1,123.5 ms |

All four cases decoded 16,777,216 source bytes and placed 4,194,304 pixels.
Checksums and encoded byte counts were identical across every repetition and
both processes.

Interpretation:

- Transparent and sparse imports still pay whole-frame inflate and placement
  traversal, but sparse canonicalization prevents dense `f32` tile storage.
- Dense import adds about 280 ms over transparent input. The current RGBA8
  loop evaluates the same sRGB transfer for repeated byte values, making a
  256-entry exact conversion table the first optimization to test.
- Export cost is both per-pixel conversion and compression dependent. The
  high-entropy translucent source roughly doubles gradient export time; that
  is not evidence that sparse lookup or compositing is responsible.
- The full-frame decoder uses 16 MiB here and 64 MiB at the current 4096²
  canvas. Streaming would reduce peak temporary memory, but this bounded,
  off-stroke operation does not justify delaying color-mixing work. Revisit
  row/tile streaming before raising import limits or targeting tighter mobile
  memory budgets.

Engineering consequence: retain the bounded whole-frame decoder for the
first product and do not spend the painterly-brush schedule on a streaming
decoder yet.

### Exact RGBA8 transfer lookup result, 2026-07-27

Change: build a 256-entry `f32` table once per import using the same reference
sRGB function, then index it for 8-bit color samples. Sixteen-bit input keeps
the continuous transfer calculation. A unit test compares every table entry's
bits with the reference function.

The identical two-process 2048²/seven-warm-run protocol produced:

| Case | Before import median | After import median | Improvement |
| --- | ---: | ---: | ---: |
| transparent | 95.3–99.1 ms | 57.8–58.0 ms | 1.64–1.71× |
| sparse center | 99.9–101.0 ms | 56.7–58.2 ms | 1.72–1.78× |
| opaque gradient | 378.1–380.5 ms | 101.1–102.7 ms | 3.68–3.77× |
| translucent noise | 381.3–383.1 ms | 97.0–98.8 ms | 3.86–3.95× |

All raster checksums, encoded PNG checksums, encoded byte counts, allocated
tile counts, and decoded-byte counts remained identical to the baseline.
Export timings remained in the prior ranges, which is expected because the
change touches import only.

Interpretation: repeated nonlinear transfer evaluation, not allocation alone,
was the dominant dense RGBA8 import cost. The optimization removes redundant
work while preserving the exact declared transfer and full-precision internal
pixels. Further import tuning is not currently justified; the remaining
~57–103 ms at 2048² is an off-stroke operation and the memory bound remains
explicit.

Still required:

- import as one document-level semantic undo command;
- static indexed and 16-bit golden fixtures;
- graphical open/import/export path selection;
- eventual ICC/CICP support through a deliberate color-management dependency,
  not handwritten partial profile parsing.
