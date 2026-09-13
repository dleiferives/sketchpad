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
    @location(5) world_offset: vec2<f32>,
    @location(6) brush_data: vec4<f32>,
    @location(7) tilts: vec4<f32>,
}

struct RoundVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) physical: vec2<f32>,
    @location(1) @interpolate(flat) start_point: vec2<f32>,
    @location(2) @interpolate(flat) end_point: vec2<f32>,
    @location(3) @interpolate(flat) radii: vec2<f32>,
    @location(4) @interpolate(flat) clip_min: vec2<f32>,
    @location(5) @interpolate(flat) clip_max: vec2<f32>,
    @location(6) @interpolate(flat) world_offset: vec2<f32>,
    @location(7) @interpolate(flat) brush_data: vec4<f32>,
    @location(8) @interpolate(flat) tilts: vec4<f32>,
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
    let fringe = vec2<f32>(select(0.5, 3.0, input.brush_data.x > 0.5));
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
    output.world_offset = input.world_offset;
    output.brush_data = input.brush_data;
    output.tilts = input.tilts;
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

@group(0) @binding(1) var<storage, read> paper_words: array<u32>;
fn paper_texel(cell: vec2<i32>) -> f32 {
    let c = vec2<u32>((cell % vec2<i32>(512) + vec2<i32>(512)) % vec2<i32>(512));
    let index = c.y*512u+c.x;
    return f32((paper_words[index/4u] >> ((index%4u)*8u)) & 255u) / 255.0;
}
fn paper_sample(point: vec2<f32>, scale: f32) -> f32 {
    let p=point/scale;
    let base=vec2<i32>(floor(p)); let f=fract(p);
    return mix(mix(paper_texel(base),paper_texel(base+vec2<i32>(1,0)),f.x),mix(paper_texel(base+vec2<i32>(0,1)),paper_texel(base+vec2<i32>(1,1)),f.x),f.y);
}

fn box_value(base: vec4<f32>, slope: vec4<f32>, t: f32) -> f32 {
    let v = base + slope * t;
    return max(max(v.x, v.y), max(v.z, v.w));
}
fn box_sweep_distance(point: vec2<f32>, end: vec2<f32>, radii: vec2<f32>) -> f32 {
    let dr = radii.y-radii.x;
    let base = vec4<f32>(point.x, -point.x, point.y, -point.y) - vec4<f32>(radii.x);
    let slope = vec4<f32>(-end.x, end.x, -end.y, end.y) - vec4<f32>(dr);
    var best = min(box_value(base, slope, 0.0), box_value(base, slope, 1.0));
    for (var i = 0u; i < 4u; i++) { for (var j = i+1u; j < 4u; j++) {
        let divisor = slope[i]-slope[j];
        if abs(divisor) > 1e-6 {
            let t = clamp((base[j]-base[i])/divisor, 0.0, 1.0);
            best = min(best, box_value(base, slope, t));
        }
    }}
    return best;
}

fn media_coverage(input: RoundVertexOutput) -> f32 {
    let kind = u32(input.brush_data.x);
    let delta = input.end_point - input.start_point;
    let relative = input.physical - input.start_point;
    var t = clamp(dot(relative, delta) / max(dot(delta, delta), 1e-12), 0.0, 1.0);
    var radii = input.radii;
    if dot(delta, delta) <= 1e-12 { t = 1.0; radii = vec2<f32>(input.radii.y); }
    let pressure = mix(input.brush_data.y, input.brush_data.z, t);
    let tilt = mix(input.tilts.xy, input.tilts.zw, t);
    let side = min(length(tilt), 1.0);
    let weight = clamp((side - 0.08) / 0.24, 0.0, 1.0);
    var direction = vec2<f32>(0.8, 0.6)*(1.0-weight) + tilt/max(length(tilt), 1e-6)*weight;
    if length(direction) > 1e-6 { direction = normalize(direction); } else { direction = vec2<f32>(0.8,0.6); }
    var aspect = 1.0 - 0.45 * side;
    if kind == 2u { aspect = 0.42; }
    if kind == 3u { aspect = 0.32; }
    if kind == 4u { aspect = 0.72 - 0.25 * side; }
    let normal = vec2<f32>(-direction.y, direction.x);
    let local = vec2<f32>(dot(relative, direction), dot(relative, normal) / aspect);
    let end = vec2<f32>(dot(delta, direction), dot(delta, normal) / aspect);
    var distance = distance_to_variable_capsule(local, vec2<f32>(0.0), end, radii);
    if kind == 2u || kind == 3u { distance = box_sweep_distance(local, end, radii * 0.92); }
    distance *= aspect;
    if kind == 2u { return clamp(0.5-distance,0.0,1.0)*(0.38+0.12*pressure); }
    let radius = mix(radii.x, radii.y, t);
    let world = input.physical + input.world_offset;
    let paper = paper_sample(world, select(0.45,0.35,kind==1u));
    if kind == 1u {
        let core = clamp(-distance/max(radius*aspect,0.25),0.0,1.0);
        let tooth = smoothstep(0.08,0.88,paper);
        let deposit = sqrt(pressure)*(0.5+0.5*tooth)*(0.45+0.55*sqrt(core));
        return clamp(0.5-distance,0.0,1.0)*deposit;
    }
    if kind == 4u {
        let edge = clamp(0.5-distance-(1.0-paper)*min(radius*aspect,0.85),0.0,1.0);
        let body = paper_sample(world,3.2);
        let tooth = smoothstep(0.12,0.86,paper*0.78+body*0.22+pressure*0.08);
        return edge*sqrt(pressure)*(0.28+0.65*pressure)*(0.65+0.35*tooth);
    }
    // Loaded paint is opaque. Grain cuts occasional grooves and chips out of
    // the deposit instead of reducing the alpha of every painted pixel.
    // Extend fixed paper tooth along travel, independently of blade orientation.
    // Local symmetric offsets preserve phase on curves and on a 180-degree return.
    let motion = delta;
    let motion_length = length(motion);
    var step = vec2<f32>(0.0);
    if motion_length > 1e-6 { step = motion / motion_length; }
    var drag = 1.0;
    for (var i = -2; i <= 2; i += 1) {
        drag = min(drag, paper_sample(world + step * f32(i), 0.65));
    }
    let breakup = paper_sample(world,2.2);
    let edge_depth = (1.0-paper)*min(min(2.0+radius*0.10,4.0),radius*aspect*0.5);
    let edge = clamp(0.5-distance-edge_depth,0.0,1.0);
    let body = smoothstep(0.0,max(radius*aspect*0.7,1.0),-distance);
    let threshold = 0.14 + (1.0-pressure)*0.04 + (1.0-body)*0.07;
    let scrape = 1.0-smoothstep(threshold,threshold+0.06,drag+breakup*0.10);
    // Subpixel nibs cannot resolve internal grooves. Keep their coverage stable.
    let detail = smoothstep(0.5,2.0,radius*aspect);
    return edge*(1.0-scrape*detail);
}

@fragment
fn round_fs(input: RoundVertexOutput) -> @location(0) f32 {
    if any(input.physical < input.clip_min) || any(input.physical >= input.clip_max) {
        discard;
    }
    if input.brush_data.x > 0.5 { return media_coverage(input); }
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
