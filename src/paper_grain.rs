//! David Revoy's CC BY 4.0 paper grain; attribution in assets/brushes/README.md.
//! Packed four texels per word on GPU; shared wrap/bilinear sampling on CPU.
pub const SIDE: usize = 512;
pub const BYTES: &[u8; SIDE * SIDE] = include_bytes!("../assets/brushes/revoy-paper.gray");

pub fn sample(point: [f32; 2], scale: f32) -> f32 {
    let p = [point[0] / scale, point[1] / scale];
    let base = [p[0].floor() as i32, p[1].floor() as i32];
    let f = [p[0] - p[0].floor(), p[1] - p[1].floor()];
    let at = |x: i32, y: i32| {
        BYTES[(y.rem_euclid(512) * 512 + x.rem_euclid(512)) as usize] as f32 / 255.0
    };
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
    mix(
        mix(at(base[0], base[1]), at(base[0] + 1, base[1]), f[0]),
        mix(at(base[0], base[1] + 1), at(base[0] + 1, base[1] + 1), f[0]),
        f[1],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paper_wraps_continuously_in_both_directions() {
        for p in [[0.0, 0.0], [-0.5, 20.25], [511.9, -10.0]] {
            assert!((sample(p, 1.0) - sample([p[0] + 512.0, p[1] - 512.0], 1.0)).abs() < 0.0001);
        }
        let left = sample([-0.0001, 17.25], 1.0);
        let right = sample([0.0001, 17.25], 1.0);
        assert!((left - right).abs() < 0.0001);
    }
}
