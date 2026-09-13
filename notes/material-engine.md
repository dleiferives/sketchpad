# Material engine: loaded paint now, fuller simulation later

**Work in progress; not ready to ship.** The full 4096px material save still
exceeds the constrained memory gate. All remote builds and tests were stopped
after the user reported memory alerts on both machines.

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

## Validation in progress

The native overlap study now has 0/5,280 transparent sampled interior pixels at
1, 2, 4 and 8 contacts; all 5,280 reach alpha >= 0.99. The four opacity/pressure
rows also have full sampled interior coverage and retained nonuniform thickness.
Atlas and Apollo passed native buildup, cancel, ordinary-brush flattening,
layer duplication/deletion/reordering, exact undo/redo, save/load and identical
continuation after reload. A live 512px contact retains earlier sampled pixels
and matches its committed output. The large 4096px test exposed the device-buffer
limit above; after correction, Atlas passed that stress case in debug as well.
Release verification and final checks are recorded below when complete.

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

## Stopped-run status — September 13, 2026

Atlas systemd confirmed `Result=oom-kill`, `MemoryPeak=3670016000` and
`MemoryMax=3670016000` for the explicitly limited test scope. This is a test
cgroup limit failure, not evidence that all 64 GB of Atlas RAM was exhausted.
The subsequent health check reported about 47 GB available on Atlas and 5 GB on
Apollo. No Sketchpad build/test processes remained on either machine. Further
remote execution was stopped in response to the user's second memory report.

Completed evidence:
- 295 library tests pass after direct raster encoding and composite-free GPU
  checkpoint construction.
- Before the final save/undo changes, native material integration passed on both
  machines; the guarded Apollo integration had no OOM events and a 667,040 KiB
  maximum child RSS including compilation.
- Atlas native opacity/load/overlap studies have zero sampled interior holes.
  The gallery's three native images, comparison/background controls, local links
  and 390px mobile layout passed browser checks.
- Atlas shader reload, mixed-media/file workflows, media-mask oracle and release
  build passed before the final save/undo changes.
- The earlier Atlas large case passed with a 6 GiB test limit, with a 2,744,980 KiB
  maximum child RSS and 6,089,396,224-byte cgroup peak including compilation/cache.
- The 3,500 MiB Atlas gate still fails during saving after the later reductions.
  The full-canvas Apollo retry was stopped by the available-memory guard, before
  a cgroup OOM event, and was not repeated with these later changes.

Still required before shipping: identify the remaining save/recovery peak,
validate the final region-swap undo and checkpoint changes in the native GPU
integration/oracles, rerun relevant checks after any correction, and demonstrate
bounded large-contact/save behavior on Apollo. Do not raise the test cap or
retry shared-machine stress work merely to obtain a passing result. The latest
remote formatting suggestions were applied locally; final format/clippy/native
verification of this work-in-progress snapshot remains outstanding.
