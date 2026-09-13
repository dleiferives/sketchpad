fn brush_coverage(input: BrushInput) -> f32 {
    let surface = media_surface(input, 3u);
    let distance = surface.distance; let pressure = surface.pressure;
    let radius = surface.radius; let aspect = surface.aspect;
    let world = input.point;
    let paper = paper_sample(world,0.45);
    // Loaded paint is opaque. Grain cuts occasional grooves and chips out of
    // the deposit instead of reducing the alpha of every painted pixel.
    // Extend fixed paper tooth along travel, independently of blade orientation.
    // Local symmetric offsets preserve phase on curves and on a 180-degree return.
    let motion = input.end - input.start;
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
