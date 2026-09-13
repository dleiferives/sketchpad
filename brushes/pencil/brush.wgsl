fn brush_coverage(input: BrushInput) -> f32 {
    let surface = media_surface(input, 1u);
    let distance = surface.distance; let pressure = surface.pressure;
    let radius = surface.radius; let aspect = surface.aspect;
    let direction = surface.direction; let normal = surface.normal;
    let world = input.point;
    let paper = paper_sample(world,0.35);
    let core = clamp(-distance/max(radius*aspect,0.25),0.0,1.0);
    let tooth = smoothstep(0.08,0.88,paper);
    let deposit = sqrt(pressure)*(0.5+0.5*tooth)*(0.45+0.55*sqrt(core));
    return clamp(0.5-distance,0.0,1.0)*deposit;
}
