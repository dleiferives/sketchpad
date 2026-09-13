//! Procedural, canvas-anchored media. Keep the equations in round_mask.wgsl in sync.
use crate::{round_geometry::RoundSweepGeometry, stroke::RoundContact};

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrushTip {
    #[default]
    HardRound = 0,
    Pencil = 1,
    Marker = 2,
    PaletteKnife = 3,
    Charcoal = 4,
}

impl BrushTip {
    pub fn radius(self, diameter: f32, pressure: f32, tilt: [f32; 2], minimum: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let side = tilt[0].hypot(tilt[1]).min(1.0);
        let scale = match self {
            Self::HardRound => p.max(minimum),
            Self::Pencil => (0.28 + 0.72 * p.sqrt()) * (0.35 + 0.65 * side),
            Self::Marker => 0.72 + 0.28 * p,
            Self::PaletteKnife => 0.55 + 0.45 * p,
            Self::Charcoal => (0.55 + 0.45 * p.sqrt()) * (0.65 + 0.35 * side),
        };
        diameter * 0.5 * scale
    }

    /// Major-axis direction and minor/major ratio. Mouse uses a useful fixed nib angle.
    pub fn shape(self, tilt: [f32; 2]) -> ([f32; 2], f32) {
        let side = tilt[0].hypot(tilt[1]).min(1.0);
        let weight = ((side - 0.08) / 0.24).clamp(0.0, 1.0);
        let length = tilt[0].hypot(tilt[1]).max(1e-6);
        let direction = [
            0.8 * (1.0 - weight) + tilt[0] / length * weight,
            0.6 * (1.0 - weight) + tilt[1] / length * weight,
        ];
        let length = direction[0].hypot(direction[1]);
        let direction = if length > 1e-6 {
            [direction[0] / length, direction[1] / length]
        } else {
            [0.8, 0.6]
        };
        let aspect = match self {
            Self::HardRound => 1.0,
            Self::Pencil => 1.0 - 0.65 * side,
            Self::Marker => 0.42,
            Self::PaletteKnife => 0.22,
            Self::Charcoal => 0.65 - 0.25 * side,
        };
        (direction, aspect)
    }

    pub fn coverage(self, mut from: RoundContact, to: RoundContact, point: [f32; 2]) -> f32 {
        if self == Self::HardRound {
            return RoundSweepGeometry::new(from, to)
                .expect("validated contact")
                .coverage(point);
        }
        if (to.center[0] - from.center[0]).powi(2) + (to.center[1] - from.center[1]).powi(2)
            <= 1e-12
        {
            // Earlier contact coverage is already in the union mask. A stationary
            // pressure/tilt change deposits the new tip, including its new radius.
            from = to;
        }
        let delta = [to.center[0] - from.center[0], to.center[1] - from.center[1]];
        let relative = [point[0] - from.center[0], point[1] - from.center[1]];
        let dot = |a: [f32; 2], b: [f32; 2]| a[0] * b[0] + a[1] * b[1];
        let t = (dot(relative, delta) / dot(delta, delta).max(1e-12)).clamp(0.0, 1.0);
        let mix = |a: f32, b: f32| a + (b - a) * t;
        let pressure = mix(from.dynamics[0], to.dynamics[0]);
        let (direction, aspect) = self.shape([
            mix(from.dynamics[1], to.dynamics[1]),
            mix(from.dynamics[2], to.dynamics[2]),
        ]);
        let transform = |v: [f32; 2]| {
            [
                dot(v, direction),
                (-v[0] * direction[1] + v[1] * direction[0]) / aspect,
            ]
        };
        let local = transform(relative);
        let end = transform(delta);
        let mut a = from;
        let mut b = to;
        a.center = [0.0; 2];
        b.center = end;
        let distance = if matches!(self, Self::Marker | Self::PaletteKnife) {
            box_sweep_distance(local, end, [a.radius * 0.92, b.radius * 0.92])
        } else {
            RoundSweepGeometry::new(a, b)
                .expect("validated contact")
                .signed_distance(local)
        } * aspect;
        let radius = mix(from.radius, to.radius);
        let edge = match self {
            Self::Pencil => 0.65,
            Self::Charcoal => (radius * 0.12).max(0.8),
            _ => 0.5,
        };
        let coverage = ((0.5 - distance) / (2.0 * edge)).clamp(0.0, 1.0);
        let fine = grain(point, 1.0);
        let coarse = grain(point, 3.0);
        let texture = match self {
            Self::Pencil => {
                ((pressure * 1.15 - fine * 0.8 - coarse * 0.18) * 2.2).clamp(0.0, 1.0)
                    * (0.3 + pressure * 0.65)
            }
            Self::Marker => 0.38 + 0.12 * pressure,
            Self::PaletteKnife => {
                let lane = grain(
                    [
                        point[0] * direction[0] + point[1] * direction[1],
                        (-point[0] * direction[1] + point[1] * direction[0]) * 0.06,
                    ],
                    1.0,
                );
                ((pressure * 0.95 - lane * 0.85 - fine * 0.25) * 3.0).clamp(0.0, 1.0)
            }
            Self::Charcoal => {
                ((pressure * 1.1 - coarse * 0.6 - fine * 0.35) * 2.5).clamp(0.0, 1.0) * 0.88
            }
            Self::HardRound => 1.0,
        };
        coverage * texture
    }
}

fn grain(point: [f32; 2], scale: f32) -> f32 {
    let x = (point[0] / scale).floor() as i32 as u32;
    let y = (point[1] / scale).floor() as i32 as u32;
    let mut h = x.wrapping_mul(0x1f123bb5) ^ y.wrapping_mul(0x5f356495) ^ 0x91e10da5;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb352d);
    h ^= h >> 15;
    (h & 65535) as f32 / 65535.0
}

