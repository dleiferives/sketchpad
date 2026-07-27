# Document Semantics, Erasers, Color, and Storage Research

Status: primary-source research snapshot, 2026-07-24. The first product now
chooses sparse raster tiles as canonical painting state, per-gesture raster
undo, and an active-layer coverage eraser. Retained-stroke lineage remains
post-MVP research. See
[first-usable-product.md](first-usable-product.md).

## Executive Findings

0. **The first painter does not need retained replay for ordinary paint.**
   Finalized painterly marks become sparse canonical raster tile state.
   Per-gesture before-images preserve undo; the native document stores tile and
   layer transactions. Active input and brush state can remain available for
   diagnostics or future features without becoming the display path.

1. **A brush should be durable behavior, not durable tessellation.** Current
   Android Ink APIs model a brush as an expression graph driven by pressure,
   speed, tilt, orientation, time, and distance. A stroke combines immutable
   inputs, a brush, and derived geometry. Sketchpad can use that separation
   without adopting Android's exact API.

2. **Partial erasing changes document lineage.** Windows Ink removes an
   intersected stroke and replaces it with zero or more fragments. Android
   Ink's current partial eraser similarly edits/splits geometry, and its
   release notes explicitly call out missing serialization for those derived
   results. Saving only the cut mesh would make provenance, re-editing, and
   migrations fragile.

3. **Compositing requires an explicit color contract.** Linear-light math,
   premultiplied alpha, artist expectations, HDR, and legacy blend behavior are
   not interchangeable. “RGBA” is insufficient format metadata.

4. **SQLite is a serious native-document candidate.** It supplies transactions,
   incremental updates, partial loading, schema evolution, and crash recovery
   in one file. OpenRaster remains valuable as an interchange format, but its
   flattened PNG layer model cannot preserve every procedural stroke or
   simulation semantic.

5. **Undo, recovery, and caches are different systems.** Semantic history or
   raster mementos preserve edits; an autosave journal preserves recent work;
   derived render caches only improve speed and must be discardable.

## Canonical Mark and Brush Model

The retained model below is a future media option and remains useful for active
input and deterministic brush tests. In the first raster painter, the committed
canonical result is the changed tile set plus layer/document transaction; the
GPU commands and active brush reservoir remain derived.

