fn brush_coverage(input: BrushInput) -> f32 {
    return clamp(0.5-distance_to_variable_capsule(input.point,input.start,input.end,input.radii),0.0,1.0);
}