// Minimum of four affine half-plane distances over the swept chisel. Evaluating
// their intersections gives square ends without sample-spaced stamp scallops.
fn box_sweep_distance(point: [f32; 2], end: [f32; 2], radii: [f32; 2]) -> f32 {
    let dr = radii[1] - radii[0];
    let base = [
        point[0] - radii[0],
        -point[0] - radii[0],
        point[1] - radii[0],
        -point[1] - radii[0],
    ];
    let slope = [-end[0] - dr, end[0] - dr, -end[1] - dr, end[1] - dr];
    let at = |t: f32| {
        (0..4)
            .map(|i| base[i] + slope[i] * t)
            .fold(f32::NEG_INFINITY, f32::max)
    };
    let mut best = at(0.0).min(at(1.0));
    for i in 0..4 {
        for j in (i + 1)..4 {
            let divisor = slope[i] - slope[j];
            if divisor.abs() > 1e-6 {
                let t = ((base[j] - base[i]) / divisor).clamp(0.0, 1.0);
                best = best.min(at(t));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contact(tip: BrushTip, pressure: f32, tilt: [f32; 2]) -> RoundContact {
        RoundContact {
            center: [64.0, 64.0],
            radius: tip.radius(60.0, pressure, tilt, 0.05),
            dynamics: [pressure, tilt[0], tilt[1]],
            elapsed_micros: 0,
        }
    }
    fn sum(tip: BrushTip, c: RoundContact) -> f32 {
        (0..128)
            .flat_map(|y| {
                (0..128).map(move |x| tip.coverage(c, c, [x as f32 + 0.5, y as f32 + 0.5]))
            })
            .sum()
    }
    #[test]
    fn dry_media_pressure_fills_tooth_and_tilt_widens_shading() {
        for tip in [BrushTip::Pencil, BrushTip::Charcoal] {
            let light = sum(tip, contact(tip, 0.2, [0.0; 2]));
            let firm = sum(tip, contact(tip, 1.0, [0.0; 2]));
            assert!(light > 0.0 && firm > light * 2.0);
            let upright = contact(tip, 0.7, [0.0; 2]);
            let tilted = contact(tip, 0.7, [0.8, 0.0]);
            assert!(tilted.radius > upright.radius);
            let values: Vec<_> = (60..69)
                .map(|x| tip.coverage(upright, upright, [x as f32 + 0.5, 64.5]))
                .collect();
            assert!(values.windows(2).any(|v| (v[0] - v[1]).abs() > 0.05));
        }
    }
    #[test]
    fn marker_is_uniform_translucent_and_round_is_solid() {
        let c = contact(BrushTip::Marker, 1.0, [0.0; 2]);
        assert_eq!(BrushTip::Marker.coverage(c, c, [64.5, 64.5]), 0.5);
        assert_eq!(BrushTip::Marker.coverage(c, c, [66.5, 64.5]), 0.5);
        assert_eq!(BrushTip::HardRound.coverage(c, c, [64.5, 64.5]), 1.0);
    }
    #[test]
    fn stationary_pressure_and_tilt_change_deposits_new_contact() {
        for tip in [
            BrushTip::Pencil,
            BrushTip::Marker,
            BrushTip::PaletteKnife,
            BrushTip::Charcoal,
        ] {
            let from = contact(tip, 0.2, [0.0; 2]);
            let to = contact(tip, 0.9, [0.7, 0.0]);
            for y in 45..84 {
                for x in 45..84 {
                    let p = [x as f32 + 0.5, y as f32 + 0.5];
                    assert_eq!(tip.coverage(from, to, p), tip.coverage(to, to, p));
                }
            }
        }
    }

    #[test]
    fn tiny_chisel_fringe_stays_inside_scheduler_bounds() {
        for tip in [
            BrushTip::Pencil,
            BrushTip::Marker,
            BrushTip::PaletteKnife,
            BrushTip::Charcoal,
        ] {
            let mut c = contact(tip, 1.0, [0.0; 2]);
            c.radius = 0.3;
            for y in 610..670 {
                for x in 610..670 {
                    let p = [x as f32 / 10.0, y as f32 / 10.0];
                    if (p[0] - c.center[0]).hypot(p[1] - c.center[1]) >= c.radius + 3.0 {
                        assert_eq!(tip.coverage(c, c, p), 0.0);
                    }
                }
            }
        }
    }

    #[test]
    fn collinear_sample_splitting_does_not_change_constant_contact_marks() {
        for tip in [
            BrushTip::HardRound,
            BrushTip::Pencil,
            BrushTip::Marker,
            BrushTip::PaletteKnife,
            BrushTip::Charcoal,
        ] {
            let mut a = contact(tip, 0.8, [0.0; 2]);
            a.center = [20.0, 64.0];
            let b = RoundContact {
                center: [100.0, 64.0],
                ..a
            };
            let mid = RoundContact {
                center: [60.0, 64.0],
                ..a
            };
            for y in 35..90 {
                for x in 0..128 {
                    let p = [x as f32 + 0.5, y as f32 + 0.5];
                    let whole = tip.coverage(a, b, p);
                    let split = tip.coverage(a, mid, p).max(tip.coverage(mid, b, p));
                    assert!(
                        (whole - split).abs() < 0.0001,
                        "{tip:?}: {whole} vs {split} at {p:?}"
                    );
                }
            }
        }
    }
}
