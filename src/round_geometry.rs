use crate::stroke::{RoundContact, RoundPathCommand, StrokeAccumulation, StrokeError};

const COVERAGE_FRINGE: f32 = 0.5;
const GEOMETRY_EPSILON: f32 = 1.0e-6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundStrokeBounds {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl RoundStrokeBounds {
    fn for_contact(contact: RoundContact) -> Self {
        let extent = contact.radius + COVERAGE_FRINGE;
        Self {
            min: [contact.center[0] - extent, contact.center[1] - extent],
            max: [contact.center[0] + extent, contact.center[1] + extent],
        }
    }

    fn include(&mut self, contact: RoundContact) {
        let other = Self::for_contact(contact);
        self.min[0] = self.min[0].min(other.min[0]);
        self.min[1] = self.min[1].min(other.min[1]);
        self.max[0] = self.max[0].max(other.max[0]);
        self.max[1] = self.max[1].max(other.max[1]);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundSweepGeometry {
    from: RoundContact,
    to: RoundContact,
}

impl RoundSweepGeometry {
    pub fn new(from: RoundContact, to: RoundContact) -> Result<Self, StrokeError> {
        if !valid_contact(from) || !valid_contact(to) {
            return Err(StrokeError::InvalidContact);
        }
        Ok(Self { from, to })
    }

    pub const fn from(self) -> RoundContact {
        self.from
    }

    pub const fn to(self) -> RoundContact {
        self.to
    }

    pub fn signed_distance(self, point: [f32; 2]) -> f32 {
        debug_assert!(point.iter().all(|value| value.is_finite()));
        let delta = [
            self.to.center[0] - self.from.center[0],
            self.to.center[1] - self.from.center[1],
        ];
        let length_squared = delta[0] * delta[0] + delta[1] * delta[1];
        if length_squared <= GEOMETRY_EPSILON * GEOMETRY_EPSILON {
            let contact = if self.from.radius >= self.to.radius {
                self.from
            } else {
                self.to
            };
            return distance(point, contact.center) - contact.radius;
        }

        let length = length_squared.sqrt();
        let radius_delta = self.from.radius - self.to.radius;
        if length <= radius_delta.abs() {
            let contact = if self.from.radius >= self.to.radius {
                self.from
            } else {
                self.to
            };
            return distance(point, contact.center) - contact.radius;
        }

        let direction = [delta[0] / length, delta[1] / length];
        let relative = [
            point[0] - self.from.center[0],
            point[1] - self.from.center[1],
        ];
        let local = [
            (relative[0] * -direction[1] + relative[1] * direction[0]).abs(),
            relative[0] * direction[0] + relative[1] * direction[1],
        ];
        let slope = radius_delta / length;
        let tangent = (1.0 - slope * slope).max(0.0).sqrt();
        let region = -slope * local[0] + tangent * local[1];

        if region < 0.0 {
            (local[0] * local[0] + local[1] * local[1]).sqrt() - self.from.radius
        } else if region > tangent * length {
            let end_y = local[1] - length;
            (local[0] * local[0] + end_y * end_y).sqrt() - self.to.radius
        } else {
            tangent * local[0] + slope * local[1] - self.from.radius
        }
    }

    pub fn coverage(self, point: [f32; 2]) -> f32 {
        coverage_from_distance(self.signed_distance(point))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct RoundSubpath {
    contacts: Vec<RoundContact>,
}

impl RoundSubpath {
    fn coverage_runs(
        &self,
        point: [f32; 2],
        mut visit: impl FnMut(f32) -> Result<(), StrokeError>,
    ) -> Result<(), StrokeError> {
        if self.contacts.len() == 1 {
            let coverage = disc_coverage(self.contacts[0], point);
            if coverage > 0.0 {
                visit(coverage)?;
            }
            return Ok(());
        }

        let mut active_run = None::<f32>;
        for contacts in self.contacts.windows(2) {
            let coverage = RoundSweepGeometry::new(contacts[0], contacts[1])?.coverage(point);
            if coverage > 0.0 {
                active_run = Some(active_run.map_or(coverage, |current| current.max(coverage)));
            } else if let Some(coverage) = active_run.take() {
                visit(coverage)?;
            }
        }
        if let Some(coverage) = active_run {
            visit(coverage)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoundStrokeGeometry {
    complete: Vec<RoundSubpath>,
    active: Option<RoundSubpath>,
    bounds: Option<RoundStrokeBounds>,
}

impl RoundStrokeGeometry {
    pub fn apply_commands(
        &mut self,
        commands: impl IntoIterator<Item = RoundPathCommand>,
    ) -> Result<(), StrokeError> {
        for command in commands {
            self.apply_command(command)?;
        }
        Ok(())
    }

    pub fn apply_command(&mut self, command: RoundPathCommand) -> Result<(), StrokeError> {
        match command {
            RoundPathCommand::Begin(contact) => {
                if self.active.is_some() {
                    return Err(StrokeError::PathAlreadyActive);
                }
                if !valid_contact(contact) {
                    return Err(StrokeError::InvalidContact);
                }
                self.include(contact);
                self.active = Some(RoundSubpath {
                    contacts: vec![contact],
                });
            }
            RoundPathCommand::Sweep { from, to } => {
                if !valid_contact(from) || !valid_contact(to) {
                    return Err(StrokeError::InvalidContact);
                }
                let active = self.active.as_mut().ok_or(StrokeError::PathNotActive)?;
                if active.contacts.last().copied() != Some(from) {
                    return Err(StrokeError::DiscontinuousPath);
                }
                active.contacts.push(to);
                self.include(to);
            }
            RoundPathCommand::End { at, .. } => {
                let active = self.active.take().ok_or(StrokeError::PathNotActive)?;
                if active.contacts.last().copied() != Some(at) {
                    self.active = Some(active);
                    return Err(StrokeError::DiscontinuousPath);
                }
                self.complete.push(active);
            }
        }
        Ok(())
    }

    pub const fn bounds(&self) -> Option<RoundStrokeBounds> {
        self.bounds
    }

    pub fn is_empty(&self) -> bool {
        self.complete.is_empty() && self.active.is_none()
    }

    pub fn is_complete(&self) -> bool {
        !self.complete.is_empty() && self.active.is_none()
    }

    pub fn subpath_count(&self) -> usize {
        self.complete.len() + usize::from(self.active.is_some())
    }

    pub fn accumulated_mask_at(
        &self,
        point: [f32; 2],
        accumulation: StrokeAccumulation,
    ) -> Result<f32, StrokeError> {
        if point.iter().any(|value| !value.is_finite()) {
            return Err(StrokeError::InvalidPoint);
        }
        let mut accumulated = 0.0;
        let mut add_subpath = |subpath: &RoundSubpath| {
            subpath.coverage_runs(point, |coverage| {
                accumulated = accumulation.add_coverage(accumulated, coverage)?;
                Ok(())
            })
        };
        for subpath in &self.complete {
            add_subpath(subpath)?;
        }
        if let Some(active) = &self.active {
            add_subpath(active)?;
        }
        Ok(accumulated)
    }

    fn include(&mut self, contact: RoundContact) {
        match &mut self.bounds {
            Some(bounds) => bounds.include(contact),
            None => self.bounds = Some(RoundStrokeBounds::for_contact(contact)),
        }
    }
}

fn valid_contact(contact: RoundContact) -> bool {
    contact.center.iter().all(|value| value.is_finite())
        && contact.radius.is_finite()
        && contact.radius > 0.0
}

fn disc_coverage(contact: RoundContact, point: [f32; 2]) -> f32 {
    coverage_from_distance(distance(point, contact.center) - contact.radius)
}

fn coverage_from_distance(distance: f32) -> f32 {
    (COVERAGE_FRINGE - distance).clamp(0.0, 1.0)
}

fn distance(first: [f32; 2], second: [f32; 2]) -> f32 {
    let delta = [first[0] - second[0], first[1] - second[1]];
    (delta[0] * delta[0] + delta[1] * delta[1]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stroke::{RoundBrushRecipeV1, StrokeMaterial, TimedBrushSample};

    fn contact(center: [f32; 2], radius: f32) -> RoundContact {
        RoundContact {
            center,
            radius,
            elapsed_micros: 0,
        }
    }

    fn build_path(points: &[[f32; 2]]) -> RoundStrokeGeometry {
        let recipe = RoundBrushRecipeV1::with_minimum_pressure_fraction(
            StrokeMaterial::paint([0.0; 3], 1.0, 1.0).unwrap(),
            2.0,
            1.0,
        )
        .unwrap();
        let samples: Vec<_> = points
            .iter()
            .enumerate()
            .map(|(index, point)| TimedBrushSample::new(*point, 1.0, [0.0; 2], index as u64))
            .collect();
        let mut path = crate::stroke::ContinuousRoundPath::begin(recipe, samples[0]).unwrap();
        for sample in &samples[1..] {
            path.update(*sample).unwrap();
        }
        path.finish().unwrap();
        let mut geometry = RoundStrokeGeometry::default();
        geometry
            .apply_commands(path.take_batch().into_commands())
            .unwrap();
        geometry
    }

    #[test]
    fn even_capsule_has_round_caps_and_a_solid_body() {
        let sweep =
            RoundSweepGeometry::new(contact([0.0, 0.0], 2.0), contact([10.0, 0.0], 2.0)).unwrap();
        assert_eq!(sweep.coverage([5.0, 0.0]), 1.0);
        assert_eq!(sweep.coverage([-2.0, 0.0]), 0.5);
        assert_eq!(sweep.coverage([5.0, 2.0]), 0.5);
        assert_eq!(sweep.coverage([5.0, 3.0]), 0.0);
    }

    #[test]
    fn contained_radius_change_reduces_to_the_larger_disc() {
        let sweep =
            RoundSweepGeometry::new(contact([0.0, 0.0], 5.0), contact([1.0, 0.0], 1.0)).unwrap();
        assert_eq!(sweep.signed_distance([0.0, 0.0]), -5.0);
        assert_eq!(sweep.coverage([5.0, 0.0]), 0.5);
        assert_eq!(sweep.coverage([6.0, 0.0]), 0.0);
    }

    #[test]
    fn consecutive_sweeps_are_one_flow_traversal_at_a_join() {
        let geometry = build_path(&[[-10.0, 0.0], [0.0, 0.0], [10.0, 0.0]]);
        let accumulation = StrokeAccumulation::OpticalDensity { flow: 0.5 };
        let mask = geometry
            .accumulated_mask_at([0.0, 0.0], accumulation)
            .unwrap();
        assert!((accumulation.resolve(mask).unwrap() - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn nonconsecutive_branches_accumulate_at_a_self_crossing() {
        let geometry = build_path(&[[-10.0, 0.0], [10.0, 0.0], [10.0, 10.0], [-10.0, -10.0]]);
        let flow = StrokeAccumulation::OpticalDensity { flow: 0.5 };
        let density = geometry.accumulated_mask_at([0.0, 0.0], flow).unwrap();
        assert!((flow.resolve(density).unwrap() - 0.75).abs() < 1.0e-6);

        let union = StrokeAccumulation::CoverageUnion;
        assert_eq!(geometry.accumulated_mask_at([0.0, 0.0], union), Ok(1.0));
    }

    #[test]
    fn pressure_breaks_form_separate_subpaths() {
        let recipe =
            RoundBrushRecipeV1::new(StrokeMaterial::paint([0.0; 3], 1.0, 0.5).unwrap(), 2.0)
                .unwrap();
        let mut path = crate::stroke::ContinuousRoundPath::begin(
            recipe,
            TimedBrushSample::new([0.0; 2], 1.0, [0.0; 2], 0),
        )
        .unwrap();
        path.update(TimedBrushSample::new([0.0; 2], 0.0, [0.0; 2], 1))
            .unwrap();
        path.update(TimedBrushSample::new([0.0; 2], 1.0, [0.0; 2], 2))
            .unwrap();
        path.finish().unwrap();

        let mut geometry = RoundStrokeGeometry::default();
        geometry
            .apply_commands(path.take_batch().into_commands())
            .unwrap();
        assert_eq!(geometry.subpath_count(), 2);
        let accumulation = recipe.material().accumulation();
        let mask = geometry
            .accumulated_mask_at([0.0; 2], accumulation)
            .unwrap();
        assert!((accumulation.resolve(mask).unwrap() - 0.75).abs() < 1.0e-6);
    }

    #[test]
    fn command_validation_is_transactional_at_an_invalid_end() {
        let first = contact([0.0, 0.0], 1.0);
        let wrong = contact([2.0, 0.0], 1.0);
        let mut geometry = RoundStrokeGeometry::default();
        geometry
            .apply_command(RoundPathCommand::Begin(first))
            .unwrap();
        assert_eq!(
            geometry.apply_command(RoundPathCommand::End {
                at: wrong,
                elapsed_micros: 1,
            }),
            Err(StrokeError::DiscontinuousPath)
        );
        assert_eq!(geometry.subpath_count(), 1);
        geometry
            .apply_command(RoundPathCommand::End {
                at: first,
                elapsed_micros: 1,
            })
            .unwrap();
    }

    #[test]
    fn bounds_include_the_antialiasing_fringe() {
        let geometry = build_path(&[[-2.0, 4.0], [8.0, -6.0]]);
        assert_eq!(
            geometry.bounds(),
            Some(RoundStrokeBounds {
                min: [-3.5, -7.5],
                max: [9.5, 5.5],
            })
        );
    }
}
