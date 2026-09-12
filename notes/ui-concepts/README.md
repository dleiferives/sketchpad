# Compact drawing workspace

Selected direction: a mirrored tool rail and brush puck, with both keyboard-assisted and screen-only operation. `research.html` contains the official references and ergonomic comparison; `index.html` includes the generated concepts and actual Atlas screenshots.

## Controls

| Action | Default keyboard / mouse | On screen |
| --- | --- | --- |
| Ink / eraser | B / E | Ink / Eraser |
| Size | Shift + horizontal drag; [ / ] | Horizontal puck drag; Tune slider and presets |
| Opacity | Hold O + vertical drag; Shift+[ / ] | Vertical puck drag; Tune slider |
| Pan | Hold Space + drag; middle drag; trackpad pixel scroll | Pan, then drag |
| Zoom / fit | Wheel; platform pinch; Home | − / +; tap percentage to fit |
| Sample color | Alt + contact; I selects one-shot picker | Pick, sample, lift |
| Color / layers | C / L | Swatch / Layers |
| Undo / redo | Ctrl/Command+Z / Ctrl/Command+Shift+Z | History buttons |
| Focus / help | Tab or F1 / ? | Focus icon / ?; Show tools restores UI |

The puck locks its axis after six logical pixels of movement. Size doubles per 100 logical pixels; opacity changes by 50 percentage points per 100 vertical pixels. Up increases opacity. Canvas adjustments keep the brush preview anchored at the initial contact, with a fixed orientation and live size feedback. They retain ownership until release, even when the modifier is released first. Escape restores the initial canvas adjustment; cancelling a pen adjustment suppresses the remaining contact until lift.

Touchscreen contacts navigate rather than paint: two fingers pan/pinch, a two-finger tap undoes, and a three-finger tap redoes. History waits for every contact to end, with a 300 ms tap limit and 8 logical pixel movement threshold. Pen strokes suppress the touch sequence. UI contacts remain captured by their original control. Trackpad pixel scrolling pans; pinch depends on events supplied by the platform.

Saved keyboard preferences win over newly introduced defaults. Restore defaults in the help panel's shortcut editor to adopt the complete map. Shift and Alt canvas modifiers are fixed; the listed command bindings are remappable. Rail placement and panel visibility are session state.

## Validation

All project commands run on Atlas via `scripts/atlas run`:

- `cargo test`: 337 tests passed, including 51 application tests.
- `cargo build --release --bin sketchpad`: passes on Atlas.
- `cargo run --bin gpu_resident_bootstrap_smoke`: real Intel GPU regression checks. Added undo/redo presentation coverage for logically blank atlas slots retained by history, plus drawing again after undo. The new check reproduced `ResidentMismatch` before the compositor fix and passes afterward.
- `cargo clippy --bin sketchpad -- -D warnings -A clippy::too_many_arguments`: passes. The allowance is for existing compositor argument-count warnings.
- Remote `rustfmt --check` for changed Rust files and local `git diff --check`.
- Live X11 mouse/keyboard smoke: Shift/O drags, cancellation, both puck axes, Space-pan, help, Tab focus and pointer restoration, mirrored color panel, drawing, erasing and undo. Window-only screenshots are hosted in this directory; transient logs and captures remain under `.artifacts/ui-refresh/`.

Physical pen pressure, simultaneous pen/touch behavior and gesture feel still require a tablet. View rotation, a radial menu, barrel-button mapping, held-E erasing and an off-hand touch modifier remain future options. Natural-media tools remain unavailable in the current GPU-resident engine and are disabled in the brush library.

## View on Atlas

Serve this directory at `http://localhost:8058` on Atlas. The session's server is bound to `127.0.0.1`; PID/log are under `.artifacts/ui-gallery/`. Source sync updates the gallery.
