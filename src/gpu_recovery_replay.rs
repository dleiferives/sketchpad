use crate::{
    document::{DocumentRevision, LayerId},
    gpu_layer_recovery::{
        replay_exact_layer_recovery_command, GpuExactLayerRecoveryCommand,
        GpuExactLayerRecoveryReplayError,
    },
    gpu_raster_recovery::{
        replay_exact_raster_recovery_command, GpuExactRasterRecoveryCommand,
        GpuExactRasterRecoveryReplayError,
    },
    gpu_recovery_timeline::GpuRecoveryTimelineSnapshot,
    gpu_round_recovery::{
        replay_round_recovery_command, GpuRoundRecoveryCommand, GpuRoundRecoveryReplayError,
    },
    raster::{Damage, RasterError, RasterLayer},
};
use std::{error::Error, fmt, mem::size_of};

#[derive(Clone, Debug, PartialEq)]
pub enum GpuRasterRecoveryCommand {
    Round(GpuRoundRecoveryCommand),
    Exact(GpuExactRasterRecoveryCommand),
    LayerSnapshot(GpuExactLayerRecoveryCommand),
    MetadataOnly,
}

impl GpuRasterRecoveryCommand {
    pub const fn layer(&self) -> Option<LayerId> {
        match self {
            Self::Round(command) => Some(command.layer()),
            Self::Exact(command) => Some(command.layer()),
            Self::LayerSnapshot(command) => Some(command.layer()),
            Self::MetadataOnly => None,
        }
    }

    pub fn retained_byte_len(&self) -> u64 {
        let (variant_size, variant_bytes) = match self {
            Self::Round(command) => (
                size_of::<GpuRoundRecoveryCommand>() as u64,
                command.retained_byte_len(),
            ),
            Self::Exact(command) => (
                size_of::<GpuExactRasterRecoveryCommand>() as u64,
                command.retained_byte_len(),
            ),
            Self::LayerSnapshot(command) => (
                size_of::<GpuExactLayerRecoveryCommand>() as u64,
                command.retained_byte_len(),
            ),
            Self::MetadataOnly => (size_of::<Self>() as u64, size_of::<Self>() as u64),
        };
        variant_bytes
            .checked_sub(variant_size)
            .and_then(|owned| owned.checked_add(size_of::<Self>() as u64))
            .expect("validated recovery payload accounting contains its inline value")
    }
}

impl From<GpuRoundRecoveryCommand> for GpuRasterRecoveryCommand {
    fn from(command: GpuRoundRecoveryCommand) -> Self {
        Self::Round(command)
    }
}

impl From<GpuExactRasterRecoveryCommand> for GpuRasterRecoveryCommand {
    fn from(command: GpuExactRasterRecoveryCommand) -> Self {
        Self::Exact(command)
    }
}

impl From<GpuExactLayerRecoveryCommand> for GpuRasterRecoveryCommand {
    fn from(command: GpuExactLayerRecoveryCommand) -> Self {
        Self::LayerSnapshot(command)
    }
}

pub fn replay_gpu_raster_recovery(
    snapshot: &GpuRecoveryTimelineSnapshot<GpuRasterRecoveryCommand>,
    layer: LayerId,
) -> Result<GpuRecoveredRaster, GpuRasterRecoveryReplayError> {
    let mut raster = snapshot.base().raster_layer(layer)?;
    let mut damage = Damage::default();
    let mut stats = GpuRasterRecoveryStats {
        commands_seen: snapshot.journal().records().len() as u64,
        ..GpuRasterRecoveryStats::default()
    };

    for record in snapshot.journal().records() {
        let command = record.command();
        if command.layer() != Some(layer) {
            stats.commands_skipped = stats.commands_skipped.saturating_add(1);
            continue;
        }
        stats.commands_applied = stats.commands_applied.saturating_add(1);
        match command {
            GpuRasterRecoveryCommand::Round(command) => {
                let replay = replay_round_recovery_command(&mut raster, command)?;
                stats.pixels_evaluated = stats
                    .pixels_evaluated
                    .saturating_add(replay.pixels_evaluated);
                stats.pixels_changed = stats.pixels_changed.saturating_add(replay.pixels_changed);
                if let Some(command_damage) = replay.damage {
                    merge_damage(&mut damage, &command_damage);
                }
            }
            GpuRasterRecoveryCommand::Exact(command) => {
                let replay = replay_exact_raster_recovery_command(layer, &mut raster, command)?;
                stats.pixels_changed = stats.pixels_changed.saturating_add(replay.pixels_changed);
                stats.tiles_written = stats.tiles_written.saturating_add(replay.tiles_written);
                stats.tiles_removed = stats.tiles_removed.saturating_add(replay.tiles_removed);
                if let Some(command_damage) = replay.damage {
                    merge_damage(&mut damage, &command_damage);
                }
            }
            GpuRasterRecoveryCommand::LayerSnapshot(command) => {
                let replay = replay_exact_layer_recovery_command(layer, &mut raster, command)?;
                stats.layer_snapshots_applied = stats.layer_snapshots_applied.saturating_add(1);
                stats.snapshot_tiles_installed = stats
                    .snapshot_tiles_installed
                    .saturating_add(replay.tiles_installed);
                stats.snapshot_tiles_discarded = stats
                    .snapshot_tiles_discarded
                    .saturating_add(replay.previous_tiles_discarded);
            }
            GpuRasterRecoveryCommand::MetadataOnly => {
                unreachable!("metadata-only recovery commands were skipped before dispatch")
            }
        }
    }
    raster.clear_history();
    stats.damage = (!damage.is_empty()).then_some(damage);
    Ok(GpuRecoveredRaster {
        revision: snapshot.target_revision(),
        raster,
        stats,
    })
}

