struct Camera {
    center: vec2f,
    zoom: f32,
    _padding: f32,
    viewport_size: vec2f,
    canvas_size: vec2f,
}

struct BrushCursor {
    position: vec2f,
    half_extents: vec2f,
    direction: vec2f,
    shape: f32,
    visible: f32,
    color: vec4f,
}

@group(0) @binding(0) var tile_texture: texture_2d_array<f32>;
@group(0) @binding(1) var<uniform> camera: Camera;
@group(0) @binding(2) var<uniform> cursor: BrushCursor;

fn view_size() -> vec2f {
    let height = camera.canvas_size.y / camera.zoom;
    return vec2f(height * camera.viewport_size.x / camera.viewport_size.y, height);
}

fn world_from_screen(screen: vec2f) -> vec2f {
    let uv = vec2f(
        screen.x / camera.viewport_size.x,
        1.0 - screen.y / camera.viewport_size.y,
    );
    let size = view_size();
    return camera.center + (uv - vec2f(0.5)) * size;
}

@vertex
fn background_vs(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    let positions = array(
        vec2f(-1.0, 1.0),
        vec2f(-1.0, -1.0),
        vec2f(1.0, -1.0),
        vec2f(-1.0, 1.0),
        vec2f(1.0, -1.0),
        vec2f(1.0, 1.0),
    );
    return vec4f(positions[vertex_index], 0.0, 1.0);
}

@fragment
fn background_fs(@builtin(position) screen_position: vec4f) -> @location(0) vec4f {
    let world = world_from_screen(screen_position.xy);
    let inside = world.x >= 0.0
        && world.y >= 0.0
        && world.x <= camera.canvas_size.x
        && world.y <= camera.canvas_size.y;
    if (!inside) {
        return vec4f(0.72, 0.73, 0.70, 1.0);
    }

    let edge_distance = min(
        min(world.x, camera.canvas_size.x - world.x),
        min(world.y, camera.canvas_size.y - world.y),
    );
    let pixel_world = view_size().y / camera.viewport_size.y;
    if (edge_distance < pixel_world * 1.5) {
        return vec4f(0.48, 0.49, 0.52, 1.0);
    }
    return vec4f(0.91, 0.91, 0.89, 1.0);
}

struct TileVertexOutput {
    @builtin(position) position: vec4f,
    @location(0) local: vec2f,
    @location(1) @interpolate(flat) layer: u32,
    @location(2) @interpolate(flat) pixel_extent: vec2u,
}

@vertex
fn tile_vs(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) origin: vec2f,
    @location(1) extent: vec2f,
    @location(2) layer: u32,
) -> TileVertexOutput {
    let corners = array(
        vec2f(0.0, 1.0),
        vec2f(0.0, 0.0),
        vec2f(1.0, 0.0),
        vec2f(0.0, 1.0),
        vec2f(1.0, 0.0),
        vec2f(1.0, 1.0),
    );
    let local = corners[vertex_index];
    let world = origin + local * extent;
    let size = view_size();
    let ndc = (world - camera.center) * vec2f(2.0 / size.x, 2.0 / size.y);

    var output: TileVertexOutput;
    output.position = vec4f(ndc, 0.0, 1.0);
    output.local = local;
    output.layer = layer;
    output.pixel_extent = vec2u(extent);
    return output;
}

@fragment
fn tile_fs(input: TileVertexOutput) -> @location(0) vec4f {
    let size = vec2i(input.pixel_extent);
    let pixel = clamp(vec2i(input.local * vec2f(size)), vec2i(0), size - vec2i(1));
    return textureLoad(tile_texture, pixel, i32(input.layer), 0);
}

struct CursorVertexOutput {
    @builtin(position) position: vec4f,
    @location(0) delta_world: vec2f,
}

@vertex
fn cursor_vs(@builtin(vertex_index) vertex_index: u32) -> CursorVertexOutput {
    let corners = array(
        vec2f(-1.0, 1.0),
        vec2f(-1.0, -1.0),
        vec2f(1.0, -1.0),
        vec2f(-1.0, 1.0),
        vec2f(1.0, -1.0),
        vec2f(1.0, 1.0),
    );
    let pixel_world = view_size().y / camera.viewport_size.y;
    let outer_radius = max(cursor.half_extents.x, cursor.half_extents.y) + pixel_world * 3.5;
    let delta = corners[vertex_index] * outer_radius;
    let world = cursor.position + delta;
    let size = view_size();
    let ndc = (world - camera.center) * vec2f(2.0 / size.x, 2.0 / size.y);

    var output: CursorVertexOutput;
    output.position = vec4f(ndc, 0.0, 1.0);
    output.delta_world = delta;
    return output;
}

@fragment
fn cursor_fs(input: CursorVertexOutput) -> @location(0) vec4f {
    if (cursor.visible < 0.5) {
        discard;
    }

    let pixel_world = view_size().y / camera.viewport_size.y;
    let local = vec2f(
        dot(input.delta_world, cursor.direction),
        dot(input.delta_world, vec2f(-cursor.direction.y, cursor.direction.x)),
    );
    var signed_distance: f32;
    if (cursor.shape < 0.5) {
        signed_distance = length(input.delta_world) - cursor.half_extents.x;
    } else if (cursor.shape < 1.5) {
        let q = abs(local) - cursor.half_extents;
        signed_distance = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0);
    } else {
        let normalized = length(local / cursor.half_extents);
        signed_distance = (normalized - 1.0) * min(cursor.half_extents.x, cursor.half_extents.y);
    }
    let edge_distance = abs(signed_distance);
    let outline = 1.0 - smoothstep(pixel_world * 2.2, pixel_world * 3.2, edge_distance);
    let bright_core = 1.0 - smoothstep(pixel_world * 0.65, pixel_world * 1.35, edge_distance);
    let rgb = mix(vec3f(0.025), cursor.color.rgb, bright_core);
    let alpha = outline * cursor.color.a;
    return vec4f(rgb * alpha, alpha);
}
