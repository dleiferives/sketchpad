const MAX_POLYGON_VERTICES: usize = 8;
const GEOMETRY_EPSILON: f32 = 1.0e-5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BladePose {
    center: [f32; 2],
    direction: [f32; 2],
    half_extents: [f32; 2],
}

impl BladePose {
    pub fn new(
        center: [f32; 2],
        direction: [f32; 2],
        half_extents: [f32; 2],
    ) -> Option<Self> {
        if center.iter().any(|value| !value.is_finite())
            || direction.iter().any(|value| !value.is_finite())
            || half_extents
                .iter()
                .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return None;
        }
        let length_squared =
            direction[0] * direction[0] + direction[1] * direction[1];
        if length_squared <= GEOMETRY_EPSILON * GEOMETRY_EPSILON {
            return None;
        }
        let inverse_length = length_squared.sqrt().recip();
        Some(Self {
            center,
            direction: [
                direction[0] * inverse_length,
                direction[1] * inverse_length,
            ],
            half_extents,
        })
    }

    pub const fn center(self) -> [f32; 2] {
        self.center
    }

    pub const fn direction(self) -> [f32; 2] {
        self.direction
    }

    pub const fn half_extents(self) -> [f32; 2] {
        self.half_extents
    }

    pub fn aligned_to(self, reference: Self) -> Self {
        let dot = dot(self.direction, reference.direction);
        if dot < 0.0 {
            Self {
                direction: [-self.direction[0], -self.direction[1]],
                ..self
            }
        } else {
            self
        }
    }

    pub fn sub_blade(self, index: usize, count: usize) -> Option<Self> {
        if count == 0 || index >= count {
            return None;
        }
        let lane_width = self.half_extents[0] * 2.0 / count as f32;
        let lane_start = -self.half_extents[0] + lane_width * index as f32;
        let lane_center = lane_start + lane_width * 0.5;
        Some(Self {
            center: [
                self.center[0] + self.direction[0] * lane_center,
                self.center[1] + self.direction[1] * lane_center,
            ],
            direction: self.direction,
            half_extents: [lane_width * 0.5, self.half_extents[1]],
        })
    }

    fn interpolate(self, other: Self, amount: f32) -> Self {
        let direction = [
            self.direction[0] + (other.direction[0] - self.direction[0]) * amount,
            self.direction[1] + (other.direction[1] - self.direction[1]) * amount,
        ];
        let inverse_length =
            (direction[0] * direction[0] + direction[1] * direction[1])
                .sqrt()
                .recip();
        Self {
            center: [
                self.center[0] + (other.center[0] - self.center[0]) * amount,
                self.center[1] + (other.center[1] - self.center[1]) * amount,
            ],
            direction: [
                direction[0] * inverse_length,
                direction[1] * inverse_length,
            ],
            half_extents: [
                self.half_extents[0]
                    + (other.half_extents[0] - self.half_extents[0]) * amount,
                self.half_extents[1]
                    + (other.half_extents[1] - self.half_extents[1]) * amount,
            ],
        }
    }

    fn corners(self) -> [[f32; 2]; 4] {
        let normal = [-self.direction[1], self.direction[0]];
        let major = [
            self.direction[0] * self.half_extents[0],
            self.direction[1] * self.half_extents[0],
        ];
        let minor = [
            normal[0] * self.half_extents[1],
            normal[1] * self.half_extents[1],
        ];
        [
            [
                self.center[0] - major[0] - minor[0],
                self.center[1] - major[1] - minor[1],
            ],
            [
                self.center[0] + major[0] - minor[0],
                self.center[1] + major[1] - minor[1],
            ],
            [
                self.center[0] + major[0] + minor[0],
                self.center[1] + major[1] + minor[1],
            ],
            [
                self.center[0] - major[0] + minor[0],
                self.center[1] - major[1] + minor[1],
            ],
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConvexPolygon {
    vertices: [[f32; 2]; MAX_POLYGON_VERTICES],
    vertex_count: u8,
    bounds: [f32; 4],
}

impl ConvexPolygon {
    pub fn vertices(&self) -> &[[f32; 2]] {
        &self.vertices[..self.vertex_count as usize]
    }

    pub const fn bounds(self) -> [f32; 4] {
        self.bounds
    }

    pub fn scanline_span(self, y: f32) -> Option<[f32; 2]> {
        if y < self.bounds[1] || y >= self.bounds[3] {
            return None;
        }
        let vertices = &self.vertices[..self.vertex_count as usize];
        let mut minimum = f32::INFINITY;
        let mut maximum = f32::NEG_INFINITY;
        for index in 0..vertices.len() {
            let first = vertices[index];
            let second = vertices[(index + 1) % vertices.len()];
            let crosses = (first[1] <= y && second[1] > y)
                || (second[1] <= y && first[1] > y);
            if !crosses {
                continue;
            }
            let amount = (y - first[1]) / (second[1] - first[1]);
            let x = first[0] + (second[0] - first[0]) * amount;
            minimum = minimum.min(x);
            maximum = maximum.max(x);
        }
        (minimum < maximum).then_some([minimum, maximum])
    }

    #[cfg(test)]
    fn contains(self, point: [f32; 2]) -> bool {
        let vertices = &self.vertices[..self.vertex_count as usize];
        (0..vertices.len()).all(|index| {
            let first = vertices[index];
            let second = vertices[(index + 1) % vertices.len()];
            cross(first, second, point) >= -GEOMETRY_EPSILON
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BladeSweep {
    polygon: ConvexPolygon,
}

impl BladeSweep {
    pub fn between(previous: BladePose, current: BladePose) -> Self {
        let current = current.aligned_to(previous);
        let previous_corners = previous.corners();
        let current_corners = current.corners();
        let points = [
            previous_corners[0],
            previous_corners[1],
            previous_corners[2],
            previous_corners[3],
            current_corners[0],
            current_corners[1],
            current_corners[2],
            current_corners[3],
        ];
        Self {
            polygon: convex_hull(points),
        }
    }

    pub const fn polygon(self) -> ConvexPolygon {
        self.polygon
    }
}

pub fn for_each_subdivided_sweep(
    previous: BladePose,
    current: BladePose,
    maximum_angle_radians: f32,
    maximum_segments: usize,
    mut emit: impl FnMut(BladeSweep),
) -> usize {
    assert!(maximum_angle_radians.is_finite() && maximum_angle_radians > 0.0);
    assert!(maximum_segments > 0);
    let current = current.aligned_to(previous);
    let angle = dot(previous.direction, current.direction)
        .clamp(-1.0, 1.0)
        .acos();
    let segment_count = ((angle / maximum_angle_radians).ceil() as usize)
        .clamp(1, maximum_segments);
    let mut first = previous;
    for index in 1..=segment_count {
        let second = previous.interpolate(current, index as f32 / segment_count as f32);
        emit(BladeSweep::between(first, second));
        first = second;
    }
    segment_count
}

fn convex_hull(mut points: [[f32; 2]; MAX_POLYGON_VERTICES]) -> ConvexPolygon {
    points.sort_by(|first, second| {
        first[0]
            .total_cmp(&second[0])
            .then_with(|| first[1].total_cmp(&second[1]))
    });

    let mut unique = [[0.0; 2]; MAX_POLYGON_VERTICES];
    let mut unique_count = 0;
    for point in points {
        if unique_count == 0
            || squared_distance(unique[unique_count - 1], point)
                > GEOMETRY_EPSILON * GEOMETRY_EPSILON
        {
            unique[unique_count] = point;
            unique_count += 1;
        }
    }
    assert!(unique_count >= 3, "a valid blade pose has at least four corners");

    let mut hull = [[0.0; 2]; MAX_POLYGON_VERTICES * 2];
    let mut count = 0;
    for &point in &unique[..unique_count] {
        while count >= 2 && cross(hull[count - 2], hull[count - 1], point) <= GEOMETRY_EPSILON {
            count -= 1;
        }
        hull[count] = point;
        count += 1;
    }
    let lower_count = count;
    for &point in unique[..unique_count - 1].iter().rev() {
        while count > lower_count
            && cross(hull[count - 2], hull[count - 1], point) <= GEOMETRY_EPSILON
        {
            count -= 1;
        }
        hull[count] = point;
        count += 1;
    }
    count -= 1;
    assert!(count <= MAX_POLYGON_VERTICES);

    let mut vertices = [[0.0; 2]; MAX_POLYGON_VERTICES];
    vertices[..count].copy_from_slice(&hull[..count]);
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for vertex in &vertices[..count] {
        bounds[0] = bounds[0].min(vertex[0]);
        bounds[1] = bounds[1].min(vertex[1]);
        bounds[2] = bounds[2].max(vertex[0]);
        bounds[3] = bounds[3].max(vertex[1]);
    }
    ConvexPolygon {
        vertices,
        vertex_count: count as u8,
        bounds,
    }
}

fn cross(first: [f32; 2], second: [f32; 2], third: [f32; 2]) -> f32 {
    (second[0] - first[0]) * (third[1] - first[1])
        - (second[1] - first[1]) * (third[0] - first[0])
}

fn dot(first: [f32; 2], second: [f32; 2]) -> f32 {
    first[0] * second[0] + first[1] * second[1]
}

fn squared_distance(first: [f32; 2], second: [f32; 2]) -> f32 {
    let x = second[0] - first[0];
    let y = second[1] - first[1];
    x * x + y * y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(center: [f32; 2], direction: [f32; 2]) -> BladePose {
        BladePose::new(center, direction, [4.0, 1.0]).unwrap()
    }

    #[test]
    fn pose_validation_normalizes_direction() {
        assert_eq!(
            pose([2.0, 3.0], [3.0, 4.0]).direction(),
            [0.6, 0.8]
        );
        assert!(BladePose::new([0.0, 0.0], [0.0, 0.0], [1.0, 1.0]).is_none());
        assert!(BladePose::new([f32::NAN, 0.0], [1.0, 0.0], [1.0, 1.0]).is_none());
        assert!(BladePose::new([0.0, 0.0], [1.0, 0.0], [0.0, 1.0]).is_none());
    }

    #[test]
    fn sub_blades_partition_the_major_axis() {
        let whole = pose([10.0, 20.0], [1.0, 0.0]);
        let first = whole.sub_blade(0, 4).unwrap();
        let last = whole.sub_blade(3, 4).unwrap();
        assert_eq!(first.center(), [7.0, 20.0]);
        assert_eq!(last.center(), [13.0, 20.0]);
        assert_eq!(first.half_extents(), [1.0, 1.0]);
        assert!(whole.sub_blade(4, 4).is_none());
        assert!(whole.sub_blade(0, 0).is_none());
    }

    #[test]
    fn swept_hull_contains_both_contact_rectangles() {
        let first = pose([10.0, 10.0], [1.0, 0.0]);
        let second = pose([20.0, 15.0], [0.0, 1.0]);
        let polygon = BladeSweep::between(first, second).polygon();
        for corner in first.corners().into_iter().chain(second.corners()) {
            assert!(polygon.contains(corner), "missing corner {corner:?}");
        }
        assert_eq!(polygon.vertices().len(), 6);
    }

    #[test]
    fn scanline_span_is_conservative_for_axis_aligned_sweep() {
        let first = pose([10.0, 10.0], [1.0, 0.0]);
        let second = pose([20.0, 10.0], [1.0, 0.0]);
        let polygon = BladeSweep::between(first, second).polygon();
        assert_eq!(polygon.scanline_span(10.0), Some([6.0, 24.0]));
        assert_eq!(polygon.scanline_span(8.9), None);
        assert_eq!(polygon.scanline_span(10.9), Some([6.0, 24.0]));
    }

    #[test]
    fn rotation_subdivision_is_bounded_and_reaches_the_last_pose() {
        let first = pose([0.0, 0.0], [1.0, 0.0]);
        let second = pose([8.0, 3.0], [0.0, 1.0]);
        let mut sweeps = Vec::new();
        let count = for_each_subdivided_sweep(
            first,
            second,
            15.0_f32.to_radians(),
            32,
            |sweep| sweeps.push(sweep),
        );
        assert_eq!(count, 6);
        assert_eq!(sweeps.len(), 6);
        for corner in second.corners() {
            assert!(sweeps.last().unwrap().polygon().contains(corner));
        }
    }

    #[test]
    fn blade_axis_sign_does_not_trigger_a_half_turn() {
        let first = pose([0.0, 0.0], [1.0, 0.0]);
        let second = pose([10.0, 0.0], [-1.0, 0.0]);
        let count = for_each_subdivided_sweep(
            first,
            second,
            5.0_f32.to_radians(),
            32,
            |_| {},
        );
        assert_eq!(count, 1);
    }
}
