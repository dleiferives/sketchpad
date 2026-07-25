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
  exit status.
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
  `scripts/<host> benchmark gpu <adapter-filter> [options...]`.
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
