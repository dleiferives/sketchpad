# Input traces

This directory contains versioned, reviewable input fixtures. A canonical
tablet trace records normalized pressure/tilt/distance, X11 source timestamps,
application-arrival offsets, phases, device identity, raw window positions,
and the recording viewport.

Record the next complete pen-down through pen-up gesture on Atlas:

```text
scripts/atlas record traces/canonical-wacom-v1.json
```

Recording mode starts with a blank transient document, does not load or save
the recovery checkpoint, writes the trace atomically beneath the remote
`.artifacts/` directory, exits after pen-up, and fetches that one artifact
back into this directory.

Committed benchmark traces must pass parser validation and retain their exact
content hash. Randomized replay scenes are generated from a committed trace
and an explicit seed; random output is not committed as a replacement for its
recipe.

Current fixture:

| File | Device | Samples | Arrival duration | Content hash |
| --- | --- | ---: | ---: | --- |
| `canonical-wacom-v1.json` | Wacom Intuos Pro S Pen stylus | 68 | 385.124 ms | `46da823fd749864d` |

This first fixture proves capture and replay; it is not a representative brush
corpus. Add separate versioned traces for very light pressure, maximum
pressure, fast motion, long curves, eraser use, and materially different
drawing styles rather than overwriting this evidence with an unlabeled sample.
