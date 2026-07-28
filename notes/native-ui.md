# Native UI and Dialog Boundary

Status: implementation decision, 2026-07-27.

Sketchpad needs a small usable desktop interface without surrendering canvas
input ownership, duplicating the GPU stack, or turning ordinary document
commands into framework-specific state. This note records the first dependency
boundary. It is not a commitment to a permanent visual style.

## File Dialog Decision

Use `rfd` 0.17.2 as an isolated native file-dialog service. Its current
[API](https://docs.rs/rfd/0.17.2/rfd/struct.FileDialog.html) provides open and
save dialogs, extension filters, initial directory/file names, and an explicit
parent through `raw-window-handle` 0.6. On Linux and BSD, the documented default
is an [XDG Desktop Portal backend](https://docs.rs/rfd/latest/rfd/#linux--bsd-backends);
this avoids adding GTK development headers to the build.

The first integration is intentionally synchronous and modal:

- dialogs may open only outside an active stroke or picker gesture;
- the application window is the explicit parent;
- cancellation is a no-op;
- the selected path goes through the same bounded PNG codec and document
  command paths as file drop and command-line I/O;
- save paths without a `.png` suffix are normalized before encoding;
- no dialog state enters the document, undo history, or recovery checkpoint.

If modal event-loop blocking causes a measured tablet-event or compositor
problem, move only the dialog call behind an application user event. Do not
make document decoding or encoding asynchronous merely to compensate for the
dialog API.

## Initial Egui Deferral

The released
[egui 0.35 workspace](https://raw.githubusercontent.com/emilk/egui/0.35.0/Cargo.toml)
pins wgpu 29 and winit 0.30. Sketchpad already owns a wgpu 30 device, queue,
surface, and render schedule. Two wgpu major versions produce different Rust
types and cannot share those objects. Downgrading the canvas renderer to select
a UI toolkit would also invalidate existing performance and exact-rendering
evidence.

Egui itself is explicit that it is actively developed and has breaking API
changes, although it is designed for engine integration and provides useful
panels, widgets, text, and accessibility. Reconsider it when a released version
matches Sketchpad's chosen wgpu version, then measure idle wakeups, UI
tessellation/upload work, binary/dependency cost, and canvas event routing
before adopting it.

### 2026-07-28 compatibility follow-up

The initial released-version decision remains correct: egui 0.35 cannot share
Sketchpad's `wgpu` 30 types. Egui's development workspace has since moved to
`wgpu` 30 while retaining `winit` 0.30. An exact pinned Git revision is now a
viable bounded experiment, though it is not equivalent to a released
dependency.

The working recommendation is to embed only `egui`, `egui-winit`, and
`egui-wgpu` over Sketchpad's existing surface—never hand the application to
`eframe`—and measure it against a glyphon-backed custom fallback. The complete
ownership, caching, input, and promotion conditions are in
[Immediate-mode UI overlay architecture](ui-overlay-architecture.md).

## Thin Custom Overlay Candidate

If a small purpose-built layer/brush/color surface is preferable,
[glyphon 0.12](https://raw.githubusercontent.com/grovesNL/glyphon/main/Cargo.toml)
currently uses wgpu 30 and describes rendering text inside an existing render
pass. That makes it compatible with the present GPU ownership model. It is a
text renderer, not a UI system: Sketchpad would still own layout, hit testing,
focus, keyboard navigation, accessibility, styling, and invalidation.

Vello is not a shortcut for the first control surface. The current
[Vello workspace](https://raw.githubusercontent.com/linebender/vello/main/Cargo.toml)
uses wgpu 29, and its own documentation still describes the renderer as alpha
software. Its vector rendering research remains relevant elsewhere, but it
does not supply application widget semantics.

## Invariants for Any Future Panel

- UI hit testing gets the first chance at pointer input; unconsumed input alone
  may start a canvas stroke.
- A pointer owner cannot migrate between UI, picker, pan, and paint during one
  contact.
- Document mutations still call the existing command methods; widgets do not
  keep a shadow document.
- Color history and panel expansion are session state. Layer order,
  visibility, opacity, names, and pixels remain document state.
- Idle UI must preserve event-loop sleep.
- UI work and canvas work need separate counters before performance claims.
- The canvas renderer remains testable headlessly without constructing UI.

## Immediate Consequence

Ship native PNG import/export dialogs with `rfd` first. Keep the current
keyboard and window-title controls while evaluating the physical tablet. Build
the first graphical layer/brush/color surface as the bounded pinned-egui
vertical slice defined in
[Immediate-mode UI overlay architecture](ui-overlay-architecture.md). Retain
the deliberately scoped custom overlay as the fallback if that measurement
fails.

### Implemented vertical slice

The first slice is now wired:

- Control/Command-I opens a parented PNG picker and imports through the existing
  bounded, undoable layer command;
- Control/Command-Shift-E opens a parented full-canvas PNG save dialog;
- adding Alt selects the existing exact-content-bounds export;
- non-PNG save names gain a final `.png` suffix because Linux save filters do
  not enforce or append the format consistently;
- cancellation performs no document or filesystem mutation.

Apollo exposes both `org.freedesktop.portal.Desktop` and
`org.freedesktop.portal.Documents`. The locked release build succeeds, and
Cargo's inverse dependency tree contains one wgpu version (`30.0.0`) plus the
isolated `rfd 0.17.2`; the dialog work did not introduce wgpu 29. Automated
coverage proves path normalization and the existing codec/document paths.
Physical interaction, focus return, and tablet-event behavior around an open
modal dialog remain a live-app checklist rather than a headless claim.

### Native document workflow

The same dialog boundary now exposes the exact layered container as named
`.sketchpad` documents:

- Control/Command-S saves the active path or opens Save As when no path exists;
- Control/Command-Shift-S always chooses a new path;
- Control/Command-O chooses and opens a document;
- the title shows the active file name and a star only for explicit-document
  modifications;
- save-before-open and save-before-close prompts run only after recovery is
  current;
- choosing “No” may leave the named document unchanged, but it cannot discard
  the recoverable committed drawing;
- opening a document marks recovery stale without marking the named document
  modified, so autosave captures the new session without lying about Save.

Dialog cancellation and failed decoding leave the current document, path, and
modified state unchanged. Paths without the declared suffix gain
`.sketchpad`. The active native path is session state for now; a recovered
startup deliberately has no assumed named path and therefore uses Save As.
