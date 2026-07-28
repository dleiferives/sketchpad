struct VertexInput {
    @location(0) position: vec2<f32>,
}

@vertex
fn blade_vs(input: VertexInput) -> @builtin(position) vec4<f32> {
    let canvas_size = vec2<f32>(2048.0, 2048.0);
    let normalized = input.position / canvas_size;
    let clip = vec2<f32>(
        normalized.x * 2.0 - 1.0,
        1.0 - normalized.y * 2.0,
    );
    return vec4<f32>(clip, 0.0, 1.0);
}

@fragment
fn blade_fs() -> @location(0) vec4<f32> {
    // Premultiplied linear RGBA. This first geometry proof is intentionally
    // opaque so overlapping convex sweep pieces describe their union without
    // accumulating opacity at internal tessellation boundaries.
    return vec4<f32>(0.04, 0.08, 0.20, 1.0);
}
