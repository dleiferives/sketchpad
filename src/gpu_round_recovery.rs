use crate::{
    document::LayerId,
    raster::{Damage, RasterError, RasterLayer},
    round_geometry::RoundStrokeGeometry,
    stroke::{RoundBrushRecipeV1, RoundPathCommand, StrokeAccumulation, StrokeError},
};
use std::{error::Error, fmt, mem::size_of};

const SHARED_ALLOCATION_COUNTER_BYTES: u64 = (2 * size_of::<usize>()) as u64;

#[derive(Clone, Debug, PartialEq)]
pub struct GpuRoundRecoveryCommand {
    layer: LayerId,
    recipe: RoundBrushRecipeV1,
    commands: Box<[RoundPathCommand]>,
    retained_byte_len: u64,
}

impl GpuRoundRecoveryCommand {
    pub fn new(
        layer: LayerId,
        recipe: RoundBrushRecipeV1,
        commands: Vec<RoundPathCommand>,
    ) -> Result<Self, Box<GpuRoundRecoveryBuildFailure>> {
        if commands.is_empty() {
            return Err(Box::new(GpuRoundRecoveryBuildFailure {
                error: GpuRoundRecoveryBuildError::EmptyPath,
                commands,
            }));
        }
        match recipe.material().accumulation() {
            StrokeAccumulation::None => {
                return Err(Box::new(GpuRoundRecoveryBuildFailure {
                    error: GpuRoundRecoveryBuildError::NoEffect,
                    commands,
                }));
            }
            StrokeAccumulation::CoverageUnion => {}
            StrokeAccumulation::OpticalDensity { flow } => {
                return Err(Box::new(GpuRoundRecoveryBuildFailure {
                    error: GpuRoundRecoveryBuildError::UnsupportedFlow(flow),
                    commands,
                }));
            }
        }

        let mut geometry = RoundStrokeGeometry::default();
        if let Err(error) = geometry.apply_commands(commands.iter().copied()) {
            return Err(Box::new(GpuRoundRecoveryBuildFailure {
                error: error.into(),
                commands,
            }));
        }
        if !geometry.is_complete() {
            return Err(Box::new(GpuRoundRecoveryBuildFailure {
                error: GpuRoundRecoveryBuildError::IncompletePath,
                commands,
            }));
        }
        if !timestamps_are_monotonic(&commands) {
            return Err(Box::new(GpuRoundRecoveryBuildFailure {
                error: GpuRoundRecoveryBuildError::TimestampMovedBackward,
                commands,
            }));
        }

        let command_bytes =
            match (commands.len() as u64).checked_mul(size_of::<RoundPathCommand>() as u64) {
                Some(byte_len) => byte_len,
                None => {
                    return Err(Box::new(GpuRoundRecoveryBuildFailure {
                        error: GpuRoundRecoveryBuildError::ByteCountOverflow,
                        commands,
                    }));
                }
            };
        let retained_byte_len = match (size_of::<Self>() as u64)
            .checked_add(SHARED_ALLOCATION_COUNTER_BYTES)
            .and_then(|base| base.checked_add(command_bytes))
        {
            Some(byte_len) => byte_len,
            None => {
                return Err(Box::new(GpuRoundRecoveryBuildFailure {
                    error: GpuRoundRecoveryBuildError::ByteCountOverflow,
                    commands,
                }));
            }
        };
        Ok(Self {
            layer,
            recipe,
            commands: commands.into_boxed_slice(),
            retained_byte_len,
        })
    }

    pub const fn layer(&self) -> LayerId {
        self.layer
    }

    pub const fn recipe(&self) -> RoundBrushRecipeV1 {
        self.recipe
    }

    pub fn commands(&self) -> &[RoundPathCommand] {
        &self.commands
    }

    pub const fn retained_byte_len(&self) -> u64 {
        self.retained_byte_len
    }

    fn geometry(&self) -> RoundStrokeGeometry {
        let mut geometry = RoundStrokeGeometry::default();
        geometry
            .apply_commands(self.commands.iter().copied())
            .expect("a recovery command retains its validated path");
        geometry
    }
}

