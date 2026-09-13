//! Directional brush-tip orientation in document coordinates. Pen tilt arrives in screen axes.
//! A tip axis has 180-degree symmetry; reversals must not spin the footprint.
#[derive(Clone, Copy, Debug)]
pub struct TipOrientation {
    axis: [f32; 2],
    anchor: Option<[f32; 2]>,
    tilted: bool,
    directed: bool,
}
impl Default for TipOrientation {
    fn default() -> Self {
        Self {
            axis: [1.0, 0.0],
            anchor: None,
            tilted: false,
            directed: false,
        }
    }
}
impl TipOrientation {
    pub fn axis(&self) -> [f32; 2] {
        self.axis
    }

    pub fn update(&mut self, position: [f32; 2], tilt: [f32; 2]) -> [f32; 2] {
        if !position.into_iter().chain(tilt).all(f32::is_finite) {
            return self.axis;
        }
        let previous = self.anchor.unwrap_or(position);
        let motion = [position[0] - previous[0], position[1] - previous[1]];
        let distance = motion[0].hypot(motion[1]);
        if self.anchor.is_none() || distance >= 0.25 {
            self.anchor = Some(position);
        }
        let side = tilt[0].hypot(tilt[1]);
        self.tilted = side >= if self.tilted { 0.08 } else { 0.12 };
        let (mut target, weight) = if self.tilted {
            // Long tip axis is perpendicular to projected pen azimuth.
            // Screen +Y points down; document +Y points up.
            ([tilt[1] / side, tilt[0] / side], 1.0)
        } else if distance >= 0.25 {
            (
                [-motion[1] / distance, motion[0] / distance],
                if self.directed {
                    1.0 - (-distance / 4.0).exp()
                } else {
                    1.0
                },
            )
        } else {
            return self.axis;
        };
        if target[0] * self.axis[0] + target[1] * self.axis[1] < 0.0 {
            target = [-target[0], -target[1]];
        }
        let next = [
            self.axis[0] * (1.0 - weight) + target[0] * weight,
            self.axis[1] * (1.0 - weight) + target[1] * weight,
        ];
        let length = next[0].hypot(next[1]);
        self.axis = [next[0] / length, next[1] / length];
        self.directed = true;
        self.axis
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn motion_pauses_reversals_and_pen_azimuth() {
        let mut pose = TipOrientation::default();
        pose.update([0.0, 0.0], [0.0; 2]);
        let horizontal = pose.update([20.0, 0.0], [0.0; 2]);
        assert!(horizontal[1].abs() > 0.999);
        assert_eq!(pose.update([20.0, 0.0], [0.0; 2]), horizontal);
        assert_eq!(pose.update([0.0, 0.0], [0.0; 2]), horizontal);
        let turned = pose.update([0.0, 40.0], [0.0; 2]);
        assert!(turned[0].abs() > 0.999);
        let pen = pose.update([0.0, 40.0], [0.8, 0.0]);
        assert!(pen[1].abs() > 0.999);
        assert_eq!(pose.update([0.0, 40.0], [-0.8, 0.0]), pen);
        assert_eq!(pose.update([0.0, 40.0], [0.0; 2]), pen);
    }
    #[test]
    fn subpixel_samples_accumulate_and_invalid_input_preserves_pose() {
        let mut pose = TipOrientation::default();
        for i in 0..100 {
            pose.update([i as f32 * 0.01, 0.0], [0.0; 2]);
        }
        assert!(pose.axis()[1].abs() > 0.999);
        let before = pose.axis();
        assert_eq!(pose.update([f32::NAN, 0.0], [0.0; 2]), before);
    }
}
