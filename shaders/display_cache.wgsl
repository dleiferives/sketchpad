struct Camera {
    center: vec2f,
    zoom: f32,
    _padding: f32,
    viewport_size: vec2f,
    canvas_size: vec2f,
}

@group(0) @binding(0) var display_cache: texture_2d<f32>;
@group(0) @binding(1) var<uniform> camera: Camera;

fn view_size() -> vec2f {
    let height = camera.canvas_size.y / camera.zoom;
    return vec2f(height * camera.viewport_size.x / camera.viewport_size.y, height);
}

struct CacheVertexOutput {
    @builtin(position) position: vec4f,
    @location(0) screen_uv: vec2f,
}

@vertex
fn cache_vs(@builtin(vertex_index) vertex_index: u32) -> CacheVertexOutput {
    let positions = array(
        vec2f(-1.0, 1.0),
        vec2f(-1.0, -1.0),
        vec2f(1.0, -1.0),
        vec2f(-1.0, 1.0),
        vec2f(1.0, -1.0),
        vec2f(1.0, 1.0),
    );
    let position = positions[vertex_index];
    var output: CacheVertexOutput;
    output.position = vec4f(position, 0.0, 1.0);
    output.screen_uv = position * vec2f(0.5) + vec2f(0.5);
    return output;
}

@fragment
fn cache_fs(input: CacheVertexOutput) -> @location(0) vec4f {
    let size = view_size();
    let world = camera.center + (input.screen_uv - vec2f(0.5)) * size;
    if (world.x < 0.0 || world.y < 0.0
        || world.x >= camera.canvas_size.x || world.y >= camera.canvas_size.y) {
        return vec4f(0.0);
    }
    return textureLoad(display_cache, vec2i(world), 0);
}
