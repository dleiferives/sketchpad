@group(0) @binding(0) var sdf_texture: texture_2d<f32>;
@group(0) @binding(1) var sdf_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) tex_coords: vec2f,
}

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let pos = array(
        vec2f(-1.0, 1.0), vec2f(-1.0, -1.0), vec2f(1.0, -1.0),
        vec2f(-1.0, 1.0), vec2f(1.0, -1.0), vec2f(1.0, 1.0),
    );
    let uv = array(
        vec2f(0.0, 0.0), vec2f(0.0, 1.0), vec2f(1.0, 1.0),
        vec2f(0.0, 0.0), vec2f(1.0, 1.0), vec2f(1.0, 0.0),
    );
    var out: VertexOutput;
    out.position = vec4f(pos[vi], 0.0, 1.0);
    out.tex_coords = uv[vi];
    return out;
}

@fragment
fn fs(in: VertexOutput) -> @location(0) vec4f {
    let c = textureSample(sdf_texture, sdf_sampler, in.tex_coords);
    let d = (c.r * 2.0 - 1.0) * 128.0;

    let px = 2.0;
    let inside = 1.0 - smoothstep(-px, px, d);
    let bg = vec3f(0.12, 0.12, 0.13);
    let fg = vec3f(0.95, 0.95, 0.97);
    return vec4f(mix(bg, fg, inside), 1.0);
}
