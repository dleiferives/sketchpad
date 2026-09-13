# Brush engine research and knife opacity

Read the [full research site](ui-concepts/brush-research.html), hosted on Atlas at
[localhost:8058/brush-research.html](http://localhost:8058/brush-research.html).
It includes an interactive opacity/flow model, actual native knife before/after
images, 22 primary references, an audit of the active renderer, and a proposed
programmable material architecture.

The recommendation is independent coverage and appearance output, deterministic
stroke variation, controlled deposition, and optional pickup/material state.
Continuous coverage, deposition and stateful simulation need distinct execution
contracts within the same package system. The report covers resource declaration,
ordered canvas reads, pigment mixing, height, history, saving and memory budgets.
These engine extensions are proposals; they are not implemented by this change.

## Implemented knife correction

Loaded palette-knife paint now reaches the selected opacity through its interior.
Texture creates localized scrapes and broken edges instead of reducing alpha
everywhere. Pressure still affects contact shape and scraping. Unresolved grooves
fade out at subpixel nib sizes. CPU, legacy GPU and packaged WGSL use matching
formulas. Other brush formulas are unchanged.

The hosted transparent PNGs come from `palette_knife_opacity_study`, rendered in
a hidden native window. Each uses four continuous 96-pixel strokes, with opacity
and pressure combinations (1, 1), (1, 0.25), (0.5, 1), and (0.25, 1). In the
full-opacity/full-pressure interior sampling band, the previous knife had 0/5,280
pixels at alpha 1. The revised knife has 4,607/5,280 at alpha 1 and 489 below 0.25
in scraped regions. These measurements describe that test path, not artistic
preference. Color-shaded ridges, impasto and paint pickup remain future work.

## Validation

Atlas and Apollo passed:

- `cargo test --locked`.
- `cargo run --locked --bin gpu_media_mask_smoke`, including legacy/package
  equivalence and subpixel tips with the existing error tolerance.
- `DISPLAY=:0 cargo test --locked --bin sketchpad palette_knife_opacity_study -- --ignored --nocapture`.
- `DISPLAY=:0 cargo test --locked --bin sketchpad media_brushes_preserve_pixels_history_and_files -- --ignored --nocapture`, including large contacts, history and persistence.
- `cargo build --locked --release`.

Both machines produced identical alpha counts for all four native study rows.
Atlas all-target Clippy passed with the existing `too_many_arguments` and
`while_let_loop` allowances. Changed Rust files passed remote rustfmt checking.
Repository-wide `cargo fmt --check` still reports pre-existing differences in
unrelated GPU document files.

The actual Atlas HTTP page passed headless Chrome checks for both image loads,
comparison toggles, background selection, opacity-model limits, section links and
22 references. Desktop and 390-pixel layouts were captured for visual review;
the narrow layout has no document-level horizontal overflow.

Commands ran remotely; Apollo used the SSH fallback because its shared terminal
pane was unavailable, with two Cargo jobs. Test logs and temporary renders are
host-local under `.artifacts/knife-opacity/`; browser review artifacts are under
`.artifacts/brush-research/`. No artist preference study or performance benchmark
of the proposed engine was performed.