fn merge_damage(destination: &mut Damage, source: &Damage) {
    for (tile, bounds) in source.tile_regions() {
        destination.add(tile, bounds);
    }
}

pub struct GpuRecoveredRaster {
    revision: DocumentRevision,
    raster: RasterLayer,
    stats: GpuRasterRecoveryStats,
}

impl GpuRecoveredRaster {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn raster(&self) -> &RasterLayer {
        &self.raster
    }

    pub fn into_raster(self) -> RasterLayer {
        self.raster
    }

    pub const fn stats(&self) -> &GpuRasterRecoveryStats {
        &self.stats
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuRasterRecoveryStats {
    pub damage: Option<Damage>,
    pub commands_seen: u64,
    pub commands_applied: u64,
    pub commands_skipped: u64,
    pub pixels_evaluated: u64,
    pub pixels_changed: u64,
    pub tiles_written: u32,
    pub tiles_removed: u32,
    pub layer_snapshots_applied: u32,
    pub snapshot_tiles_installed: u32,
    pub snapshot_tiles_discarded: u32,
}

#[derive(Debug)]
pub enum GpuRasterRecoveryReplayError {
    Raster(RasterError),
    Round(GpuRoundRecoveryReplayError),
    Exact(GpuExactRasterRecoveryReplayError),
    LayerSnapshot(GpuExactLayerRecoveryReplayError),
}

impl fmt::Display for GpuRasterRecoveryReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raster(error) => error.fmt(formatter),
            Self::Round(error) => error.fmt(formatter),
            Self::Exact(error) => error.fmt(formatter),
            Self::LayerSnapshot(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuRasterRecoveryReplayError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Raster(error) => Some(error),
            Self::Round(error) => Some(error),
            Self::Exact(error) => Some(error),
            Self::LayerSnapshot(error) => Some(error),
        }
    }
}

impl From<RasterError> for GpuRasterRecoveryReplayError {
    fn from(error: RasterError) -> Self {
        Self::Raster(error)
    }
}

impl From<GpuRoundRecoveryReplayError> for GpuRasterRecoveryReplayError {
    fn from(error: GpuRoundRecoveryReplayError) -> Self {
        Self::Round(error)
    }
}

impl From<GpuExactRasterRecoveryReplayError> for GpuRasterRecoveryReplayError {
    fn from(error: GpuExactRasterRecoveryReplayError) -> Self {
        Self::Exact(error)
    }
}

