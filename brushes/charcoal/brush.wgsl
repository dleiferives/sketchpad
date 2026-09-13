fn brush_coverage(input: BrushInput) -> f32 {
    let surface = media_surface(input, 4u);
    let distance = surface.distance; let pressure = surface.pressure;
    let radius = surface.radius; let aspect = surface.aspect;
    let direction = surface.direction; let normal = surface.normal;
    let world = input.point;
    let paper = paper_sample(world,0.45);
        let edge = clamp(0.5-distance-(1.0-paper)*min(radius*aspect,0.85),0.0,1.0);
        let body = paper_sample(world,3.2);
        let tooth = smoothstep(0.12,0.86,paper*0.78+body*0.22+pressure*0.08);
        return edge*sqrt(pressure)*(0.28+0.65*pressure)*(0.65+0.35*tooth);
}
