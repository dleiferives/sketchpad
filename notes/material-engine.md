# Material engine: loaded paint now, fuller simulation later

**Implemented and validated on Atlas and Apollo.** The loaded-paint knife now
retains opaque pigment and surface relief. Both machines pass the 4096px material
stroke/save/load/undo/redo stress test under a 3,500 MiB hard test limit, with no
OOM events. This is the first material model; full wet-paint simulation remains
future work.

The [palette-knife research](ui-concepts/palette-knife-research.html) records the
physical references, application precedents, measured repeated-hole defect and
acceptance criteria. The user approved moving to a material engine, with a
possible full-scale simulation later. This note is the implementation contract.

## First slice

Ship one generously loaded opaque knife. Preserve coverage, deposited thickness
and unlit pigment separately. Paint texture should primarily be surface relief;
paper cannot be a permanent zero-coverage stencil. Separate contacts add material
to the surface and cover earlier paint. Keep preview and commit on the same path.

The initial deposition model is a bounded thickness envelope per contact, with
automatic paint replenishment. It is not a mass-conserving wet-fluid solver or a
finite blade reservoir. Its load setting controls deposited thickness, not an
unimplemented simulation of running out of paint. The ordinary opacity control
remains a composition control. No automatic settling after lift.

Use a sparse companion material plane for each drawing layer, outside the Layers
UI. Material and color share atlas allocation, exact history capture, recovery
and document persistence. A material stroke changes both in one undo entry.
Old documents import as flat surfaces. Duplicate/delete/reorder must preserve
the relationship; image export renders appearance rather than material payload.

The material shader contract must be distinct from coverage-only WGSL packages.
Keep existing hard round, marker and other v1 brushes compatible. A programmable
deposition stage and an engine-owned surface stage provide a path to richer
simulation without pretending arbitrary shaders can manage document history.

## Next stages

1. **Contact mechanics:** flexible blade underside; flat, tip and edge contact;
   calibrated pressure/contact mapping. Blade pose is independent of travel.
   Tablet azimuth/roll and mouse angle controls need stable fallbacks and
   distance-based filtering through pauses, turns and storage chunks.
2. **Transport:** spatial paint map on the blade; finite load and explicit refill;
   pickup, deposition and plowing with destinations for displaced paint. Track
   canvas + blade quantities, including refill/cleaning, with conservation tests.
3. **Pigment depth:** bounded wet strata above an immobile substrate. Preserve
   buried colors for later mixing; keep optical mixing separate from transport.
   Two strata are a prototype budget, not an established sufficient limit.
4. **Wet/dry behavior:** separate movable from fixed material. Add explicit
   drying and optional settling only with stable history semantics. Never erase
   unrelated lower drawing layers as an implicit consequence of scraping.
5. **Full simulation investigation:** compare local 2.5D contact/transfer against
   a viscous solver and, if artist-visible benefits justify it, volumetric paint.
   Heightfields cannot represent every fold/overhang. Study non-Newtonian flow,
   yield stress, leveling, blade bending and adhesion against physical samples.
   A full solver is an option, not a promised near-term performance improvement.

Maintain separate coordinate systems for canvas tooth, blade-local paint,
motion-aligned striations and deposited relief. Do not encode highlights into
pigment that later gets picked up. Imported images are flat/dry; selection moves
must carry material; layer merges require an explicit material-stack policy.

## Validation and performance gates

Loaded repeated strokes must fill old gaps; ridge shading must remain visible
with opaque interiors on light and dark backgrounds. Test crossings, curves,
reversals, stationary pressure changes and alternate input subdivisions. A
512-pixel stroke across most of the screen must remain visible throughout and
after lift. Undo, redo, cancellation, save/load and duplicated layers must retain
both material and appearance. Assert exact restore and the same next-stroke
response after reload, not only a similar screenshot.

