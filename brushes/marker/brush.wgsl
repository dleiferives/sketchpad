fn brush_coverage(input: BrushInput) -> f32 {
    let surface = media_surface(input, 2u);
    let distance = surface.distance; let pressure = surface.pressure;
    let radius = surface.radius; let aspect = surface.aspect;
    let direction = surface.direction; let normal = surface.normal;
    let world = input.point;
    return clamp(0.5-distance,0.0,1.0)*(0.38+0.12*pressure);
}
