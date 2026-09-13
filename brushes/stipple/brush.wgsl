fn brush_coverage(input: BrushInput) -> f32 {
    let distance = distance_to_variable_capsule(input.point,input.start,input.end,input.radii);
    let cell = vec2<i32>(floor(input.point/3.0));
    let grain = brush_noise(cell);
    let pressure = mix(input.pressures.x,input.pressures.y,0.5);
    return clamp(0.5-distance,0.0,1.0)*select(0.0,0.8,grain < pressure*0.65);
}
