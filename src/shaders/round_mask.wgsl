struct MaskUniform {
    page_size: vec2<f32>,
    _padding: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> mask: MaskUniform;

struct RoundVertexInput {
    @location(0) start_point: vec2<f32>,
    @location(1) end_point: vec2<f32>,
    @location(2) radii: vec2<f32>,
    @location(3) clip_min: vec2<f32>,
    @location(4) clip_max: vec2<f32>,
}

struct RoundVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) physical: vec2<f32>,
    @location(1) @interpolate(flat) start_point: vec2<f32>,
    @location(2) @interpolate(flat) end_point: vec2<f32>,
    @location(3) @interpolate(flat) radii: vec2<f32>,
    @location(4) @interpolate(flat) clip_min: vec2<f32>,
    @location(5) @interpolate(flat) clip_max: vec2<f32>,
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

fn physical_to_clip(physical: vec2<f32>) -> vec4<f32> {
    let unit = physical / mask.page_size;
    return vec4<f32>(unit.x * 2.0 - 1.0, 1.0 - unit.y * 2.0, 0.0, 1.0);
}

@vertex
fn round_vs(
    @builtin(vertex_index) vertex_index: u32,
    input: RoundVertexInput,
) -> RoundVertexOutput {
    let fringe = vec2<f32>(0.5);
    let bounds_min = min(
        input.start_point - vec2<f32>(input.radii.x),
        input.end_point - vec2<f32>(input.radii.y),
    ) - fringe;
    let bounds_max = max(
        input.start_point + vec2<f32>(input.radii.x),
        input.end_point + vec2<f32>(input.radii.y),
    ) + fringe;
    let physical = mix(bounds_min, bounds_max, quad_corner(vertex_index));

    var output: RoundVertexOutput;
    output.position = physical_to_clip(physical);
    output.physical = physical;
    output.start_point = input.start_point;
    output.end_point = input.end_point;
    output.radii = input.radii;
    output.clip_min = input.clip_min;
    output.clip_max = input.clip_max;
    return output;
}

fn distance_to_variable_capsule(
    point: vec2<f32>,
    start_point: vec2<f32>,
    end_point: vec2<f32>,
    radii: vec2<f32>,
) -> f32 {
    let delta = end_point - start_point;
    let length_squared = dot(delta, delta);
    if length_squared <= 1.0e-12 {
        if radii.x >= radii.y {
            return length(point - start_point) - radii.x;
        }
        return length(point - end_point) - radii.y;
    }

    let segment_length = sqrt(length_squared);
    let radius_delta = radii.x - radii.y;
    if segment_length <= abs(radius_delta) {
        if radii.x >= radii.y {
            return length(point - start_point) - radii.x;
        }
        return length(point - end_point) - radii.y;
    }

    let direction = delta / segment_length;
    let relative = point - start_point;
    let local = vec2<f32>(
        abs(dot(relative, vec2<f32>(-direction.y, direction.x))),
        dot(relative, direction),
    );
    let slope = radius_delta / segment_length;
    let tangent = sqrt(max(0.0, 1.0 - slope * slope));
    let region = -slope * local.x + tangent * local.y;
    if region < 0.0 {
        return length(local) - radii.x;
    }
    if region > tangent * segment_length {
        return length(vec2<f32>(local.x, local.y - segment_length)) - radii.y;
    }
    return tangent * local.x + slope * local.y - radii.x;
}

@fragment
fn round_fs(input: RoundVertexOutput) -> @location(0) f32 {
    if any(input.physical < input.clip_min) || any(input.physical >= input.clip_max) {
        discard;
    }
    let distance = distance_to_variable_capsule(
        input.physical,
        input.start_point,
        input.end_point,
        input.radii,
    );
    return clamp(0.5 - distance, 0.0, 1.0);
}

struct ClearVertexInput {
    @location(0) bounds_min: vec2<f32>,
    @location(1) bounds_max: vec2<f32>,
}

@vertex
fn clear_vs(
    @builtin(vertex_index) vertex_index: u32,
    input: ClearVertexInput,
) -> @builtin(position) vec4<f32> {
    let physical = mix(input.bounds_min, input.bounds_max, quad_corner(vertex_index));
    return physical_to_clip(physical);
}

@fragment
fn clear_fs() -> @location(0) f32 {
    return 0.0;
}
