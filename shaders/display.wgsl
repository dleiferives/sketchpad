struct Camera {
    offset: vec2f,
    zoom: f32,
    canvas_size: f32,
}

@group(0) @binding(0) var sdf_texture: texture_2d<f32>;
@group(0) @binding(1) var sdf_sampler: sampler;
@group(0) @binding(2) var<uniform> camera: Camera;

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
    let sz = vec2f(1280.0, 720.0);
    let uv = vec2f(screen_pos.x / sz.x, 1.0 - screen_pos.y / sz.y);

    let view_w = camera.canvas_size / camera.zoom;
    let view_h = camera.canvas_size / camera.zoom;

    let wx = camera.offset.x + (uv.x - 0.5) * view_w;
    let wy = camera.offset.y + (uv.y - 0.5) * view_h;

    let tx = wx / camera.canvas_size;
    let ty = wy / camera.canvas_size;

    var d: f32 = 128.0;
    if (tx >= 0.0 && tx <= 1.0 && ty >= 0.0 && ty <= 1.0) {
        let c = textureSample(sdf_texture, sdf_sampler, vec2f(tx, ty));
        d = (c.r * 2.0 - 1.0) * 128.0;
    }

    let px = 2.0 / camera.zoom;
    let inside = 1.0 - smoothstep(-px, px, d);
    let bg = vec3f(0.12, 0.12, 0.13);
    let fg = vec3f(0.95, 0.95, 0.97);
    return vec4f(mix(bg, fg, inside), 1.0);
}
