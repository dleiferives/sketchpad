# A drawing workspace with a visible working set

Research and implementation, 12 September 2026. Primary reference: **Sketchbook** (the former Autodesk product), alongside Procreate, Concepts, Krita and Fresco. This is a structural UI pass, with native renders at [the Atlas gallery](http://localhost:8058/ui-redesign.html).

## What the references actually organize

| Reference | Structure | What we take from it |
| --- | --- | --- |
| [Sketchbook desktop and tablet](https://help.sketchbook.com/docs/basic-ui-elements) | An edge brush palette, toolbar, brush/color pucks, layer editor and optional Lagoon marking menus. Its tablet layout exposes more than its phone layout. | Keep a small brush set visible. Separate frequently used presets from the full library. Place controls around the work. Do not use phone-style concealment just because input is a pen. |
| [Procreate interface](https://help.procreate.com/procreate/handbook/5.0/interface-gestures/interface) | Painting tools, layers and color at the top; persistent size and opacity at the side; advanced settings behind those named destinations. Sidebar placement can change. This cited handbook describes version 5.0. | Size and opacity are core drawing controls. A clean canvas does not require hiding them. Keep current tool and settings legible, with alternate placement for handedness. |
| [Concepts workspace](https://concepts.app/en/manual/workspace) | Eight configurable tool slots in a wheel or bar, current tool properties, separate precision and layer sections, and normal/compact/hidden presentation. | Organize by the artist's working set. Make the active brush an entry into the library. Separate precision work from everyday brush adjustment. |
| [Krita navigation and workspace](https://docs.krita.org/en/user_manual/getting_started/navigation.html) | Brush parameters in the toolbar, task-specific dockers, saved workspaces, and a cursor-local popup palette for brushes and colors. | Visible controls and expert shortcuts can coexist. A near-pointer palette is a promising later accelerator; it should not be the only way to find a brush. |
| [Fresco interface](https://helpx.adobe.com/ca/fresco/desktop/introduction/getting-started-with-user-interface.html) | Tool and layer controls alongside contextual touch shortcuts and gesture access. | An on-screen modifier can serve the second hand when there is no keyboard. Its meaning must be visible for the active tool. |

These are observations from official documentation, not a ranking of market share or measured artist performance. The conclusions in the right column are design judgments for this app.

## Why this arrangement

**Frequent actions deserve direct access.** Progressive disclosure works when advanced or infrequent choices move into secondary views and the entry points clearly describe them. Our old “Tune → More brushes” put a basic material change behind two ambiguous steps. A brush palette and a button bearing the current brush name make that operation discoverable. [NN/g: progressive disclosure](https://www.nngroup.com/articles/progressive-disclosure/)

**Keep state beside the controls that change it.** The artist should see the active brush, color, size, opacity and layer without opening a panel. Names matter: a generic pen icon cannot tell someone whether they selected graphite or a shader package. Selection highlighting should also make Pan and Pick modes apparent. This applies recognition and visible-state principles to the drawing loop. [NN/g: usability heuristics](https://www.nngroup.com/articles/ten-usability-heuristics/)

**Reduce decoration and unnecessary travel without making targets tiny.** Pointing involves a tradeoff between travel distance and target size. A compact default suits a precise mouse or pen; a larger explicit touch mode accommodates a finger. We use 36-point main controls in compact mode and 44-point controls in touch mode. W3C's enhanced target guidance uses 44×44 CSS pixels; it is useful reference material, not a claim that this native application conforms to WCAG. [W3C: enhanced target size](https://www.w3.org/WAI/WCAG22/Understanding/target-size-enhanced)

**Preserve learned gestures.** The keyboard hand is valuable: Shift-drag size, O-drag opacity, Space-pan and Shift+Space-zoom remain available. The visible controls provide the equivalent path without a keyboard. A future marking menu needs stable item positions and a visible way to learn it. Adding another hidden gesture alone would repeat the discoverability problem.

## Implemented structure

- **Compact document strip:** named File entry, direct Save, undo/redo, current layer, Touch/Compact, side switch, help and Focus.
- **Brush strip:** current brush name opens the library directly; color opens its editor; size and opacity sliders remain visible.
- **Brush palette:** Round, Pencil, Marker, Knife, Charcoal and Eraser are one tap away. Brushes opens the full library; Pick and Pan have visible mode state.
- **Flat searchable library:** built-ins and installed shader brushes share one list and one scroll area. Searching matches names and descriptions. Current selection, unavailable GPU brushes, empty search results and shader reload errors have explicit states.
- **Responsive layout:** document and brush strips share the top row at 1200 logical pixels and above; narrower windows use two rows. The palette can move to either side. Color/library/layer scrolling keeps controls within short windows.
- **Quieter styling:** neutral surfaces, smaller margins and rounded corners, a single blue selected state, and unboxed idle buttons. Arrow icons are painted geometry, avoiding missing font glyphs.
- **Focus:** hides the drawing chrome, with a visible Show tools button for restoring it without a keyboard.

The color editor retains saturation/value, hue, presets and recent colors. This pass improves its access and containment; it does not add a new color model or palette-management system. Layer creation, duplication, visibility, rename, order and opacity remain in the directly accessible layer panel.

## Industrial design workflow: the next substantive gaps

Concepts explicitly organizes precision around guides, measurements, scale and snapping. Sketchbook exposes rulers, perspective, symmetry, selection and transforms. Those features support constructing a product and revising proportions, beyond making a brush stroke attractive. [Concepts precision tools](https://concepts.app/en/manual/precision-tools), [Sketchbook tools](https://help.sketchbook.com/docs/tools-in-sketchbook)

A sensible next sequence is:

1. Selection and transform: lasso/rectangle, visible selection state, move/scale/rotate, clear commit/cancel, exact undo. Include moving whole layers and imported references.
2. Better layer manipulation: thumbnails and drag reordering, then lock/alpha-lock where the engine supports it.
3. Construction aids: straightedge, ellipse, perspective and symmetry with visible enabled states and a dedicated compact precision panel.
4. Personal working sets: pin/reorder shader brushes and save placement/density across sessions. The current palette is fixed and layout preferences are session-only.
5. Optional cursor-local palette or off-hand modifier, tested with a real pen and touch device.

These are proposed follow-ups, not controls implemented by this UI change. They should enter the same workspace structure when their document operations work reliably.

## Validation

The UI tests exercise both density modes, mirrored placement, landscape/portrait and 800×600 windows, panel containment with full recent-color history and a long layer list, pointer-only brush selection and slider use, and focus restoration. A hardware GPU fixture renders the actual egui widgets offscreen for visual inspection; its paper and sample document state are a fixture, not a captured user painting.

Physical tablet reach, occlusion, pressure and simultaneous pen/touch feel still need artist testing. The next usability check is a short drawing task: switch pencil/marker, adjust size and opacity, sample color, duplicate/reorder a layer, pan/zoom, undo, save and resume. Observe missed targets and hunting before optimizing further.

Verified on Atlas and Apollo:

- Full ordinary `cargo test --locked` suites passed; the final targeted UI suite passed all 19 tests on each host. Hardware cases remain explicitly opt-in.
- Hardware offscreen UI renders passed on both GPUs. Final Atlas desktop, portrait, touch, brush library/search, color and layer images were inspected and published in the gallery.
- `file_workflow_preserves_pixels_layers_and_failed_open_state` passed on both hosts with `DISPLAY=:0`. Initial SSH runs lacked a display and failed to create X11 windows; no document assertion failed.
- Final `cargo build --locked --release --bin sketchpad` passed on both hosts. Apollo used two Cargo jobs. Running user windows were not restarted.
- Atlas all-target Clippy passed with existing `too_many_arguments` and `while_let_loop` allowances. Remote rustfmt and local diff checks passed.
- Atlas returned HTTP 200 for the gallery and all eight comparison images on port 8058.

Transient logs and renders are in each host's `.artifacts/ui-redesign/`; the review images and research are versioned with the implementation.