Measure Atlas and Apollo separately. Sparse allocation, bounded active work,
history backpressure and input-to-visible latency matter more than claiming a
shader is automatically fast. Surface resolution must not depend on view zoom.
Full simulation requires a CPU/reference oracle, quantitative error tolerances,
artist review, and separate performance budgets for contact and settling.

Implementation and validation results are recorded below as they are completed.


## Implemented representation and boundaries

Coverage API v1 remains available. Loaded-paint API v2 returns coverage and a
bounded thickness envelope. The surface stage adds that envelope to the previous
surface, up to 64 document-pixel units. Opacity interpolates both the visible
color and material transaction. It is not a finite-load or mass-conserving model.

The material plane is RGBA32Float: alpha encodes height / 64, RGB encodes unlit
pigment premultiplied by that value. This storage encoding retains the existing
validated four-float tile/undo/archive machinery without treating height as
compositing alpha. The cached color plane stores the shaded visible result.
Changing illumination globally is not yet exposed; future relighting must
regenerate appearance from material rather than blend highlights into pigment.

A companion plane uses the drawing layer's stable ID with its top bit reserved.
Public layer IDs cannot use that bit. Companion keys live in the same sparse
atlas, but never enter the UI stack or normal compositing. Exact recovery groups
color and companion regions under their parent layer while retaining each plane's
identity. Duplicating, reclaiming or saving a layer handles both planes.

Native document version 2 stores a companion raster beside each layer. Files
without material retain version 1 encoding; the loader accepts both versions.
The material encoding above is fixed for version 2. A fuller simulation needs a
new explicitly versioned material payload, not reinterpretation of saved height.
Snapshots and undo retain actual material data; they do not depend on rerunning
an edited shader package. Pixel-only exports flatten the visible appearance.

V1 painting and erasing over material flatten the covered portion proportionally.
They do not allocate new empty companion planes outside existing material. This
is the initial mixed-media rule, not an assertion about physical wet/dry media.

The application requests the adapter's supported undo-buffer limit, up to 1 GiB,
and caps atlas capacity to fit that limit. Color and material both count against
the shared slot budget. This fixes the old 256 MiB default-device limit exposed
by a 512 MiB color-plus-material memento on a fully painted 4096px layer. Limits
are ceilings; allocation remains proportional to touched pages and regions.

## Brush and document validation

The native overlap study now has 0/5,280 transparent sampled interior pixels at
1, 2, 4 and 8 contacts; all 5,280 reach alpha >= 0.99. The four opacity/pressure
rows also have full sampled interior coverage and retained nonuniform thickness.
Atlas and Apollo passed native buildup, cancel, ordinary-brush flattening,
layer duplication/deletion/reordering, exact undo/redo, save/load and identical
continuation after reload. A live 512px contact retains earlier sampled pixels
and matches its committed output. The large 4096px test initially exposed the
device-buffer limit above; the memory corrections and final release validation
are recorded below.

## Apollo memory incident and correction

The initial release 4096px material stress test on Apollo was killed with SIGKILL
while the user reported an application being terminated for excess memory.
Kernel OOM logs were not readable by the SSH account, so the exact victim and
kernel decision could not be confirmed. Further large Apollo tests were paused.

The first implementation unnecessarily retained two full preview atlases, each
indexed by the shared color/material slot address. A full 4096px color/material
surface could require 1 GiB of preview plus a 512 MiB RGBA32F deposition envelope.
The corrected preview uses one atlas containing both outputs at their actual
slots. Compute uses a reusable pair of tile-sized scratch textures and ordered
copies. The deposition envelope uses RG32F: coverage and thickness retain 32-bit
precision while the two unused channels disappear. For 32 occupied 1024px atlas
pages, those temporaries fall from 1,536 MiB to 768 MiB plus 0.5 MiB scratch.
Persistent color, pigment, height and exact history retain their precision.

After commit or cancel, material preview and deposition textures are explicitly
destroyed, including allocations held by cached bind groups. Preview generations
invalidate bindings before another material contact renders. Save encoding now
reserves the header in its output buffer instead of copying the complete payload
again, and releases the encoded color raster before encoding its material mate.
The serialized format and checksums remain unchanged.

