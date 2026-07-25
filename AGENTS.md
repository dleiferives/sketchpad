# Remote execution

This checkout is the editing authority, but project commands must not run on
the local machine.

- Read and edit source files locally.
- Before compiling, testing, or running project code, sync the checkout to
  `atlas:~/projs/sketchpad` with `scripts/sync-to-atlas`.
- Normally run project commands with `scripts/atlas run <command> [args...]`.
  It syncs the checkout, discovers the terminal pane in the `atlas` tab of
  the local `dylan` Zellij session, sends the command through its interactive
  SSH shell, captures the output, and returns the remote exit status.
- Use `scripts/atlas screen`, `scripts/atlas interrupt`, `scripts/atlas sync`,
  and `scripts/atlas status` for the corresponding common operations.
- Use `scripts/remote-run <command> [args...]` only as a fallback when the
  shared Atlas pane is unavailable. It also syncs before opening SSH.
- Do not run `cargo`, project binaries, shaders, formatters, generators, or
  other project tooling locally.
- Treat the sync as one-way. Do not edit source files in the Atlas mirror.
- `.git`, `.commandcode`, and `target` are host-local and are not synchronized.
