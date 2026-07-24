struct VertexInput {
    @location(0) position: vec2f,
    @location(1) tex_coords: vec2f,
}

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) tex_coords: vec2f,
}

struct Camera {
    offset: vec2f,
    zoom: f32,
    canvas_size: f32,
}

@group(0) @binding(0) var sdf_texture: texture_2d<f32>;
@group(0) @binding(1) var sdf_sampler: sampler;
@group(0) @binding(2) var<uniform> camera: Camera;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4f(in.position, 0.0, 1.0);
    out.tex_coords = in.tex_coords;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4f {
    let canvas = camera.canvas_size;

    let world_x = (in.tex_coords.x - 0.5) * canvas / camera.zoom + camera.offset.x;
    let world_y = (in.tex_coords.y - 0.5) * canvas / camera.zoom + camera.offset.y;

    let tex_x = world_x / canvas;
    let tex_y = world_y / canvas;

    if (tex_x < 0.0 || tex_x > 1.0 || tex_y < 0.0 || tex_y > 1.0) {
        return vec4f(0.12, 0.12, 0.13, 1.0);
    }

    let d = textureSample(sdf_texture, sdf_sampler, vec2f(tex_x, tex_y)).r;
    let px = 2.0 / (camera.zoom * 720.0);
    let half_px = 0.5 * px;
    let inside = smoothstep(half_px, -half_px, d);
    let bg = vec3f(0.12, 0.12, 0.13);
    let fg = vec3f(0.95, 0.95, 0.97);
    return vec4f(mix(bg, fg, inside), 1.0);
}