Android's current
[`BrushBehavior`](https://developer.android.com/reference/androidx/ink/brush/BrushBehavior)
is an expression tree. Its
[source nodes](https://developer.android.com/reference/androidx/ink/brush/behavior/SourceNode.Source)
include pressure, tilt, orientation, direction, elapsed time, distance,
physical speed, brush-size-normalized speed, and predicted time/distance.
Brushes can also contain coats and texture layers in the
[brush package](https://developer.android.com/reference/androidx/ink/brush/package-summary).

The
[`Stroke`](https://developer.android.com/reference/androidx/ink/strokes/Stroke)
type combines an immutable input batch and brush with a derived partitioned
mesh. Its
[`InProgressStroke`](https://developer.android.com/reference/androidx/ink/strokes/InProgressStroke)
accepts incremental real/predicted input and a random seed and can finish
time-varying brush behavior after the final input.

Sketchpad's candidate canonical retained mark therefore contains:

```
Stroke
  stable identity
  ordered-layer position
  real input samples and timestamps
  normalized optional stylus channels
  immutable, versioned brush graph or brush snapshot
  deterministic seed
  document transform
  semantic edits / lineage
  conservative bounds or enough data to recompute them
```

Fitted curves, meshes, strip buffers, curve bands, coverage tiles, and GPU
objects remain derived. If a brush algorithm changes between releases, its
versioned behavior must remain reproducible or be migrated deliberately.

Open questions:

- Are raw samples retained forever, or can a deliberate “simplify/freeze”
  command replace them with fitted geometry?
- Which normalization is performed at input time versus brush evaluation time?
- Are physical units trustworthy across devices?
- Does a transform affect brush width, texture scale, time, and wetness?
- Can brush resources be embedded, content-addressed, or externally linked?

## Eraser Semantics and Lineage

Windows
[`StrokeCollection.Erase`](https://learn.microsoft.com/en-us/dotnet/api/system.windows.ink.strokecollection.erase?view=windowsdesktop-10.0)
defines split erasing by removing an intersected original and replacing it with
zero or more new strokes. AndroidX Ink
[release notes](https://developer.android.com/jetpack/androidx/releases/ink)
describe experimental partial, mesh, and pixel erasers. As of 1.1.0-alpha05
(2026-07-15), cut edges lack antialiasing and erased `PartitionedMesh` results
do not yet have a serialization API.

This is direct evidence that “erase” is not one operation. Sketchpad should
name at least:

- whole-object erase: remove selected logical strokes;
- split erase: replace a stroke with retained fragments;
- coverage erase: paint reduced alpha into raster media;
- mask erase: preserve source and add a nondestructive mask;
- destination-dependent erase: modify wet or simulated state.

Candidate split lineage:

```
original stroke S
  └─ split-erase operation E
       ├─ visible interval S:a
       ├─ visible interval S:b
       └─ erased interval metadata
```

The fragments may share the original input/brush data plus interval boundaries
rather than duplicate every sample. The renderer can derive clipped geometry.
Undo removes `E`; it does not need a cache snapshot of the original mesh.

Questions requiring product decisions:

- Can a split fragment still be selected as part of its parent stroke?
- Does erasing remove the centerline, the swept outline, or visible coverage?
- How do soft edges and translucent self-overlap behave?
- What happens when the source stroke is later transformed or its brush edited?
- Are masks first-class layer objects, per-stroke operations, or both?

Geometry libraries such as
[Skia PathOps](https://api.skia.org/SkPathOps_8h.html) show that curve Boolean
operations are available, but they do not choose these semantics for us.

## Color and Compositing Contract

The
[CSS Color 4 specification](https://www.w3.org/TR/css-color-4/) distinguishes
encoded sRGB from linear-light sRGB and requires premultiplication before
interpolation in many cases. Linear-light values also need enough precision;
Krita's
[color-managed workflow guidance](https://docs.krita.org/en/general_concepts/colors/color_managed_workflow.html)
recommends at least 16-bit precision for linear workflows.

A Sketchpad document needs to name:

- working color space and profile;
- transfer function;
- channel precision;
- premultiplied or straight storage;
- default Porter-Duff operator;
- blend-space rules;
- group isolation behavior;
- mask and coverage interpretation;
- display/output transform.

The portable baseline hypothesis is premultiplied source-over in a named
working space. Whether ordinary brush blending occurs in linear light, encoded
space, or an artist-oriented alternative must be judged visually and against
compatibility needs. Linear RGBA16F is not automatically the answer: it affects
memory, bandwidth, blend appearance, and device support.

Pigment mixing remains an explicit wet-paint or brush operation. It should not
silently replace the layer compositing contract.

## Native Storage Candidates

### SQLite Application File

SQLite's
[application file format guidance](https://www.sqlite.org/appfileformat.html)
argues for a transactional database as an application document: one
cross-platform file, incremental updates, extensible schema, partial loading,
and possible on-disk undo. Its
[atomic commit documentation](https://www.sqlite.org/atomiccommit.html)
describes failure-safe transaction behavior.

A possible logical schema—not a commitment—would separate:

- document metadata and color contract;
- layer tree and ordering keys;
- immutable brush/resource versions;
- stroke inputs and semantic operations;
- raster-tile versions or content-addressed blobs;
- undo/recovery journal;
- optional derived-cache records with a renderer/version key.

Advantages:

- editing one tile or stroke does not rewrite a ZIP archive;
- transactions can atomically update document state and journal;
- indexes support partial viewport loading;
- migrations can be explicit SQL/schema operations;
- derived caches can be dropped without parsing an entire asset package.

Risks and open work:

- untrusted database hardening and limits;
- schema migration tests across old versions;
- compaction and file growth after long editing sessions;
- content-addressed blob deduplication;
- cloud synchronization and merge semantics;
- whether large raster blobs should remain inline;
- atomic “Save As” and portable snapshot behavior.

SQLite's
[WAL documentation](https://www.sqlite.org/wal.html) warns that WAL uses
sidecar files, does not work over network filesystems, and requires
checkpointing. A live WAL database should not be treated as a cloud-sync
format without a deliberate snapshot/checkpoint protocol.

### ZIP/OpenRaster-Style Container

The
[OpenRaster layout](https://www.openraster.org/baseline/file-layout-spec.html)
uses a ZIP container with `stack.xml`, PNG layers, a thumbnail, and a merged
image. Its
[layer-stack specification](https://www.openraster.org/baseline/layer-stack-spec.html)
provides interoperable layer and blend semantics.

This is attractive for:

- export/import with ordinary paint applications;
- recovery via a merged preview;
- human-inspectable package contents;
- a portable frozen snapshot.

It is weaker as the live native document when edits frequently change small
parts of large data or when procedural/simulation semantics do not map to PNG
layers. A hybrid is plausible: SQLite as the native editable document and
OpenRaster/SVG/PNG as explicit interoperability exports.

## Undo, Autosave, and Recovery

The systems should be specified separately:

| System | Durable purpose | Candidate mechanism |
|---|---|---|
| undo/redo | reverse intentional edits | semantic commands; raster tile mementos |
| crash recovery | recover recent committed gestures | transaction journal/autosave log |
| save checkpoint | portable authoritative document | atomic DB transaction or snapshot |
| preview/recovery image | show/salvage appearance | merged raster and thumbnail |
| render cache | reduce regeneration work | discardable versioned blobs |

### First unified in-memory edit sequence

The first layered implementation must not add an independent “layer undo”
stack beside each raster layer. That would lose chronology. For example,
paint-bottom → create-top → paint-top → hide-bottom must undo in exactly that
reverse order regardless of which layer is active when the user presses Undo.

The interim in-memory rule is:

- the document owns one ordered edit sequence;
- a raster entry names the stable layer whose existing tile memento must swap;
- a structural entry carries the reversible state for create, imported-layer
  insertion, duplicate, delete, rename, visibility, opacity, or reorder;
- active-layer selection is navigation and is not an edit;
- undo/redo of a structural command restores its declared before/after active
  layer so selection remains valid;
- every applied/undone command returns exact composite damage; metadata-only
  commands may return an empty damage set while still counting as an edit;
- any new edit clears redo for the entire document, including per-layer raster
  redo entries;
- imported rasters enter with no foreign local undo/redo history;
- layer IDs remain monotonic and are not reused by undo;
- history is bounded to the newest 256 semantic entries; evicting a raster
  entry also evicts its corresponding oldest tile memento, including when a
  later delete command temporarily owns that layer;
- history is session state, not part of the current recovery checkpoint.

This keeps the existing efficient tile swap mechanism for pixels while giving
structural operations the same global order. It is still a count bound rather
than the eventual byte budget. Commands that retain a deleted/imported raster
can differ enormously in size, so later memory accounting must report raster
payload, raster mementos, and structurally retained layers separately before a
byte eviction policy is selected.

The application registers a raster edit immediately after a successful
stroke commit. Registration verifies that exactly one untracked local raster
memento exists; a mismatch is an invariant failure rather than silently
creating corrupt global history.

Krita documents
[autosave and backup behavior](https://docs.krita.org/en/user_manual/autosave.html).
Its
[file FAQ](https://docs.krita.org/en/KritaFAQ.html) notes that KRA is a ZIP
archive and that `mergedimage.png` can sometimes be salvaged. That is useful
precedent for embedding a flattened recovery image even when the canonical
document is richer.

Required failure tests:

- process kill during a gesture;
- process kill during autosave/checkpoint;
- disk full;
- corrupted or missing derived cache;
- old brush version unavailable;
- GPU device loss;
- partial raster-tile write;
- migration failure;
- opening untrusted files with extreme sizes or counts.

The recovery guarantee should say exactly whether the last sample, last
gesture, or last checkpoint may be lost.

### Current interim raster checkpoint

The executable now has a deliberately narrower recovery mechanism while the
native schema remains under research. It is not called the Sketchpad document
format.

- The checkpoint owns only the current defined canvas geometry and canonical
  premultiplied raster pixels.
- Only committed gestures, undo, and redo states mark it dirty. An active
  half-stroke is never serialized.
- Nontransparent pixels are encoded as deterministic ordered row runs inside
  sorted sparse tiles; transparent tile storage is not written.
- A fixed magic, format version, feature flags, payload length, and 64-bit
  checksum guard the payload.
- The decoder bounds file size, canvas dimensions, tile size/count, run
  location/order, allocation arithmetic, finite channels, and the current
  premultiplied `[0, 1]` color contract before accepting state.
- Saving writes a new same-directory temporary file, synchronizes it, renames
  it over the checkpoint, and synchronizes the parent directory on Unix.
- The recovery file lives in the user state directory rather than the source
  checkout, so development rsync cannot delete it.
- Startup corruption or incompatible geometry produces a blank canvas without
  replacing the suspect file. A later intentional edit may create a new
  checkpoint.

The current recovery guarantee is therefore “the most recent successfully
checkpointed committed state.” The application schedules a checkpoint two
seconds after the last committed edit and attempts one on ordinary shutdown.
A process kill during the delay can lose those latest committed gestures.
Encoding and I/O are still synchronous and must be measured on dense content.
The format contains no layers, semantic strokes, brush resources, color
profile, history, preview, or migration machinery, so it must be replaced or
explicitly imported by the eventual native document implementation.

## Coordinate and Serialization Model

Canonical coordinates should not be constrained by WGSL `f32`. Candidate
encodings include:

- `f64` document coordinates serialized directly;
- integer spatial-cell coordinates plus local fixed/floating offsets;
- hierarchical coordinates with explicit scale levels;
- nested local 2D coordinate frames for deliberately deep content.

The choice affects:

- stable ordering and spatial indexes;
- deterministic curve fitting;
- binary equivalence across platforms;
- bounds and damage queries;
- transform accumulation;
- maximum extent and zoom ratio;
- storage size.

GPU geometry should be derived into cell-local or camera-relative `f32`. The
file format stores the authoritative coordinate, not the GPU approximation.

Camera zoom, derived LOD, painter's order, and optional semantic scale should
not share one serialized `z`. Ordinary geometry does not need to store the zoom
at which it was authored. If zoom intentionally reveals a representation or
nested canvas, store that explicit scale behavior and coordinate-frame
relationship separately. See
[scale-space-storage.md](scale-space-storage.md).

## Recommended Research Sequence

1. Write the five representative marks and their edit/erase/transform
   expectations.
2. Specify the minimum canonical stroke and versioned brush graph.
3. Specify whole, split, coverage, and mask erasers with example histories.
4. Choose two candidate color contracts and compare known overlap scenes.
5. Prototype the logical document schema on paper for both SQLite and a ZIP
   container.
6. Walk every semantic action through save, undo, crash recovery, partial
   loading, migration, and cache loss.
7. Decide the coordinate error budget and serialization encoding.
8. Only then freeze a first native file schema.

## Decision Gates

Before a storage implementation is selected:

1. Can the file rebuild all visible content without a renderer-specific cache?
2. Can a brush version reproduce old strokes deterministically?
3. Is each eraser's effect on canonical data specified?
4. Are color, alpha, blending, and group semantics named?
5. Can one gesture commit atomically as one undo/recovery unit?
6. Can large documents load by viewport or layer without full decompression?
7. Is there a recovery image when semantic data is damaged?
8. Are schema migration and hostile-input limits testable?
9. Is cloud/network synchronization explicitly separated from local
   transactional storage?
10. Can every derived cache be deleted without losing the artwork or history?
