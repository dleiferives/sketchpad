# Sketchpad Notes

The notes are organized by purpose:

- [First usable product](first-usable-product.md) — the current governing
  direction: a fast defined/resizeable canvas, sparse raster tiles, excellent
  input and brushes, brush-local color mixing, layers, undo, recovery, and
  explicit post-MVP boundaries. Start here for what to build.
- [Implementation status](implementation-status.md) — what the current
  executable actually does, its controls, automated coverage, known
  limitations, and immediate engineering order.
- [Native tablet input](tablet-input.md) — the implemented Atlas/XInput2 pen
  and eraser path, verified device ranges, normalized event contract,
  correctness coverage, hardware test checklist, and Wayland/Windows/macOS
  portability boundary.
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
