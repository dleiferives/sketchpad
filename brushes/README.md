# Brush packages

Each directory contains `brush.json` and `brush.wgsl`. Copy `stipple/` to start a
new brush, change its ID to match the new directory, and edit its shader.
Sketchpad reloads accepted changes between strokes; errors appear in the brush
library and leave the previous version working.

See [the authoring guide](../notes/shader-brushes.md) for inputs, footprint limits,
reload behavior, recovery, and performance details. Built-in paper sampling uses
[David Revoy’s credited texture](../assets/brushes/README.md).
