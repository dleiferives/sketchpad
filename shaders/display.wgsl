struct Camera {
    center: vec2f,
    zoom: f32,
    canvas_size: f32,
    viewport_size: vec2f,
    _padding: vec2f,
}

@group(0) @binding(0) var sdf_texture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> camera: Camera;

fn load_sdf(coord: vec2i) -> f32 {
    return textureLoad(sdf_texture, coord, 0).r;
}

fn sample_sdf(uv: vec2f) -> f32 {
    let size_u = textureDimensions(sdf_texture);
    let size = vec2f(size_u);
    let texel = uv * size - vec2f(0.5);
    let x0 = i32(clamp(floor(texel.x), 0.0, size.x - 1.0));
    let y0 = i32(clamp(floor(texel.y), 0.0, size.y - 1.0));
    let x1 = min(x0 + 1, i32(size_u.x) - 1);
    let y1 = min(y0 + 1, i32(size_u.y) - 1);
    let fx = fract(texel.x);
    let fy = fract(texel.y);

    let top = mix(load_sdf(vec2i(x0, y0)), load_sdf(vec2i(x1, y0)), fx);
    let bottom = mix(load_sdf(vec2i(x0, y1)), load_sdf(vec2i(x1, y1)), fx);
    return mix(top, bottom, fy);
}

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
    let pos = array(
        vec2f(-1.0, 1.0), vec2f(-1.0, -1.0), vec2f(1.0, -1.0),
        vec2f(-1.0, 1.0), vec2f(1.0, -1.0), vec2f(1.0, 1.0),
    );
    return vec4f(pos[vi], 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) screen_pos: vec4f) -> @location(0) vec4f {
    let uv = vec2f(
        screen_pos.x / camera.viewport_size.x,
        1.0 - screen_pos.y / camera.viewport_size.y,
    );

    let view_w = camera.canvas_size / camera.zoom;
    let view_h = view_w;
    let aspect = camera.viewport_size.x / camera.viewport_size.y;
    let adjusted_view_w = view_h * aspect;

    let wx = camera.center.x + (uv.x - 0.5) * adjusted_view_w;
    let wy = camera.center.y + (uv.y - 0.5) * view_h;

    let tx = wx / camera.canvas_size;
    let ty = wy / camera.canvas_size;

    var d: f32 = camera.canvas_size * 2.0;
    if (tx >= 0.0 && tx <= 1.0 && ty >= 0.0 && ty <= 1.0) {
        d = sample_sdf(vec2f(tx, ty));
    }

    let pixel_world = view_h / camera.viewport_size.y;
    let aa_width = max(pixel_world * 0.75, 0.0001);
    let inside = 1.0 - smoothstep(-aa_width, aa_width, d);
    let bg = vec3f(0.12, 0.12, 0.13);
    let fg = vec3f(0.95, 0.95, 0.97);
    return vec4f(mix(bg, fg, inside), 1.0);
}
