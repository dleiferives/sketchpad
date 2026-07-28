# Sketchpad Notes

The notes are organized by purpose:

- [First usable product](first-usable-product.md) — the current governing
  direction: a fast defined/resizeable canvas, sparse raster tiles, excellent
  input and brushes, layers, undo, recovery, and explicit post-MVP boundaries.
  Color mixing is currently deferred. Start here for what to build.
- [Product feature roadmap](feature-roadmap.md) — the active ordered delivery
  queue for layers, native persistence, image import/export, UI follow-through,
  the deferred mixing gate, and the correctness/performance infrastructure
  shipped with each slice.
- [PNG import and export contract](png-io-contract.md) — the implemented
  bounded static-PNG codec contract: straight/sRGB interchange,
  premultiplied-linear working pixels, centered clipping, atomic streaming
  export, explicit unsupported metadata, and remaining application wiring.
- [Implementation status](implementation-status.md) — what the current
  executable actually does, its controls, automated coverage, known
  limitations, and immediate engineering order.
- [Native UI and dialog boundary](native-ui.md) — the isolated native-dialog
  decision, current wgpu compatibility findings, and invariants for a future
  graphical control surface.
- [Immediate-mode UI overlay architecture](ui-overlay-architecture.md) —
  current `wgpu` 30 toolkit compatibility, the recommended pinned-egui
  integration experiment, input/render ownership, cached repaint policy,
  performance gate, and a glyphon-backed custom fallback.
- [Native tablet input](tablet-input.md) — the implemented Atlas/XInput2 pen
  and eraser path, verified device ranges, normalized event contract,
  correctness coverage, hardware test checklist, and Wayland/Windows/macOS
  portability boundary.
- [Physical pen latency investigation](input-latency-investigation.md) —
  Apollo hover/contact failure, live probe boundaries, interpretation, and
  controlled capture procedure.
- [Active-stroke presentation](active-stroke-presentation.md) — investigation
  of “Immediate only for the current layer,” the current retained/damage
  architecture, portable viewport caching versus true platform front buffers,
  mobile bandwidth risks, and the measurements required before implementation.
- [Research synthesis](research-synthesis.md) — evidence ledger, definitions,
  comparison of architectures, corrected claims, and prioritized research
  agenda for both the first product and later work.
- [Design vision](design-vision.md) — desired artistic/product experience,
  first-product priorities, and later product questions.
- [Architecture](architecture.md) — the selected first sparse-raster path,
  stable system boundaries, and later candidate backends.
- [Renderer selection](renderer-selection.md) — the working decision to own a
  focused `wgpu` render architecture, use Vello as replaceable leverage rather
  than the canvas foundation, and specialize by measured device capability.
- [Performance laboratory](performance-laboratory.md) — deterministic
  document/input/edit/view traces, cache protocols, correctness oracles,
  measurements, regression policy, and the hardware matrix for comparing
  renderers honestly.
- [Performance proof plan](performance-proof-plan.md) — what the current
  Apollo/trace evidence proves, the bit-exact optimization contract, the
  measurement gaps, and the ordered route from work amplification to
  frame-paced rendering and specialized kernels.
- [Performance-aware code](performance-aware-code.md) — the implementation
  doctrine derived from Casey Muratori, data-oriented design, and Rust
  performance guidance: control/pixel planes, contiguous tile kernels,
  allocation policy, instrumentation, and review rules.
- [Scale-space storage](scale-space-storage.md) — comparison of ordinary 2D,
  hierarchical `(level, x, y)`, semantic scale, nested canvases, true 3D, and
  volume-texture storage. Post-MVP research.
- [Modern rendering research](modern-rendering-research.md) — 2023–2026 work on
  analytic curve coverage, sparse vector strips, continuous-density brushes,
  Gaussian primitives, wet simulation, latency, caching, and a prioritized
  experiment plan.
- [Document semantics and storage](document-semantics-and-storage.md) — brush
  graphs, eraser lineage, color contracts, SQLite versus container formats,
  undo, autosave, recovery, and coordinate serialization.
- [Signed-distance operations](sdf-operations.md) — what SDF/ADF techniques do
  and do not represent, with open construction/update questions.
- [GPU stroke rendering](gpu-stroke-rendering.md) — detailed summary and scope
  of the Levien/Uguray stroke-expansion paper.
- [Pigment mixing](pigment-mixing.md) — corrected Mixbox/pigment findings,
  possible product semantics, licensing, and research sequence.

## Documentation Rule

New claims should be labeled or written so the reader can distinguish:

- verified facts from a primary source or the prototype;
- design inferences;
- working hypotheses that need measurement;
- actual Sketchpad decisions.

When later research changes a conclusion, update the subject note and the claim
correction/research agenda in
[research-synthesis.md](research-synthesis.md) together.
