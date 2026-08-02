struct CommitUniform {
    page_size: vec2<f32>,
    opacity: f32,
    _padding: f32,
    color: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> commit: CommitUniform;

@group(0) @binding(1)
var stroke_mask: texture_2d<f32>;

struct SlotVertexInput {
    @location(0) bounds_min: vec2<f32>,
    @location(1) bounds_max: vec2<f32>,
}

struct SlotVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) physical: vec2<f32>,
}

fn quad_corner(vertex_index: u32) -> vec2<f32> {
    let corners = array(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    return corners[vertex_index];
}

@vertex
fn slot_vs(
    @builtin(vertex_index) vertex_index: u32,
    input: SlotVertexInput,
) -> SlotVertexOutput {
    let physical = mix(input.bounds_min, input.bounds_max, quad_corner(vertex_index));
    let unit = physical / commit.page_size;

    var output: SlotVertexOutput;
    output.position = vec4<f32>(unit.x * 2.0 - 1.0, 1.0 - unit.y * 2.0, 0.0, 1.0);
    output.physical = physical;
    return output;
}

@fragment
fn commit_fs(input: SlotVertexOutput) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(input.physical));
    let effect = commit.opacity * textureLoad(stroke_mask, pixel, 0).x;
    return vec4<f32>(commit.color.rgb * effect, effect);
}

@fragment
fn clear_fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}
