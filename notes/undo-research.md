# Undo storage and replay research

2026-09-12. Recommendation: keep exact recent undo, retain stroke commands, and make older history cheaper through lossless compression, sharing, and bounded disk storage. Evaluate checkpointed GPU replay for older history after measuring its latency and exactness. The broad-stroke fix in `0fe4948` is a correctness repair, not the finished memory optimization.

Implementation update: [lossless archive storage and validation](undo-storage-implementation.md). The observations below describe the pre-archive baseline.

## Baseline before archive storage

`src/gpu_document_undo.rs` saves affected, block-aligned regions of the active layer. The document uses 128×128 tiles; undo damage is rounded to 16×16 blocks and merged into a rectangle within each affected tile. This is not a screenshot of every layer or a copy of the entire canvas on every input sample. One completed round stroke creates one undo transaction. However, painting across most tiles can still make its saved regions cover the entire canvas.

Pixels are four 32-bit floating-point channels: 16 bytes per pixel. A 4096×4096 pixel payload is 256 MiB. For comparison, four 16-bit channels would be 128 MiB and four 8-bit channels 64 MiB. These are raw payload calculations, not complete process memory estimates or interchangeable quality settings.

The original 64 MiB history ceiling rejected a 256 MiB memento at pen-up and the app canceled the stroke. The fix allows a layer-wide transaction within atlas capacity and aligns the mirror, recovery journal, and before/after recovery budgets. It also relieves pending mirror pressure before another commit. Large recent history can still evict older entries; we have not added compressed or disk-backed history.

We already retain stroke information: `GpuRoundRecoveryCommand` stores the brush recipe, target layer and path commands. The live recovery system combines commands with a pixel base, then incorporates exact GPU readback. Normal undo uses GPU pixel mementos; commands primarily support recovery. GPU history, pending readback, CPU mirror and exact before/after recovery can all contribute memory. Shared allocations must be counted once when measuring actual peak usage.

An immediate broad-stroke save exposed expensive CPU replay during the regression. Save/export now reconcile the existing GPU result, and autosave waits for that readback. The current recovery implementation's CPU replay cost does not establish that a future optimized GPU replay design would be slow.

## What other applications establish

- **MyPaint:** `Brushwork` records a stroke sequence, but normal undo and redo load before/after snapshots. Its tile surface snapshots share tile objects and copy a tile when it is next written. Stroke replay is also used to retrace the last stroke with a changed brush. Keeping stroke data and keeping pixel history are compatible choices. [Brushwork source](https://github.com/mypaint/mypaint/blob/master/lib/command.py), [tile snapshot source](https://github.com/mypaint/mypaint/blob/master/lib/tiledsurface.py).
- **Krita:** its memento manager retains changed tile versions through copy-on-write. Its documented performance settings allow older undo states to move to a disk swap file, trading older-undo latency for available RAM. [Memento manager source](https://github.com/KDE/krita/blob/master/libs/image/tiles3/kis_memento_manager.h), [performance settings](https://docs.krita.org/en/reference_manual/preferences/performance_settings.html).
- **GIMP:** its manual distinguishes inexpensive metadata changes from costly raster operations and uses both minimum undo levels and a memory budget. It does not save the undo history in XCF. This is useful precedent for separating document saving from retained undo history. [Undo documentation](https://docs.gimp.org/3.0/en/gimp-concepts-undo.html).
- **Procreate:** its public help documents up to 250 recent steps in a session, clearing history when leaving the canvas, and invalidating redo after a new edit. This establishes user-facing behavior; it does not disclose the internal pixel storage or compression design. [Undo and redo](https://help.procreate.com/articles/tvicQm-undo-and-redo).

## Why not store only strokes?

A recipe can be much smaller than its pixel effect: coordinates, pressure, timing, brush settings, target layer, and any required seeds or brush resources. But that is an instruction for producing pixels, not the previous pixels it covered. An opaque stroke or eraser destroys information in the current layer. Undo must obtain the earlier state from stored pixels or reconstruct it from an earlier base and retained operations.

| Approach | Strength | Cost or limitation |
| --- | --- | --- |
| Changed pixel regions | Exact, predictable recent undo; handles paint, erasing, imports and destructive edits | Broad edits need large payloads unless shared or compressed |
| Stroke/operation log plus checkpoints | Compact commands; older states can be rebuilt without retaining every raw undo buffer | Replay work grows between checkpoints; needs complete resources, dependencies and renderer compatibility |
| Hybrid | Fast recent undo plus cheaper older history; commands remain useful for replay/recovery | More ownership, scheduling and testing complexity |

A practical replay design starts from a nearby checkpoint, not necessarily from the first stroke. It can restrict reconstruction to affected tiles when dependencies permit. Smudge, wet paint, filters, selections and imported images complicate this: an operation may read neighboring or earlier pixels and needs more than pointer coordinates. Bit-exact replay across different GPUs or renderer versions must be demonstrated, not assumed.

## Proposed next work

1. **Measure the current system.** Track commit and undo/redo latency, GPU buffers, CPU mirror/history bytes, unique shared allocations, and temporary peak allocations. Use small strokes, long thin strokes, the 512 px full-canvas case, translucent overlaps, erasers, imports, multiple layers and rapid undo/redo. Run isolated release benchmarks on Atlas and Apollo; the hidden-window functional test is not a latency benchmark.
2. **Compact exact history first.** Benchmark lossless compression of older pixel regions, markers for all-transparent or uniform regions, and shared immutable tile versions. Keep recent history ready to apply. Avoid storing duplicate full-sized pixel payloads where ownership can safely be shared. On a previously blank tile, the old state should ideally be representable without a full zero-filled buffer; redo still needs a retained new version or a valid reconstruction path.
3. **Move older history out of scarce memory.** Use a bounded, checksummed disk cache and prefetch around the current history position. Treat history residency separately from whether an edit can commit. Budget exhaustion must not silently delete the stroke that was just drawn.
4. **Compare checkpointed GPU replay.** Preserve versioned recipes, random seeds, brush resources and operation dependencies. Choose checkpoints by measured replay work and memory pressure, rather than an arbitrary fixed number of strokes. Retain exact pixel data for operations that cannot be replayed reliably or cheaply.
5. **Consider pixel precision separately.** RGBA16F would halve raw pixel storage but changes numerical behavior. Evaluate blending, repeated erasure, color precision and cross-device rendering before making that product-wide change. Lossless storage improvements avoid that quality tradeoff.

Behavior to preserve: one pen contact is one undo step; a committed selection transform is one step; navigation does not add drawing history; new edits after undo create a new branch; save/recovery and undo-after-restart are explicit, separate policies. Recent undo should remain responsive even after a long painting session. Large edits should remain visible while the application manages history storage.
