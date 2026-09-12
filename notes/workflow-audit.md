# Drawing workflow audit

Requested scope: finish mouse brush-adjustment behavior, fix file management/save/load, then improve color picking, import, layers, selection, moving selected artwork, moving whole layers, stacking order, and remaining layout friction. Commit each tested increment.

## Completed before this audit

- Compact rail/puck, tablet-friendly controls and desktop shortcuts (`e9007ac`).
- Stationary brush preview during adjustment (`c29d109`).
- Actual mouse pointer returns to the origin on release or Escape, validated via XQueryPointer on Atlas (`66c179c`). The release binary has been built; avoid restarting the user's working window before save/load is reliable.

## File workflow: completed

Observed gaps:

- File operation failures were only logged, without in-app feedback.
- Every dropped file was sent to PNG import, including native `.sketchpad` documents.
- Valid native documents with dimensions different from the startup canvas were rejected.
- File and unsaved-change dialogs depended on synchronous desktop portal calls.
- A failed recovery started blank with the same recovery destination, so later autosave could overwrite the unreadable original.

Implemented an in-app file browser, overwrite confirmation, unsaved-change modal, New, visible results/errors, native-file drop routing, prepared document replacement with view fitting, dimension-aware rendering, and a separate recovery destination after startup recovery failure. Discard on close now restores the saved drawing in recovery, so discarded edits do not reappear. A validated session hint restores the document filename on restart and falls back to an unnamed recovered sketch if either file has changed.

Atlas validation passed: full Cargo tests; hidden-window hardware GPU regression covering immediate save, metadata/pixel round-trip, export, failed open/save, different canvas geometry, filename recovery, and cancel/discard; Clippy (with the existing too-many-arguments allowance); formatting. A live isolated native window saved and reopened a real document through the new browser using mouse and keyboard.

## Large-stroke reliability

A 512 px stroke covering the 4096×4096 canvas reproduced a pen-up failure: the GPU undo memento required 268,435,456 bytes against a 67,108,864-byte limit. Application budgets now allow one layer-wide stroke within the resident atlas capacity, with matching mirror and exact recovery budgets. Pending mirror snapshots are drained when the next commit needs room, and before exact undo/redo or immediate save/export. Autosave waits for the existing GPU readback instead of needlessly replaying a large stroke on the CPU.

Repeated stationary input also replaced a path endpoint timestamp without emitting a command, making the next segment or pen-up discontinuous. The producer now retains the last emitted endpoint; a regression checks recovery geometry with stationary samples before movement and before lift.

Apollo hardware release regressions passed: full-canvas stroke, immediate save/reopen with pixel checks, exact undo and redo, and a following eraser stroke while a full snapshot remained pending; existing file workflow passed as well. Atlas regular tests, Clippy with existing-lint allowances, and formatting passed. These are functional checks, not isolated performance benchmarks. The raised budgets are a correctness fix; compressed history and replay/checkpoint tradeoffs still require separate measurement.

## Next editing work

- Color: improve picking precision and preview, editable values, and palette organization.
- Import: make placement and follow-up movement discoverable; PNG is the current supported format.
- Layers: direct stacking/reordering, clearer active/hidden state, usable opacity, whole-layer move.
- Selection: actual region selection, visible bounds, move/apply/cancel, one coherent undo transaction.
- Layout: review the complete workflow at desktop and tablet sizes, including the new dialogs and panels.

Preserve GPU document/history/recovery semantics. Do not turn selected-pixel movement into a sequence of destructive layer operations or lose history by silently rebuilding the document. Physical tablet testing remains separate from automated gesture tests because no tablet is attached.
