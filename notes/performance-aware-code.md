# Performance-Aware Code for Sketchpad

Status: implementation doctrine, 2026-07-24. This note translates
performance-aware and data-oriented programming research into concrete rules
for Sketchpad. It is not a claim that every function must be hand-optimized.
It defines how the hot drawing path should be shaped, what must be measured,
and which abstractions should not be introduced without evidence.

## Decision

Sketchpad should be written so that its important work is visible in the code
and legible to the hardware:

- organize hot code around transformations over batches of data;
- resolve sparse and dynamic state outside inner pixel loops;
- give kernels contiguous memory with explicit bounds and formats;
- allocate and snapshot at tile or gesture granularity, never per pixel;
- dispatch brush and blend variants outside the hottest loops;
- preserve low-level entry points underneath convenient operations;
- build concrete paths first and compress only repeated, observed structure;
- estimate traffic and work before optimizing;
- measure deterministic workloads on the target hardware before declaring a
  design fast;
- keep correctness tests and instrumentation beside the optimized path.

This is the implementation counterpart to the product direction in
[first-usable-product.md](first-usable-product.md) and the measurement contract
in [performance-laboratory.md](performance-laboratory.md).

## What the Research Actually Says

### Performance awareness is earlier than optimization

Casey Muratori distinguishes performance-aware programming from late,
hardware-specific optimization. The former means understanding how ordinary
design choices affect the amount and shape of the work, so a program does not
begin hundreds of times away from reasonable hardware use. His course is
organized around waste, instructions per clock, SIMD, caching, multithreading,
data throughput, repetition testing, and inspection of actual machine code.

Sketchpad inference: do not begin with SIMD intrinsics or unsafe Rust. Begin by
ensuring a brush update does not walk the entire canvas, replay old strokes,
allocate per sample, hash per pixel, or upload unchanged tiles. Those are
algorithm and dataflow problems whose cost dominates instruction-level tuning.

Sources:

