struct Camera {
    center: vec2f,
    zoom: f32,
    _padding: f32,
    viewport_size: vec2f,
    canvas_size: vec2f,
}

@group(0) @binding(0) var color_page: texture_2d<f32>;
@group(0) @binding(1) var<uniform> camera: Camera;

fn view_size() -> vec2f {
    let height = camera.canvas_size.y / camera.zoom;
    return vec2f(height * camera.viewport_size.x / camera.viewport_size.y, height);
}

struct CompositeVertexOutput {
    @builtin(position) position: vec4f,
    @location(0) local: vec2f,
    @location(1) @interpolate(flat) physical_origin: vec2u,
    @location(2) @interpolate(flat) pixel_extent: vec2u,
    @location(3) @interpolate(flat) opacity: f32,
}

@vertex
fn composite_vs(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) logical_origin: vec2f,
    @location(1) logical_extent: vec2f,
    @location(2) physical_origin: vec2u,
    @location(3) opacity: f32,
) -> CompositeVertexOutput {
    let corners = array(
        vec2f(0.0, 1.0),
        vec2f(0.0, 0.0),
        vec2f(1.0, 0.0),
        vec2f(0.0, 1.0),
        vec2f(1.0, 0.0),
        vec2f(1.0, 1.0),
    );
    let local = corners[vertex_index];
    let world = logical_origin + local * logical_extent;
    let size = view_size();
    let ndc = (world - camera.center) * vec2f(2.0 / size.x, 2.0 / size.y);

    var output: CompositeVertexOutput;
    output.position = vec4f(ndc, 0.0, 1.0);
    output.local = local;
    output.physical_origin = physical_origin;
    output.pixel_extent = vec2u(logical_extent);
    output.opacity = opacity;
    return output;
}

@fragment
fn composite_fs(input: CompositeVertexOutput) -> @location(0) vec4f {
    let size = vec2i(input.pixel_extent);
    let local_pixel = clamp(vec2i(input.local * vec2f(size)), vec2i(0), size - vec2i(1));
    return textureLoad(color_page, vec2i(input.physical_origin) + local_pixel, 0) * input.opacity;
}
