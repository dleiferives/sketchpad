struct TileUniform {
    origin: vec2<f32>,
    tile_size: f32,
    _padding: f32,
    color: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> tile: TileUniform;

struct VertexInput {
    @location(0) position: vec2<f32>,
}

@vertex
fn stroke_vs(input: VertexInput) -> @builtin(position) vec4<f32> {
    let local = (input.position - tile.origin) / tile.tile_size;
    let clip = vec2<f32>(local.x * 2.0 - 1.0, 1.0 - local.y * 2.0);
    return vec4<f32>(clip, 0.0, 1.0);
}

@fragment
fn stroke_fs() -> @location(0) vec4<f32> {
    return tile.color;
}