- Casey Muratori,
  [Welcome to the Performance-Aware Programming Series](https://www.computerenhance.com/p/welcome-to-the-performance-aware)
- Casey Muratori,
  [Performance-Aware Programming table of contents](https://www.computerenhance.com/p/table-of-contents)

### Organize by the operation when the operation is what must be fast

Muratori's "Clean Code, Horrible Performance" example is not merely about the
few cycles of one indirect call. His follow-up stresses that virtualized
hierarchies obscure the complete operation from both the compiler and the
programmer. Flattening the representation makes common computation visible;
segregating kinds of data can be better still when the workload permits it.

This does **not** mean every `match` is fast or every trait is slow. The relevant
question is where dispatch happens and what it prevents:

- one brush dispatch per command or tile is ordinary control work;
- one dynamic dispatch per pixel prevents a simple, visible pixel kernel;
- a trait at a renderer boundary may be harmless;
- a graph of tiny polymorphic color operations inside every dab is a hot-path
  architecture problem.

Sketchpad inference: select a concrete brush kernel before entering its tile
loop. Let the kernel see the pixel representation, row stride, clipped bounds,
brush parameters, and reservoir state directly. Do not make every pixel call
through `dyn Brush`, `dyn BlendMode`, or a chain of boxed nodes.

Sources:

- Casey Muratori,
  ["Clean" Code, Horrible Performance](https://www.computerenhance.com/p/clean-code-horrible-performance)
- Casey Muratori,
  [Response regarding "Clean Code, Horrible Performance"](https://www.computerenhance.com/p/response-to-a-reporter-regarding)
- Mike Acton,
  [Data-Oriented Design and C++](https://isocpp.org/blog/2015/01/cppcon-2014-data-oriented-design-and-c-mike-acton)

### Data orientation means studying the actual data

Data-oriented design is not a universal instruction to replace every
array-of-structures with a structure-of-arrays. The useful questions are:

- what data exists;
- how much exists;
- which fields are used together;
- how often each transform runs;
- what order it is traversed in;
- which cases are common or rare;
- what must be mutable, retained, uploaded, or persisted.

For Sketchpad, RGBA channels are commonly read and written together during
compositing, so an interleaved pixel may be appropriate. Brush sample
attributes may be transformed in batches where separate arrays are useful.
Tile metadata is cold relative to tile pixels and should not be interleaved
through the pixel buffer. The choice follows access patterns and measurements,
not a slogan.

Sources:

- Richard Fabian,
  [Data-Oriented Design: type, frequency, quantity, shape, and probability](https://www.dataorienteddesign.com/dodmain/node3.html)
- Mike Acton,
  [Data-Oriented Design and C++](https://isocpp.org/blog/2015/01/cppcon-2014-data-oriented-design-and-c-mike-acton)

### Build from working details, then compress repeated semantics

Muratori's "Semantic Compression" recommends making code usable before trying
to make it reusable. A reusable concept should normally be extracted after at
least two real examples reveal what is actually shared. His "Complexity and
Granularity" adds that a convenient high-level operation should still be
replaceable by a small number of lower-level operations.

Sketchpad inference:

- implement one direct raster tile path before inventing a universal media
  graph;
- implement two or more real brush kernels before claiming their shared model;
- do not genericize tile storage over every hypothetical pixel type in the
  first implementation;
- retain a low-level "edit this clipped tile region" operation underneath
  higher-level gesture and brush calls;
- add a renderer interface only at a boundary that is demonstrably stable.

This is compatible with good Rust types. It rejects speculative abstraction,
not type safety.

Sources:

- Casey Muratori,
  [Semantic Compression](https://caseymuratori.com/blog_0015)
- Casey Muratori,
  [Complexity and Granularity](https://caseymuratori.com/blog_0016)
- Casey Muratori,
  [Defining a Single Enumerant](https://caseymuratori.com/blog_0017)

### Allocation and indirection require evidence

The Rust Performance Book notes that allocation has real fixed costs, that
`Vec` can avoid repeated growth when capacity is known, that workhorse
collections can retain capacity between iterations, and that `Rc`/`Arc`
introduce reference counts and heap allocation when sharing is not actually
needed. It also warns that seemingly clever alternatives such as `SmallVec`
still require benchmarks.

Sketchpad inference:

- a tile owns one fixed-size contiguous pixel allocation;
- a gesture reuses command, touched-tile, and scratch buffers where practical;
- tile snapshots occur once on first write in a gesture;
- undo owns explicit snapshots rather than making every live tile an `Arc`;
- sparse lookup tables reserve based on observed document sizes;
- GPU staging allocations are pooled or ring-buffered;
- no allocation is allowed in the per-pixel portion of a brush kernel;
- alternative hashers, small-vector types, arenas, and custom allocators are
  experiments, not defaults.

Source:

- Nicholas Nethercote et al.,
  [The Rust Performance Book: Heap Allocations](https://nnethercote.github.io/perf-book/heap-allocations.html)

## The Two Planes

Sketchpad's first raster implementation should make a strong distinction
between control work and bulk pixel work.

### Control plane

Control work may branch, hash, validate, and allocate when necessary:

- normalize input samples;
- determine brush variant and parameters;
- compute conservative dab or segment bounds;
- turn bounds into affected tile coordinates;
- look up or allocate each affected tile;
- snapshot a tile once on first gesture write;
- choose the CPU or GPU kernel;
- record dirty rectangles and revisions;
- enqueue uploads and compositing work;
- commit, cancel, undo, or redo a transaction.

This work should still be measured, but it occurs per sample, command, tile, or
gesture rather than per pixel.

### Pixel plane

Bulk work receives already-resolved data:

- one or more contiguous pixel slices;
- tile-local, clipped integer bounds;
- explicit row stride and valid edge extent;
- a concrete pixel format;
- precomputed brush coefficients;
- direct reservoir/mixing state;
- no sparse-map lookup;
- no object graph traversal;
- no heap allocation;
- no logging or string formatting;
- no dynamic brush dispatch;
- no lock acquisition.

The inner traversal should ordinarily be rows with `x` as the inner dimension.
Bounds checks should be easy for the compiler to remove or hoist. Branches that
are constant for a command should be selected before the loop.

## Intended Brush Update Shape

```text
real input samples
        │
        ▼
resample / stabilize / generate brush commands
        │
        ▼
compute command bounds and affected tile coordinates
        │
        ├── sparse lookup or allocate: once per affected tile
        ├── snapshot: at most once per tile per gesture
        └── clip command to valid tile-local rectangle
                              │
                              ▼
          concrete contiguous tile kernel
          coverage → pickup/deposit/mix → pixel write
                              │
                              ▼
          dirty rectangle + revision + upload command
```

An API that accepts one pixel at a time is useful for tests and tools, but it
must be a convenience layer over tile editing, not the path used by a brush
dab. The low-level tile operation is the performance boundary.

## First Raster-Core Constraints

The first code slice should enforce the following properties:

1. A canvas has explicit integer pixel bounds.
2. An untouched canvas allocates no pixel tiles.
3. A tile contains a contiguous fixed-size pixel buffer and separate cold
   metadata.
4. Tile lookup happens through a sparse coordinate index.
5. A gesture owns the write transaction.
6. The first write to a tile in a gesture captures exactly one before-image.
7. Repeated writes to that tile do not capture more snapshots.
8. Cancel restores before-images and removes tiles created by the gesture.
9. Commit creates one undo unit and clears redo.
10. Undo and redo swap owned tile states rather than clone them again.
11. Damage is explicit and clipped to canvas/tile bounds.
12. Bulk tile editing is available without per-pixel sparse lookups.
13. Absent tiles read as transparent.
14. Empty committed tiles can be removed from canonical sparse storage.
15. Tests cover edge tiles, cancellation, allocation count, damage, and exact
    undo/redo state.

The first version may use a clear reference pixel format while format
benchmarks are constructed. That temporary choice must be named as such and
must not silently freeze the persistent file format.

## Memory and Traffic Estimates

These are arithmetic estimates, not benchmark results.

For interleaved RGBA:

| Tile | 8 bytes/pixel | 16 bytes/pixel |
| --- | ---: | ---: |
| 128×128 | 128 KiB | 256 KiB |
| 256×256 | 512 KiB | 1 MiB |

The 8-byte case corresponds to four 16-bit channels; the 16-byte case
corresponds to four `f32` channels. A first-write undo snapshot copies at least
that much per touched tile. A kernel that reads and writes every pixel moves at
least twice that tile size before mask, reservoir, cache, or upload traffic.

A 3840×2160 pass over an 8-byte working image reads or writes about 63.3 MiB.
One read plus one write is about 126.6 MiB. This is why damage-limited work is
an architectural requirement, not a late optimization.

These estimates suggest experiments; they do not decide between 128 and 256
tiles, normalized and floating-point channels, or fused and separated passes.

## Instrumentation Is Part of the Feature

Every brush replay should eventually report at least:

- real samples consumed;
- generated commands or dabs;
- tile lookups;
- tiles allocated;
- tiles snapshotted;
- snapshot bytes;
- pixels conservatively covered;
- pixels actually modified, where inexpensive to count;
- dirty tile count and dirty area;
- CPU command-generation time;
- CPU tile-kernel time;
- upload bytes;
- GPU pass count and GPU timestamps;
- end-to-end frame timing;
- undo memory growth.

Counters should be plain numeric state, compiled into development builds, and
reset/reused without allocation. They make hidden amplification visible before
a profiler is needed.

## Measurement Rules

The Rust Performance Book recommends representative workloads, comparison
between versions, and metrics appropriate to the product. It also notes that
wall time is user-relevant but noisy and that cycles or instruction counts can
be useful lower-variance diagnostics.

For Sketchpad:

- measure release builds;
- use deterministic stroke traces over empty, lightly painted, and dense
  existing tiles;
- record distributions and worst frames, not only a mean;
- separate cold allocation, warm drawing, commit, undo, and upload phases;
- report bytes and counts beside time;
- compare output against a correctness oracle;
- run the same workloads before and after each structural optimization;
- use Atlas `perf`/hardware counters when available;
- add allocation profiling when snapshot or command allocation is material;
- inspect generated assembly only after a measured kernel is hot;
- retain results with hardware, driver, revision, and configuration metadata.

Sources:

- Nicholas Nethercote et al.,
  [The Rust Performance Book: Benchmarking](https://nnethercote.github.io/perf-book/benchmarking.html)
- Nicholas Nethercote et al.,
  [The Rust Performance Book: Profiling](https://nnethercote.github.io/perf-book/profiling.html)

## Rust Policy

Safe Rust is the default because ownership and bounds are important to document
correctness. Safety is not an excuse for a fragmented data model, and `unsafe`
is not a performance strategy by itself.

Use `unsafe` only when all of the following are true:

1. a release benchmark identifies the exact safe construct as material;
2. generated code or profiling explains the cost;
3. a small, isolated unsafe kernel removes that cost;
4. the safe and unsafe paths share deterministic equivalence tests;
5. the unsafe block states its invariants next to the code;
6. the measured win is retained in the performance record.

Likewise, do not scatter `#[inline(always)]`, SIMD, custom allocators, or a
faster hasher through the code on intuition. Each is a local experiment after
the dataflow is correct.

## Review Questions

For every hot-path change, ask:

1. What exact data transform is this?
2. At what frequency does it run: gesture, sample, tile, pixel, or channel?
3. How many bytes and allocations should it require?
4. Is sparse lookup outside the bulk loop?
5. Is dispatch outside the bulk loop?
6. Are hot and cold fields separated?
7. Is the traversal contiguous and bounded?
8. Can scratch capacity be reused?
9. Did we make the common case easy without removing a lower-level path?
10. Is the abstraction based on two real uses?
11. What correctness test protects it?
12. What deterministic benchmark would show a regression?
13. What did measurement say on the actual target?

## Anti-Goals

Do not mistake performance-aware programming for:

- one enormous function;
- removing useful names and module boundaries;
- avoiding all traits or iterators;
- speculative `unsafe`;
- manual SIMD everywhere;
- refusing libraries;
- assuming structure-of-arrays is always superior;
- trusting microbenchmarks without product traces;
- optimizing cold UI or file-dialog code while brush latency is unmeasured;
- cargo-culting the exact memory systems of a C or C++ game engine into Rust.

The goal is simple, inspectable work with an honest cost model. Readability and
performance should reinforce each other in the drawing path because both the
programmer and the processor need to see what happens.

## Immediate Consequences

Before the first sparse raster implementation is considered complete:

- expose one bulk tile-edit entry point;
- keep the existing prototype runnable;
- add deterministic transaction tests;
- add counters for tile lookup, allocation, snapshots, and snapshot bytes;
- create a release benchmark trace that repeatedly paints over existing tiles;
- compare at least 128×128 and 256×256 tiles;
- measure the reference pixel representation rather than assuming it is
  production-worthy;
- record the first Atlas result in the performance laboratory.
