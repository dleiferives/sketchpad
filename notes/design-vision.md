# Design Vision — Sketchpad

## Core Identity

A drawing app that combines three things into one:

1. **Infinite zoom & resolution** (Mischief's SDF magic)
2. **Blazing fast GPU strokes** (modern compute shader pipeline)
3. **Light, minimal interface** (Sketchbook Pro's philosophy)

The canvas is infinite. The rendering is mathematically precise. The tools stay out of your way.

## Reference Apps

### Mischief (by 61 Solutions / Foundry)

The breakthrough: **Adaptively Sampled Distance Fields (ADFs)** instead of pixels or vectors.

- Every stroke is a continuous mathematical function, not a grid of pixels
- Zoom in infinitely — no blur, no pixelation, no vector subdivision
- File sizes are tiny because only edge regions need high-resolution sampling
- Strokes are CSG union operations on the distance field: `SDF_canvas = min(SDF_canvas, SDF_stroke)`
- ADFs store distance values at varying resolution — dense near surfaces, sparse in empty regions
- Runs on GPU via fragment shader evaluation of the distance field

ADFs were developed at MERL (Mitsubishi Electric Research Labs) by Sarah Frisken, Ron Perry, and others (SIGGRAPH 2000). Mischief was the first major consumer product to use them.

### Sketchbook Pro (Autodesk)

The interface philosophy:
- Maximum canvas, minimum chrome
- Floating tool palette — dismissible, repositionable
- Tools grouped by function: draw / select / transform / color
- One-tap access to brush, eraser, color picker, undo
- Pinch-zoom-pan gestures feel completely natural
- The canvas itself is the primary UI surface
- Dark theme that recedes behind the artwork

## Our Approach: Hybrid Vector-SDF Architecture

Two representations, one canvas.

### Representation A: SDF Canvas (infinite zoom)

The canvas is fundamentally a signed distance field. For every point `(x, y)` in the infinite plane, there's a signed distance `d` to the nearest drawn surface. Negative = inside a stroke, positive = outside.

Stored as a **sparse hierarchical grid**:

```
Level 0 (coarse):    whole canvas → 1 tile
Level 1:             4 tiles
Level 2:             16 tiles
Level 3:             64 tiles
...
```

Each tile is a small SDF texture (e.g., 128×128 distance samples) covering a region. Tiles subdivide when needed — i.e., when a stroke passes through them, creating detail that the current resolution can't capture.

Key properties:
- **Resolution-independent**: zoom determines which level you sample from
- **Memory-efficient**: most tiles are empty, so don't exist
- **GPU-friendly**: a tile is just a texture — perfect for compute shaders
- **Infinite canvas**: the data structure has no bounds

### Representation B: Vector Stroke Pipeline (input)

When the user draws, we capture Bézier paths from stylus input. The GPU stroke expansion pipeline (from the Levien/Uguray paper) converts these to filled outlines. But instead of rasterizing to pixels, we **stamp them into the SDF**:

```
For each segment of the expanded stroke outline:
    Find affected SDF tiles
    For each affected tile:
        Compute SDF of the stroke region
        SDF_tile = min(SDF_tile, SDF_stroke)
```

This is a compute shader operation. The stroke's SDF can be evaluated analytically (distance to the filled outline polygon) or via the paper's arc/flatten representation.

### Rendering Flow

```
User draws → Stylus input → Bézier path capture
  │
  ├─► GPU stroke expansion (paper algorithm)
  │     Bézier → Euler spiral → parallel curves → flatten
  │
  └─► SDF stamping (compute shader)
        For each tile the stroke overlaps:
          Evaluate distance to stroke geometry
          SDF[tile] = min(SDF[tile], distance)
          If tile detail exceeds threshold → subdivide

Display:
  View transform (zoom, pan) → select tile level
  Fragment shader per screen pixel:
    Sample SDF at pixel position
    if d < 0: inside stroke → fill color
    if |d| < 1px: edge → anti-alias
    if d > 0: outside → background / transparent
```

The display shader is where the SDF magic happens. Anti-aliasing is free — just smoothstep the distance to the edge. Zoom is free — just scale the sampling coordinate. Everything stays crisp.

### Why This Beats Pure Vector

- **Vector renderers** must re-flatten/re-expand strokes every frame when zoomed — work is proportional to scene complexity × zoom level
- **SDF renderers** do work proportional to screen pixels only — the scene complexity is pre-baked into the SDF
- **Vector files** get huge with many complex strokes; SDF storage compresses by only storing detail where needed
- **Vector undo** requires rebuilding the scene graph; SDF undo is cheap (just revert tiles)

### Why This Beats Pure Raster

- Raster zoom = blur
- Raster storage = pixels × resolution, huge for high-zoom canvases
- SDF zoom = perfect

## Interface Principles

### Canvas-First

The canvas fills the screen. Everything else is transient, dismissible, or overlaid with transparency.

### Tool System

```
Core tools (always one-tap away):
  Brush     — draw with current brush
  Eraser    — subtract from SDF (same as brush, negative)
  Color     — picker + palette
  Undo/Redo — tap or gesture

Secondary tools (toolbar or radial menu):
  Select    — lasso / rect select a region
  Transform — move / scale / rotate selection
  Layer     — layer management
  Zoom      — pinch gesture is primary; explicit tool for non-touch
```

### Brush System

Brushes are parameterized:

```
Brush = {
    shape: circle | flat | texture | custom
    size: 1..N px
    opacity: 0..1 (how much the SDF is modified)
    hardness: 0..1 (soft edge fall-off)
    spacing: 0..1 (how far apart sample points are)
    texture: optional image/noise for grain
}
```

The SDF brush kernel: for each point along the stroke, compute the distance field of the brush shape at that position, and union it into the canvas SDF. A soft brush simply uses a distance function with a smooth fall-off rather than a hard edge.

### Gestures

- **One finger** — draw
- **Two finger pinch** — zoom
- **Two finger pan** — scroll
- **Two finger rotate** — rotate canvas
- **Three finger tap** — undo
- **Three finger swipe** — redo
- **Long press** — color picker (eyedropper)

### Platform Adaptations

| Feature | Desktop | Mobile |
|---|---|---|
| Primary input | Stylus/tablet | Finger/stylus |
| Toolbar | Fixed sidebar | Floating, collapsible |
| Undo | Ctrl+Z | Three-finger tap |
| Zoom | Scroll wheel or pinch | Pinch |
| Color picker | Eyedropper tool | Long press |
| File menu | Menu bar | Bottom sheet / drawer |

## Data Model

### Document

```
Document {
    layers: Vec<Layer>
    size: (width, height) in document units
    background: Color
}
```

### Layer

```
Layer {
    name: String
    visible: bool
    opacity: f32
    blend_mode: BlendMode
    sdf_grid: SparseSDFGrid
}
```

### SparseSDFGrid

```
SparseSDFGrid {
    root: Tile
    max_depth: u32         // maximum subdivision level
    resolution_per_tile: u32  // e.g., 128×128 samples
}

Tile {
    data: Option<SDFTexture>  // None = empty tile
    children: Option<[Tile; 4]>  // quadtree subdivision
}
```

### SDFTexture

A 2D array of `f32` distance values. Positive = outside, negative = inside. The magnitude is the distance to the nearest edge in document units.

## Performance Targets

| Scenario | Target |
|---|---|
| Stroke input to display latency | < 8ms (below one frame at 120Hz) |
| Canvas render (1080p) | < 4ms GPU time |
| Zoom (pinch) | 60fps sustained |
| Undo | Instant (single frame) |
| File save (typical drawing) | < 100ms |
| File open (typical drawing) | < 500ms |
| Max SDF tile resolution | 128×128 (fits in GPU shared memory) |
| Max subdivision levels | 16 (covers zoom from 1:1 to 65,536:1) |

## Technology Choices Summary

| Concern | Choice | Rationale |
|---|---|---|
| Language | Rust | Safety, wgpu ecosystem, cargo |
| GPU API | wgpu | Vulkan/Metal/D3D12/WebGPU |
| Shader language | WGSL | wgpu native, cross-platform |
| Windowing | winit | Cross-platform, stylus events |
| Stroke input → Bézier | kurbo | Curve math |
| Stroke expansion | Custom WGSL (Levien/Uguray) | Our own pipeline |
| Canvas storage | Sparse SDF quadtree | Infinite zoom, compact |
| SDF stamping | Compute shader (custom) | GPU SDF CSG |
| Display | Fragment shader SDF evaluation | Free AA, free zoom |
| File format | Custom binary (.sketchpad) | Designed for SDF tiles |
| UI toolkit | TBD (possibly egui or custom) | Needs to be light |