These changes reduce waste; they do not make memory unbounded or free. Resident
paint, exact undo, mirror readback, CPU recovery and native file I/O still need
separate budgets. Large remote tests must run inside a memory-limited user cgroup
with swap disabled, so a failed test cannot exhaust the user's whole machine.
Atlas is used first; Apollo receives a conservative hard limit only after that
validation. Peak process RSS is not the same as total GPU/driver/cgroup memory.

The first guarded Apollo retry was stopped by the 2 GiB available-memory reserve
(no cgroup OOM event). Subsequent work also bounds outstanding loaded-mask
submissions to four, preventing synthetic/replayed input from accumulating
unbounded driver and upload work. Every command is preserved; if the GPU falls
behind, submission waits for the oldest pending batch. Pen-up drains the mask
work before allocating the material transaction. The deposition envelope can be
released once its surface pass has been submitted, before the commit snapshot.

Undo/redo now swaps disjoint regions through one reusable region-sized scratch
buffer, instead of allocating a second complete memento. With 128px tiles this
scratch is at most 256 KiB, versus 512 MiB for the full color/material transaction.
All three copies still execute in order for every region; saved undo pixels and
resident-state restoration remain exact.

A stricter Atlas run isolated the remaining peak to native saving. Checkpoint
construction now recovers color/material planes directly without building an
unused flattened composite. Nested raster encodings append directly into the
one document buffer instead of holding another complete encoded layer at once.
The existing checkpoint byte format and checksum validation are preserved.

## Memory investigation — September 13, 2026

An early constrained Atlas run reported `Result=oom-kill`,
`MemoryPeak=3670016000` and `MemoryMax=3670016000`. That was the isolated
3,500 MiB test scope reaching its limit, not evidence that Atlas exhausted all
64 GB of system RAM. Health checks reported about 47 GB available on Atlas and
5 GB on Apollo. Remote work stopped after the user's second memory report and
resumed only after their instruction to continue. The earlier 6 GiB Atlas pass
was insufficient evidence for the lower-memory machine.

The resumed runs keep the 3,500 MiB hard cap and disable test swap. A monitor
stops only its own test process group if system available memory falls below
2 GiB, charged memory excluding reclaimable disk cache exceeds 3,300 MiB, or
runtime exceeds four minutes. MemoryHigh is 3,350 MiB. This is a stress-test
safety boundary, not an application-wide guarantee or an FPS benchmark.

## Continued memory work

Complete immutable interior tiles now share allocation between GPU readback,
CPU mirror, exact recovery, recovered rasters and full-tile before snapshots.
Partial regions and clipped canvas-edge tiles retain their existing copy path;
later edits use copy-on-write. Two regression tests assert shared allocation,
exact replay and isolation of earlier snapshots after a partial edit.

The resumed trace also identified overlap at pen-up: the GPU could still retain
surface/envelope initialization work when the host allocated undo and mirror
buffers. Broad material contacts (at least eight retained mask pages) now drain
the surface submission before ending/releasing the mask and preparing the
transaction. Ordinary small contacts avoid this extra wait. This bounds overlap;
it is not a claim of improved interactive latency or a smaller persistent file.

A new material contact whose entire captured before-state is uninitialized now
stores an implicit blank memento (tile/region metadata, no GPU pixel buffer).
The existing exact-history hydration path materializes its zeros if undo needs
to swap pixels later. This removes a 512 MiB allocation from the initial 4096px
material transaction. History accounts resident pixel bytes separately from its
logical capture size, and zero-byte entries still obey the entry-count bound.

Native file loading now retains one encoded raster plane at a time rather than
an entire encoded material document alongside all decoded planes. The byte and
file APIs share their body parser. Nested checksums and the incremental outer
checksum are validated before publishing a document; trailing or truncated data
is rejected. Legacy raster imports still use their existing decoder and release
the encoded bytes before constructing the composite. A file roundtrip and
corrupt-checksum test exercise this path.