pub fn replay_round_recovery_command(
    layer: &mut RasterLayer,
    command: &GpuRoundRecoveryCommand,
) -> Result<GpuRoundRecoveryReplay, GpuRoundRecoveryReplayError> {
    let geometry = command.geometry();
    let bounds = geometry
        .bounds()
        .expect("a complete recovery path has conservative bounds");
    let min_x = (bounds.min[0] as f64)
        .floor()
        .clamp(0.0, layer.width() as f64) as u32;
    let min_y = (bounds.min[1] as f64)
        .floor()
        .clamp(0.0, layer.height() as f64) as u32;
    let max_x = (bounds.max[0] as f64)
        .ceil()
        .clamp(0.0, layer.width() as f64) as u32;
    let max_y = (bounds.max[1] as f64)
        .ceil()
        .clamp(0.0, layer.height() as f64) as u32;
    let gesture = layer.begin_brush_gesture(command.recipe.diameter())?;
    let mut pixels_evaluated = 0_u64;
    let mut pixels_changed = 0_u64;
    let replay = (|| -> Result<(), GpuRoundRecoveryReplayError> {
        for y in min_y..max_y {
            for x in min_x..max_x {
                pixels_evaluated = pixels_evaluated.saturating_add(1);
                let mask = geometry.accumulated_mask_at(
                    [x as f32 + 0.5, y as f32 + 0.5],
                    StrokeAccumulation::CoverageUnion,
                )?;
                if mask == 0.0 {
                    continue;
                }
                let current = layer
                    .pixel(x, y)
                    .expect("recovery bounds are clipped to the raster");
                let updated = command.recipe.material().apply(current, mask);
                if layer.set_pixel(gesture, x, y, updated)? {
                    pixels_changed = pixels_changed.saturating_add(1);
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = replay {
        layer
            .cancel_gesture(gesture)
            .expect("recovery rollback owns the active raster gesture");
        return Err(error);
    }
    let damage = layer.commit_gesture(gesture)?;
    Ok(GpuRoundRecoveryReplay {
        damage,
        pixels_evaluated,
        pixels_changed,
    })
}

fn timestamps_are_monotonic(commands: &[RoundPathCommand]) -> bool {
    let mut previous = None::<u64>;
    for command in commands {
        let (first, last) = match *command {
            RoundPathCommand::Begin(contact) => (contact.elapsed_micros, contact.elapsed_micros),
            RoundPathCommand::Sweep { from, to } => (from.elapsed_micros, to.elapsed_micros),
            RoundPathCommand::End { at, elapsed_micros } => (at.elapsed_micros, elapsed_micros),
        };
        if last < first || previous.is_some_and(|previous| first < previous) {
            return false;
        }
        previous = Some(last);
    }
    true
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuRoundRecoveryReplay {
    pub damage: Option<Damage>,
    pub pixels_evaluated: u64,
    pub pixels_changed: u64,
}

pub struct GpuRoundRecoveryBuildFailure {
    pub error: GpuRoundRecoveryBuildError,
    pub commands: Vec<RoundPathCommand>,
}

impl fmt::Debug for GpuRoundRecoveryBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuRoundRecoveryBuildFailure")
            .field("error", &self.error)
            .field("command_count", &self.commands.len())
            .finish()
    }
}

impl fmt::Display for GpuRoundRecoveryBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuRoundRecoveryBuildFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GpuRoundRecoveryBuildError {
    EmptyPath,
    NoEffect,
    UnsupportedFlow(f32),
    IncompletePath,
    TimestampMovedBackward,
    ByteCountOverflow,
    Stroke(StrokeError),
}

impl fmt::Display for GpuRoundRecoveryBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => write!(formatter, "GPU round recovery path is empty"),
            Self::NoEffect => write!(formatter, "GPU round recovery stroke has no effect"),
            Self::UnsupportedFlow(flow) => write!(
                formatter,
                "GPU round recovery currently requires full flow, got {flow}"
            ),
            Self::IncompletePath => write!(formatter, "GPU round recovery path is incomplete"),
            Self::TimestampMovedBackward => {
                write!(formatter, "GPU round recovery timestamp moved backward")
            }
            Self::ByteCountOverflow => write!(formatter, "GPU round recovery bytes overflow"),
            Self::Stroke(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuRoundRecoveryBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Stroke(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StrokeError> for GpuRoundRecoveryBuildError {
    fn from(error: StrokeError) -> Self {
        Self::Stroke(error)
    }
}

#[derive(Debug)]
pub enum GpuRoundRecoveryReplayError {
    Stroke(StrokeError),
    Raster(RasterError),
}

impl fmt::Display for GpuRoundRecoveryReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stroke(error) => error.fmt(formatter),
            Self::Raster(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuRoundRecoveryReplayError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Stroke(error) => Some(error),
            Self::Raster(error) => Some(error),
        }
    }
}

impl From<StrokeError> for GpuRoundRecoveryReplayError {
    fn from(error: StrokeError) -> Self {
        Self::Stroke(error)
    }
}

impl From<RasterError> for GpuRoundRecoveryReplayError {
    fn from(error: RasterError) -> Self {
        Self::Raster(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::DocumentRevision,
        gpu_recovery_journal::GpuRecoveryJournal,
        raster::LinearRgba,
        stroke::{RoundContact, StrokeMaterial},
    };

    fn contact(center: [f32; 2], radius: f32, elapsed_micros: u64) -> RoundContact {
        RoundContact {
            center,
            radius,
            elapsed_micros,
        }
    }

    fn dot_commands(at: RoundContact) -> Vec<RoundPathCommand> {
        vec![
            RoundPathCommand::Begin(at),
            RoundPathCommand::End {
                at,
                elapsed_micros: at.elapsed_micros + 1,
            },
        ]
    }

    fn recipe(material: StrokeMaterial) -> RoundBrushRecipeV1 {
        RoundBrushRecipeV1::with_minimum_pressure_fraction(material, 4.0, 1.0).unwrap()
    }

    #[test]
    fn build_rejects_incomplete_density_and_backward_time_without_losing_commands() {
        let layer = LayerId::from_raw(1);
        let at = contact([10.5, 10.5], 2.0, 10);
        let incomplete = vec![RoundPathCommand::Begin(at)];
        let failure = GpuRoundRecoveryCommand::new(
            layer,
            recipe(StrokeMaterial::paint([0.0; 3], 1.0, 1.0).unwrap()),
            incomplete,
        )
        .unwrap_err();
        assert_eq!(failure.error, GpuRoundRecoveryBuildError::IncompletePath);
        assert_eq!(failure.commands, vec![RoundPathCommand::Begin(at)]);

        let density = GpuRoundRecoveryCommand::new(
            layer,
            recipe(StrokeMaterial::paint([0.0; 3], 1.0, 0.5).unwrap()),
            dot_commands(at),
        )
        .unwrap_err();
        assert_eq!(
            density.error,
            GpuRoundRecoveryBuildError::UnsupportedFlow(0.5)
        );

        let mut backward = dot_commands(at);
        backward[1] = RoundPathCommand::End {
            at,
            elapsed_micros: 9,
        };
        let failure = GpuRoundRecoveryCommand::new(
            layer,
            recipe(StrokeMaterial::paint([0.0; 3], 1.0, 1.0).unwrap()),
            backward,
        )
        .unwrap_err();
        assert_eq!(
            failure.error,
            GpuRoundRecoveryBuildError::TimestampMovedBackward
        );
    }

    #[test]
    fn cpu_replay_applies_full_flow_paint_and_erase_without_allocating_empty_erase() {
        let layer = LayerId::from_raw(1);
        let at = contact([10.5, 10.5], 2.0, 0);
        let paint = GpuRoundRecoveryCommand::new(
            layer,
            recipe(StrokeMaterial::paint([0.2, 0.4, 0.8], 0.5, 1.0).unwrap()),
            dot_commands(at),
        )
        .unwrap();
        let erase = GpuRoundRecoveryCommand::new(
            layer,
            recipe(StrokeMaterial::eraser(0.25, 1.0).unwrap()),
            dot_commands(at),
        )
        .unwrap();
        let mut raster = RasterLayer::new(32, 32, 16).unwrap();

        let paint_replay = replay_round_recovery_command(&mut raster, &paint).unwrap();
        assert!(paint_replay.damage.is_some());
        assert!(paint_replay.pixels_changed > 0);
        assert_eq!(
            raster.pixel(10, 10),
            Some(LinearRgba::premultiplied(0.1, 0.2, 0.4, 0.5))
        );

        let erase_replay = replay_round_recovery_command(&mut raster, &erase).unwrap();
        assert!(erase_replay.damage.is_some());
        assert_eq!(
            raster.pixel(10, 10),
            Some(LinearRgba::premultiplied(0.075, 0.15, 0.3, 0.375))
        );

        let mut empty = RasterLayer::new(32, 32, 16).unwrap();
        let empty_replay = replay_round_recovery_command(&mut empty, &erase).unwrap();
        assert!(empty_replay.damage.is_none());
        assert_eq!(empty.allocated_tile_count(), 0);
    }

    #[test]
    fn journal_snapshot_replays_round_commands_in_revision_order() {
        let layer = LayerId::from_raw(1);
        let first = GpuRoundRecoveryCommand::new(
            layer,
            recipe(StrokeMaterial::paint([1.0, 0.0, 0.0], 0.5, 1.0).unwrap()),
            dot_commands(contact([10.5, 10.5], 2.0, 0)),
        )
        .unwrap();
        let second = GpuRoundRecoveryCommand::new(
            layer,
            recipe(StrokeMaterial::paint([0.0, 0.0, 1.0], 0.5, 1.0).unwrap()),
            dot_commands(contact([10.5, 10.5], 2.0, 2)),
        )
        .unwrap();
        let mut journal = GpuRecoveryJournal::new(DocumentRevision::INITIAL, 4, 16_384).unwrap();
        journal
            .record(
                DocumentRevision::from_raw(1),
                first.retained_byte_len(),
                first,
            )
            .unwrap();
        journal
            .record(
                DocumentRevision::from_raw(2),
                second.retained_byte_len(),
                second,
            )
            .unwrap();
        let snapshot = journal.snapshot();
        let mut raster = RasterLayer::new(32, 32, 16).unwrap();
        for record in snapshot.records() {
            replay_round_recovery_command(&mut raster, record.command()).unwrap();
        }

        assert_eq!(snapshot.target_revision(), DocumentRevision::from_raw(2));
        assert_eq!(
            raster.pixel(10, 10),
            Some(LinearRgba::premultiplied(0.25, 0.0, 0.5, 0.75))
        );
    }
}
