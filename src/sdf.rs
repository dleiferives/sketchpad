pub const CANVAS_SIZE: u32 = 1024;
pub const FIELD_WIDTH: u32 = CANVAS_SIZE;
pub const FIELD_HEIGHT: u32 = CANVAS_SIZE;
pub const EMPTY_DISTANCE: f32 = CANVAS_SIZE as f32 * 2.0;

#[derive(Debug, Clone)]
pub struct SdfField {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
    dirty: bool,
}

impl SdfField {
    pub fn new(width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0);
        Self {
            width,
            height,
            data: vec![EMPTY_DISTANCE; (width * height) as usize],
            dirty: true,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    pub fn reset(&mut self) {
        self.data.fill(EMPTY_DISTANCE);
        self.dirty = true;
    }

    pub fn sample_position(&self, x: u32, y: u32) -> [f32; 2] {
        assert!(x < self.width && y < self.height);
        [
            (x as f32 + 0.5) * CANVAS_SIZE as f32 / self.width as f32,
            (y as f32 + 0.5) * CANVAS_SIZE as f32 / self.height as f32,
        ]
    }

    pub fn stamp_circle(&mut self, center: [f32; 2], radius: f32) {
        if radius <= 0.0 {
            return;
        }

        let sample_spacing =
            (CANVAS_SIZE as f32 / self.width as f32).min(CANVAS_SIZE as f32 / self.height as f32);
        let margin = sample_spacing * 2.0;
        let Some((min_x, max_x)) = self.sample_range(
            center[0] - radius - margin,
            center[0] + radius + margin,
            self.width,
        ) else {
            return;
        };
        let Some((min_y, max_y)) = self.sample_range(
            center[1] - radius - margin,
            center[1] + radius + margin,
            self.height,
        ) else {
            return;
        };

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let sample = self.sample_position(x, y);
                let dx = sample[0] - center[0];
                let dy = sample[1] - center[1];
                let distance = (dx * dx + dy * dy).sqrt() - radius;
                let index = (y * self.width + x) as usize;
                self.data[index] = self.data[index].min(distance);
            }
        }
        self.dirty = true;
    }

    fn sample_range(&self, min_world: f32, max_world: f32, count: u32) -> Option<(u32, u32)> {
        let scale = count as f32 / CANVAS_SIZE as f32;
        let min_index = ((min_world * scale) - 0.5).ceil() as i32;
        let max_index = ((max_world * scale) - 0.5).floor() as i32;
        let min_index = min_index.clamp(0, count as i32 - 1);
        let max_index = max_index.clamp(0, count as i32 - 1);
        (min_index <= max_index).then_some((min_index as u32, max_index as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_field_is_empty() {
        let field = SdfField::new(8, 8);
        assert!(field.data.iter().all(|&value| value == EMPTY_DISTANCE));
    }

    #[test]
    fn circle_stamp_has_negative_center_and_positive_edge() {
        let mut field = SdfField::new(128, 128);
        field.stamp_circle([512.0, 512.0], 100.0);

        let center_index = (64 * field.width + 64) as usize;
        assert!(field.data[center_index] < 0.0);

        let edge_index = (64 * field.width + 76) as usize;
        assert!(field.data[edge_index] > -5.0);
    }

    #[test]
    fn circles_union_with_minimum_distance() {
        let mut field = SdfField::new(128, 128);
        field.stamp_circle([400.0, 512.0], 80.0);
        let before = field.data[(64 * field.width + 64) as usize];
        field.stamp_circle([624.0, 512.0], 80.0);
        let after = field.data[(64 * field.width + 64) as usize];

        assert!(after <= before);
    }
}