The device requests `MemoryHints::MemoryUsage`. In the pinned wgpu Vulkan
allocator this uses smaller allocation blocks than the default Performance
hint, reducing retained driver-pool memory after a large temporary workload.
This is a backend-dependent hint, not a hard budget or a performance claim.
See the [wgpu API](https://docs.rs/wgpu/latest/wgpu/enum.MemoryHints.html) and
[pinned allocator source](https://github.com/gfx-rs/wgpu/blob/e904d2eac09a9494fb8a453b7e0278fb06e8693c/wgpu-hal/src/lib.rs).

Material preview pages now allocate only occupied page IDs. Holes in the shared
atlas address space bind one tiny dummy texture, rather than allocating an
unused prefix of full-size pages. Preview generations invalidate cached bindings
when a hole becomes occupied. This matters for a small edit after a large drawing.

Implicit blank undo hydration uses an unmapped, zero-initialized GPU buffer,
avoiding another full-size mapped staging allocation. After a submitted redo
returns the material memento to an entirely uninitialized before-state, its GPU
handle is released and history again accounts only the implicit metadata.
Queued GPU work retains its required resources until completion. Atlas pins,
history entry limits and exact initialization flags are preserved.

## Final validation — September 13, 2026

- Atlas: 298 library tests and the complete non-ignored Cargo suite pass.
  All-target Clippy passes with the repository's existing argument-count and
  while-let-loop allowances. Changed-file formatting is checked remotely.
- Atlas native GPU checks: material integration; shader reload with active-contact
  pinning; mixed-media history/files; failed-open preservation and file workflow;
  exact region-swap commit/undo/redo and bounded mirror readback oracle.
- Atlas and Apollo: final release application builds pass.
- Atlas browser gallery: three native images load at their expected sizes,
  comparison/background controls and local links work, CSV measurements match,
  and the 390px mobile viewport has no page overflow.
- Atlas and Apollo: release material integration covers actual document install,
  next-stroke equivalence after native reload, cancellation, duplicated/deleted/
  reordered layers, ordinary-media flattening and live 512px preview consistency.
- Both release large-contact tests pass: one 512px knife contact covers most of a
  4096px document, commits as one history entry, saves, loads through the native
  file codec, undoes, redoes, then accepts an eraser edit. The large fixture loads
  a second CPU document for validation; it does not reinstall that full 4096px
  document into the live GPU owner. Actual install/continuation is covered by the
  smaller material integration fixture.

| Latest guarded release run | Atlas | Apollo |
| --- | ---: | ---: |
| Hard cgroup limit | 3,500 MiB | 3,500 MiB |
| Cgroup peak, including cache/driver charges | 3,631,288,320 bytes | 3,593,580,544 bytes |
| Maximum child RSS | 1,615,024 KiB | 1,573,452 KiB |
| Minimum available system memory | 51,841,384,448 bytes | 2,198,351,872 bytes |
| Guard stopped test / OOM events | no / 0 | no / 0 |

Atlas's scope includes recompilation; Apollo's test binary was already built.
The scope forbids swap and can throttle at MemoryHigh, so these runs are memory
and correctness checks, not comparable performance benchmarks. Apollo's headroom
is limited: passing this fixture does not establish a bound for additional large
layers, arbitrary documents or every full-document replacement workflow. Further
simulation needs its own budgets and tests; keep the system-memory reserve.

The final Apollo pass follows a guard stop during redo (no OOM) that identified
retained blank undo pixels. Neither the hard cap nor the system-memory reserve
was relaxed to obtain the passing result. Native image studies remain available
at `http://localhost:8058/material-engine.html` on Atlas, with the physical research
and fuller-simulation roadmap linked alongside them.

## Orientation follow-up

Directional tip orientation now follows pen azimuth or travel, with retained
heading through pauses and storage batches. The [orientation note](brush-orientation.md)
records the shared pencil/marker/charcoal/knife behavior. The user explicitly
deferred scraping and further material interaction for a later discussion.
