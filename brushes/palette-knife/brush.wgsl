// Material ABI v2: coverage and deposited thickness in document-pixel units.
// Grain shapes the paint surface. It cannot exclude the same interior pixels.
fn brush_deposit(input: BrushInput) -> vec2<f32> {
    let surface = media_surface(input, 3u);
    let radius = surface.radius;
    let paper = paper_sample(input.point, 0.45);
    let edge_depth = (1.0-paper) * min(1.3, radius * 0.025);
    let coverage = clamp(0.5 - surface.distance - edge_depth, 0.0, 1.0);
    let motion = input.end - input.start;
    let direction = motion / max(length(motion), 1e-6);
    var tooth = 0.0;
    // Elongated microrelief follows travel, independently of the held blade.
    for (var i = -4; i <= 4; i += 1) {
        tooth += paper_sample(input.point + direction * f32(i) * 1.5, 0.65) / 9.0;
    }
    let interior = smoothstep(0.0, max(radius * surface.aspect * 0.65, 1.0), -surface.distance);
    let rim = exp(-pow((surface.distance + 1.8) / 1.3, 2.0));
    let detail = smoothstep(0.5, 3.0, radius * surface.aspect);
    let height = (0.65 + 0.35 * surface.pressure) * (0.6 + detail * (1.15 * tooth + 0.7 * rim)) * mix(0.65, 1.0, interior);
    return vec2<f32>(coverage, height);
}
