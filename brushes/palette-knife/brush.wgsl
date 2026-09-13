fn brush_coverage(input: BrushInput) -> f32 {
    let surface = media_surface(input, 3u);
    let distance = surface.distance; let pressure = surface.pressure;
    let radius = surface.radius; let aspect = surface.aspect;
    let direction = surface.direction; let normal = surface.normal;
    let world = input.point;
    let paper = paper_sample(world,0.45);
    let drag = paper_sample(vec2<f32>(dot(world,direction),dot(world,normal)*0.15),2.8);
    let edge_depth = (1.0-paper)*min(min(1.5+radius*0.08,3.0),radius*aspect*0.5);
    let edge = clamp(0.5-distance-edge_depth,0.0,1.0);
    let body = smoothstep(0.0,max(radius*aspect*0.7,1.0),-distance);
    let scrape = smoothstep(0.2,0.65,drag+body*0.3+pressure*0.15);
    return edge*(0.65+0.35*pressure)*(0.7+0.3*scrape)*(0.88+0.12*paper);
}
