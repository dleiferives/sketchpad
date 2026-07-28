struct Camera {
    center: vec2<f32>,
    zoom: f32,
    _padding: f32,
    viewport_size: vec2<f32>,
    canvas_size: vec2<f32>,
}

@group(0) @binding(0)
var stroke_tiles: texture_2d_array<f32>;

@group(0) @binding(1)
var<uniform> camera: Camera;

fn view_size() -> vec2<f32> {
    let height = camera.canvas_size.y / camera.zoom;
    return vec2<f32>(
        height * camera.viewport_size.x / camera.viewport_size.y,
        height,
    );
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
    @location(2) @interpolate(flat) pixel_extent: vec2<u32>,
}

@vertex
fn display_vs(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) origin: vec2<f32>,
    @location(1) extent: vec2<f32>,
    @location(2) layer: u32,
) -> VertexOutput {
    let corners = array(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let local = corners[vertex_index];
    let world = origin + local * extent;
    let size = view_size();
    let ndc = (world - camera.center) * vec2<f32>(2.0 / size.x, 2.0 / size.y);

    var output: VertexOutput;
    output.position = vec4<f32>(ndc, 0.0, 1.0);
    output.local = local;
    output.layer = layer;
    output.pixel_extent = vec2<u32>(extent);
    return output;
}

@fragment
fn display_fs(input: VertexOutput) -> @location(0) vec4<f32> {
    let size = vec2<i32>(input.pixel_extent);
    let pixel = clamp(
        vec2<i32>(input.local * vec2<f32>(size)),
        vec2<i32>(0),
        size - vec2<i32>(1),
    );
    return textureLoad(stroke_tiles, pixel, i32(input.layer), 0);
}
