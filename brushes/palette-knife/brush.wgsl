fn brush_coverage(input: BrushInput) -> f32 {
    let surface = media_surface(input, 3u);
    let distance = surface.distance; let pressure = surface.pressure;
    let radius = surface.radius; let aspect = surface.aspect;
    let direction = surface.direction; let normal = surface.normal;
    let world = input.point;
    let paper = paper_sample(world,0.45);
    // Loaded paint is opaque. Grain cuts occasional grooves and chips out of
    // the deposit instead of reducing the alpha of every painted pixel.
    let drag = paper_sample(vec2<f32>(dot(world,direction),dot(world,normal)*0.45),0.65);
    let breakup = paper_sample(world,2.2);
    let edge_depth = (1.0-paper)*min(min(2.0+radius*0.10,4.0),radius*aspect*0.5);
    let edge = clamp(0.5-distance-edge_depth,0.0,1.0);
    let body = smoothstep(0.0,max(radius*aspect*0.7,1.0),-distance);
    let threshold = 0.25 + (1.0-pressure)*0.04 + (1.0-body)*0.07;
    let scrape = 1.0-smoothstep(threshold,threshold+0.06,drag+breakup*0.10);
    // Subpixel nibs cannot resolve internal grooves. Keep their coverage stable.
    let detail = smoothstep(0.5,2.0,radius*aspect);
    return edge*(1.0-scrape*detail);
}