impl From<GpuExactLayerRecoveryReplayError> for GpuRasterRecoveryReplayError {
    fn from(error: GpuExactLayerRecoveryReplayError) -> Self {
        Self::LayerSnapshot(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gpu_atlas::LayerTileKey,
        gpu_document_mirror::{GpuCpuMirror, GpuMirrorPatchRegion},
        gpu_layer_recovery::GpuExactLayerRecoveryCommand,
        gpu_raster_recovery::GpuExactRasterRecoveryCommand,
        gpu_recovery_timeline::GpuRecoveryTimeline,
        raster::{LinearRgba, RasterLayer, RectU32, TileCoord},
        stroke::{RoundBrushRecipeV1, RoundContact, RoundPathCommand, StrokeMaterial},
    };

    fn dot(layer: LayerId, color: [f32; 3]) -> GpuRoundRecoveryCommand {
        let at = RoundContact {
            center: [8.5, 8.5],
            radius: 2.0,
            elapsed_micros: 0,
        };
        GpuRoundRecoveryCommand::new(
            layer,
            RoundBrushRecipeV1::with_minimum_pressure_fraction(
                StrokeMaterial::paint(color, 1.0, 1.0).unwrap(),
                4.0,
                1.0,
            )
            .unwrap(),
            vec![
                RoundPathCommand::Begin(at),
                RoundPathCommand::End {
                    at,
                    elapsed_micros: 1,
                },
            ],
        )
        .unwrap()
    }

    fn empty_base() -> crate::gpu_document_mirror::GpuCpuMirrorSnapshot {
        GpuCpuMirror::new(32, 32, 16, DocumentRevision::INITIAL)
            .unwrap()
            .snapshot()
    }

    fn record(
        timeline: &mut GpuRecoveryTimeline<GpuRasterRecoveryCommand>,
        revision: u64,
        command: impl Into<GpuRasterRecoveryCommand>,
    ) {
        let command = command.into();
        timeline
            .record(
                DocumentRevision::from_raw(revision),
                command.retained_byte_len(),
                command,
            )
            .unwrap();
    }

    #[test]
    fn simulated_device_loss_replays_round_then_exact_pixels_to_the_target_revision() {
        let layer = LayerId::from_raw(1);
        let mut timeline = GpuRecoveryTimeline::new(empty_base(), 8, 1_000_000).unwrap();
        record(&mut timeline, 1, dot(layer, [1.0, 0.0, 0.0]));
        let exact_blue = LinearRgba::premultiplied(0.0, 0.0, 0.75, 0.75);
        let exact = GpuExactRasterRecoveryCommand::from_regions(
            16,
            vec![GpuMirrorPatchRegion {
                key: LayerTileKey::new(layer, TileCoord::new(0, 0)),
                local_bounds: RectU32::from_xywh(0, 0, 16, 16).unwrap(),
                initialized: true,
                pixels: vec![exact_blue; 16 * 16].into_boxed_slice(),
            }],
        )
        .unwrap();
        record(&mut timeline, 2, exact);

        let recovered = replay_gpu_raster_recovery(&timeline.snapshot(), layer).unwrap();
        assert_eq!(recovered.revision(), DocumentRevision::from_raw(2));
        assert_eq!(recovered.raster().pixel(8, 8), Some(exact_blue));
        assert_eq!(recovered.raster().undo_depth(), 0);
        assert_eq!(recovered.stats().commands_seen, 2);
        assert_eq!(recovered.stats().commands_applied, 2);
        assert_eq!(recovered.stats().commands_skipped, 0);
        assert!(recovered.stats().pixels_changed > 0);
    }

    #[test]
    fn per_layer_replay_skips_other_layers_and_exact_absence_reclaims_storage() {
        let selected = LayerId::from_raw(1);
        let other = LayerId::from_raw(2);
        let mut timeline = GpuRecoveryTimeline::new(empty_base(), 8, 1_000_000).unwrap();
        record(&mut timeline, 1, dot(selected, [1.0, 0.0, 0.0]));
        record(&mut timeline, 2, dot(other, [0.0, 1.0, 0.0]));
        let absent = GpuExactRasterRecoveryCommand::from_regions(
            16,
            vec![GpuMirrorPatchRegion {
                key: LayerTileKey::new(selected, TileCoord::new(0, 0)),
                local_bounds: RectU32::from_xywh(0, 0, 16, 16).unwrap(),
                initialized: false,
                pixels: Box::new([]),
            }],
        )
        .unwrap();
        record(&mut timeline, 3, absent);

        let recovered = replay_gpu_raster_recovery(&timeline.snapshot(), selected).unwrap();
        assert_eq!(recovered.revision(), DocumentRevision::from_raw(3));
        assert_eq!(recovered.raster().allocated_tile_count(), 0);
        assert_eq!(recovered.stats().commands_seen, 3);
        assert_eq!(recovered.stats().commands_applied, 2);
        assert_eq!(recovered.stats().commands_skipped, 1);
        assert_eq!(recovered.stats().tiles_removed, 1);
    }

    #[test]
    fn exact_layer_snapshot_can_seed_later_semantic_strokes() {
        let layer = LayerId::from_raw(4);
        let imported = LinearRgba::premultiplied(0.1, 0.2, 0.3, 0.75);
        let mut source = RasterLayer::new(32, 32, 16).unwrap();
        let gesture = source.begin_gesture().unwrap();
        source.set_pixel(gesture, 20, 4, imported).unwrap();
        source.commit_gesture(gesture).unwrap();
        let snapshot = GpuExactLayerRecoveryCommand::from_raster(layer, &source).unwrap();
        let mut timeline = GpuRecoveryTimeline::new(empty_base(), 8, 1_000_000).unwrap();
        record(&mut timeline, 1, snapshot);
        record(&mut timeline, 2, dot(layer, [1.0, 0.0, 0.0]));

        let recovered = replay_gpu_raster_recovery(&timeline.snapshot(), layer).unwrap();

        assert_eq!(recovered.raster().pixel(20, 4), Some(imported));
        assert_ne!(
            recovered.raster().pixel(8, 8),
            Some(LinearRgba::TRANSPARENT)
        );
        assert_eq!(recovered.stats().layer_snapshots_applied, 1);
        assert_eq!(recovered.stats().snapshot_tiles_installed, 1);
        assert_eq!(recovered.stats().snapshot_tiles_discarded, 0);
        assert_eq!(recovered.stats().commands_applied, 2);
    }
}
