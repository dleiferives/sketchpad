use crate::sdf::{SdfField, FIELD_HEIGHT, FIELD_WIDTH};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircleStamp {
    pub center: [f32; 2],
    pub radius: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operation {
    AddCircle(CircleStamp),
}

pub struct Document {
    pub operations: Vec<Operation>,
    pub field: SdfField,
}

impl Document {
    pub fn new() -> Self {
        Self {
            operations: Vec::new(),
            field: SdfField::new(FIELD_WIDTH, FIELD_HEIGHT),
        }
    }

    pub fn add_circle(&mut self, center: [f32; 2], radius: f32) {
        let stamp = CircleStamp { center, radius };
        self.operations.push(Operation::AddCircle(stamp));
        self.field.stamp_circle(center, radius);
    }

    pub fn rebuild_field(&mut self) {
        self.field.reset();
        for operation in &self.operations {
            match operation {
                Operation::AddCircle(stamp) => self.field.stamp_circle(stamp.center, stamp.radius),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_matches_incremental_cache() {
        let mut document = Document::new();
        document.add_circle([256.0, 256.0], 40.0);
        document.add_circle([512.0, 512.0], 80.0);
        let incremental = document.field.data.clone();

        document.rebuild_field();

        assert_eq!(document.field.data, incremental);
    }
}
