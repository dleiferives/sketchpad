# Exact undo storage

Implemented 2026-09-12, following [the undo research](undo-research.md).

## Behavior and ownership

The application keeps the nearest two history entries on each side of the undo position on the GPU when capacity permits. One compression worker archives older raster transitions using the exact before/after regions already produced by GPU mirror reconciliation. It does not perform another GPU readback. Under history pressure, the application finishes required mirror work and compression before preparing the next stroke commit. Without pressure, painting and hot undo do not wait for an unrelated compression job.

Archived mementos retain their capture plans, resident identities and atlas pins. Their GPU-buffer weight becomes zero; they still count toward the 256-entry logical history limit. On a cold undo/redo, a new mapped GPU buffer is filled from validated archive regions before the history transaction begins. Decompression uses bounded region temporaries, and keeps RGBA32F bits unchanged. A failed restore leaves document pixels, revision and undo position unchanged and displays an error. A completed worker chooses before/after from the entry's current stack; results for evicted or branched-away entries are released.

Recovery commands and GPU mementos share immutable archive handles. Uniform regions use a 16-byte pixel marker; other regions use fast zlib only when smaller than raw bytes. Equal encoded regions share a handle after byte comparison; hash equality alone does not authorize sharing. Handles carry trusted decoded lengths and a 64-bit integrity checksum. Reads bound both encoded and decoded sizes. The checksum detects accidental damage; it is not an authentication mechanism.

Application limits are 32 MiB of unique compressed payload in memory and 512 MiB of disk reservations per document session. Disk reservations round each file to 4 KiB, rather than charging tiny compressed files as only a few bytes; filesystem metadata and differing filesystem allocation units are additional overhead. Recovery allows at most 64 MiB of conservatively accounted archived commands alongside its existing hot before/after budget. Limits are independent of whether a new stroke can commit. If archiving cannot make room, normal bounded oldest-history eviction remains the fallback; the new stroke is retained.

Cache files live under `$XDG_CACHE_HOME/sketchpad/undo` or `~/.cache/sketchpad/undo`. Session directories are private on Unix. Shared handles keep files and reservations alive through history eviction or an outstanding recovery snapshot. Exclusive session leases protect other live documents/processes; abandoned sessions are reclaimed at the next cache creation. This is session undo storage, not undo history embedded in the document format.

The mixed raster/metadata regression also found and fixed a recovery ordering bug: metadata and exact layer installation can advance the CPU mirror without a GPU readback. Recovery now acknowledges those structural revisions when available, while refusing to skip an outstanding raster handoff. This prevents the next stroke or undo from failing with a mirror-source revision mismatch.

## Validation and measurements

`src/bin/undo_storage_smoke.rs` is a headless hardware-GPU correctness control and timing harness. It exercises twelve translucent paint and eraser contacts at 16, 96 and 512 px on a 512×512 canvas. It compares every pixel bit through all undo and redo steps in GPU, compressed-memory, disk and 4 MiB GPU-pressure modes. It also checks metadata/raster ordering, redo invalidation, failed cache reads, restored cache reads, last-handle accounting and a zero-capacity cache fallback that preserves every painted pixel while evicting old history.

For the same fully archived twelve-step history, Atlas and Apollo produced these retained sizes:

| Storage control | GPU history buffers | Accounted CPU history | Unique compressed RAM payload | Reserved disk blocks |
| --- | ---: | ---: | ---: | ---: |
| GPU buffers + raw CPU transitions | 13,852,672 B | 25,157,344 B | 0 | 0 |
| Compressed memory | 0 | 359,448 B | 295,720 B | 0 |
| Disk | 0 | 22,512 B | 0 | 557,056 B |

These are history-storage counters, not process RSS or total application/GPU memory. Current canvas textures, CPU mirror, pending readback, undo swap scratch, transient mapped buffers, allocator overhead, filesystem cache and the harness's expected-pixel arrays are separate. The app normally retains recent GPU history; forcing all entries cold is a correctness and storage control. Smooth brush coverage compresses well; arbitrary imported/textured pixels can fall back to raw storage. The byte results do not predict every drawing's compression ratio.

The harness also uploads an exact checkpoint after contact four and replays contacts five through eight with the same versioned brush recipes and device. The checkpoint base matches exactly, but the result differs at 40 of 262,144 pixels on both tested Intel adapters. The control reports this explicitly. Checkpoint replay remains experimental and is not used to reconstruct production undo.

Both machines rejected isolated benchmark preflight: Atlas had a balanced power profile, background applications/containers and insufficient CPU idle time; Apollo had a balanced profile, no reported online AC supply and background processes. Functional release runs passed without altering those applications or power settings. Their timing arrays are diagnostic observations, not isolated latency or speedup claims.

The separate hidden-window 4096×4096/512 px stroke regression verifies pen-up, immediate save, undo, redo and a following eraser while a large snapshot is pending. The file workflow regression covers pixels, layers, recovery and failed file operations. Regular Rust tests, Clippy with the repository's existing allowances, release builds and the resident bootstrap smoke complement these controls.

## Reproduce

Run through the remote helpers; never run project tooling locally. For an isolated measurement, let the preflight succeed without overriding its checks:

```sh
scripts/atlas run bash -lc 'cargo build --locked --release --bin undo_storage_smoke && scripts/benchmark-preflight && scripts/benchmark-context .artifacts/undo-validation/context.txt && target/release/undo_storage_smoke .artifacts/undo-validation/cache > .artifacts/undo-validation/smoke.txt'
```

Use the corresponding Apollo helper, or the authorized `SKETCHPAD_REMOTE_HOST=apollo scripts/remote-run` fallback when its shared pane is unavailable. Apollo builds use `CARGO_BUILD_JOBS=2`. GPU smoke tests are headless; hidden-window application regressions require the host's X11 display.

## Remaining optimization work

Cold restoration currently reads/decompresses on demand. Prefetch, adaptive checkpoint placement, large-session peak-memory instrumentation and isolated latency distributions remain follow-ups. The checkpoint control must establish exactness and complete operation dependencies before replacing any pixel history. Pixel precision remains unchanged.
