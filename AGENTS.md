# Remote execution

This checkout is the editing authority, but project commands must not run on
the local machine. Atlas is the primary development machine; Apollo is the
lower-power CPU/integrated-GPU test machine.

- Read and edit source files locally.
- Normally run project commands with either
  `scripts/atlas run <command> [args...]` or
  `scripts/apollo run <command> [args...]`. Each command first synchronizes
  the checkout to `<host>:~/projs/sketchpad`, discovers the terminal pane in
  the matching tab of the local `dylan` Zellij session, sends the command
  through its interactive SSH shell, captures output, and returns the remote
  exit status. Commands targeting the same pane are serialized locally; do not
  bypass that lock by pasting a second automated command into the pane.
- Use `scripts/{atlas,apollo} screen`, `interrupt`, `sync`, and `status` for
  the corresponding common operations.
- Remote-generated files belong beneath `.artifacts/`, which source sync
  deliberately preserves. Fetch a specific artifact with
  `scripts/<host> fetch .artifacts/<path> [local-path]`; this is the only
  remote-to-local transfer and must never be used to pull source edits.
- Record and fetch the next complete Atlas tablet stroke with
  `scripts/atlas record [local-trace-path]`.
- Run and fetch a release replay plus machine-context artifact with
  `scripts/<host> benchmark cpu [options...]` or
  `scripts/<host> benchmark gpu <adapter-filter> [options...]`. Use
  `scripts/<host> benchmark profile [options...]` for the prepared-scene exact
  replay oracle and region-dominated CPU transaction loop. It paints
  64-stroke transaction bursts by default;
  `--transaction-batch-strokes 1` preserves the adversarial immediate-undo
  control. GPU replay accepts `--display-hz 60,120` to compare late-latched
  display opportunities against one-submit-per-sample scheduling.
  `--damage-coalescing union|rect4` and `--damage-merge-cost-kib N` select the
  pending-damage policy and its call-equivalent merge threshold.
  `--texture-upload write-texture|staging-ring` selects the transfer path.
  `--presentation direct|cache-rgba32` compares direct tile-array sampling
  with the full-precision dirty-updated display cache.
  Staging with a 0 KiB threshold is the application/replay default;
  selecting `write-texture` without an explicit threshold selects its 64 KiB
  control. `--visibility cached|rebuild` selects persistent visibility and
  instance state or its exact rebuild control. `--view-zoom Z` changes the
  centered replay camera for renderer-isolation cases. Each command first
  rejects known background
  applications/containers, non-performance power state, AC disconnection,
  low available memory, or a non-idle CPU.
- Apollo defaults Cargo to two parallel build jobs to coexist with its 8 GB
  memory budget. Override with `SKETCHPAD_CARGO_BUILD_JOBS=<count>` when a
  deliberately isolated compile test needs another setting. Runtime
  benchmarks are unaffected.
- Use `scripts/sync-to-atlas` or `scripts/sync-to-apollo` when only a one-way
  source synchronization is needed.
- Use `scripts/remote-run <command> [args...]` only as a fallback when the
  shared pane is unavailable. It targets Atlas by default; set
  `SKETCHPAD_REMOTE_HOST=apollo` for Apollo. It also syncs before opening SSH.
- Do not run `cargo`, project binaries, shaders, formatters, generators, or
  other project tooling locally.
- Treat every sync as one-way. Do not edit source files in either remote
  mirror.
- `.git`, `.commandcode`, `.artifacts`, and `target` are host-local and are not
  synchronized.
