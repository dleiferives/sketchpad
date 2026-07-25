#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum ToolKind {
    Pen,
    Eraser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum TabletPhase {
    Hover,
    Down,
    Move,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TabletSample {
    pub device_id: u16,
    pub tool: ToolKind,
    pub position: [f32; 2],
    pub pressure: f32,
    pub tilt: [f32; 2],
    pub distance: f32,
    pub timestamp_millis: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TabletEvent {
    Sample {
        phase: TabletPhase,
        sample: TabletSample,
    },
    BackendError(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabletAxisInfo {
    pub number: u16,
    pub label: String,
    pub min: f64,
    pub max: f64,
    pub resolution: u32,
}

impl TabletAxisInfo {
    pub fn normalize_unit(&self, value: f64) -> f32 {
        normalize_range(value, self.min, self.max, 0.0, 1.0)
    }

    pub fn normalize_signed(&self, value: f64) -> f32 {
        if self.min.is_finite()
            && self.max.is_finite()
            && value.is_finite()
            && self.min < 0.0
            && self.max > 0.0
        {
            return if value < 0.0 {
                (value / -self.min).clamp(-1.0, 0.0) as f32
            } else {
                (value / self.max).clamp(0.0, 1.0) as f32
            };
        }
        normalize_range(value, self.min, self.max, -1.0, 1.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabletDeviceInfo {
    pub id: u16,
    pub name: String,
    pub tool: ToolKind,
    pub axes: Vec<TabletAxisInfo>,
}

impl TabletDeviceInfo {
    pub fn axis(&self, label: &str) -> Option<&TabletAxisInfo> {
        self.axes
            .iter()
            .find(|axis| axis.label.eq_ignore_ascii_case(label))
    }
}

fn normalize_range(value: f64, min: f64, max: f64, out_min: f32, out_max: f32) -> f32 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() || max <= min {
        return out_min;
    }
    let unit = ((value - min) / (max - min)).clamp(0.0, 1.0) as f32;
    out_min + unit * (out_max - out_min)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(min: f64, max: f64) -> TabletAxisInfo {
        TabletAxisInfo {
            number: 2,
            label: "Abs Pressure".to_owned(),
            min,
            max,
            resolution: 1,
        }
    }

    #[test]
    fn unit_normalization_clamps_to_declared_range() {
        let axis = axis(0.0, 8192.0);
        assert_eq!(axis.normalize_unit(-1.0), 0.0);
        assert_eq!(axis.normalize_unit(4096.0), 0.5);
        assert_eq!(axis.normalize_unit(9000.0), 1.0);
    }

    #[test]
    fn signed_normalization_maps_tilt_extremes() {
        let axis = axis(-64.0, 63.0);
        assert_eq!(axis.normalize_signed(-64.0), -1.0);
        assert_eq!(axis.normalize_signed(0.0), 0.0);
        assert_eq!(axis.normalize_signed(63.0), 1.0);
    }

    #[test]
    fn degenerate_ranges_fail_closed() {
        assert_eq!(axis(3.0, 3.0).normalize_unit(3.0), 0.0);
        assert_eq!(axis(f64::NAN, 3.0).normalize_signed(0.0), -1.0);
    }
}
