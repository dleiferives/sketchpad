use crate::{
    checkpoint::{self, CheckpointError, CheckpointSummary, DocumentSnapshot},
    document::{Document, DocumentError, DocumentLayerParts, DocumentRevision, LayerId},
    document_metadata::{DocumentLayerMetadata, DocumentMetadata},
    gpu_recovery_replay::{
        replay_gpu_raster_recovery, GpuRasterRecoveryCommand, GpuRasterRecoveryReplayError,
    },
    gpu_recovery_timeline::GpuRecoveryTimelineSnapshot,
    gpu_revision_tasks::GpuRevisionedPayload,
};
use std::{error::Error, fmt, path::Path};

pub type GpuDocumentLayerMetadata = DocumentLayerMetadata;
pub type GpuDocumentMetadataSnapshot = DocumentMetadata;

#[derive(Clone)]
pub struct GpuDocumentRecoverySnapshot {
    metadata: GpuDocumentMetadataSnapshot,
    rasters: GpuRecoveryTimelineSnapshot<GpuRasterRecoveryCommand>,
}

impl GpuDocumentRecoverySnapshot {
    pub fn new(
        metadata: GpuDocumentMetadataSnapshot,
        rasters: GpuRecoveryTimelineSnapshot<GpuRasterRecoveryCommand>,
    ) -> Result<Self, Box<GpuDocumentRecoverySnapshotFailure>> {
        let error = if metadata.revision() != rasters.target_revision() {
            Some(GpuDocumentRecoverySnapshotError::RevisionMismatch {
                metadata: metadata.revision(),
                rasters: rasters.target_revision(),
            })
        } else if metadata.dimensions() != rasters.base().dimensions()
            || metadata.tile_size() != rasters.base().tile_size()
        {
            Some(GpuDocumentRecoverySnapshotError::GeometryMismatch {
                metadata_dimensions: metadata.dimensions(),
                raster_dimensions: rasters.base().dimensions(),
                metadata_tile_size: metadata.tile_size(),
                raster_tile_size: rasters.base().tile_size(),
            })
        } else {
            None
        };
        if let Some(error) = error {
            return Err(Box::new(GpuDocumentRecoverySnapshotFailure {
                error,
                metadata,
                rasters,
            }));
        }
        Ok(Self { metadata, rasters })
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.metadata.revision()
    }

    pub const fn metadata(&self) -> &GpuDocumentMetadataSnapshot {
        &self.metadata
    }

    pub const fn rasters(&self) -> &GpuRecoveryTimelineSnapshot<GpuRasterRecoveryCommand> {
        &self.rasters
    }

    pub fn recover_document(&self) -> Result<GpuRecoveredDocument, GpuDocumentRecoveryError> {
        let mut parts = Vec::with_capacity(self.metadata.layers().len());
        let mut stats = GpuDocumentRecoveryStats::default();
        for layer in self.metadata.layers() {
            let recovered =
                replay_gpu_raster_recovery(&self.rasters, layer.id()).map_err(|error| {
                    GpuDocumentRecoveryError::Raster {
                        layer: layer.id(),
                        error,
                    }
                })?;
            stats.layers_recovered = stats.layers_recovered.saturating_add(1);
            stats.command_visits = stats
                .command_visits
                .saturating_add(recovered.stats().commands_seen);
            stats.commands_applied = stats
                .commands_applied
                .saturating_add(recovered.stats().commands_applied);
            stats.pixels_evaluated = stats
                .pixels_evaluated
                .saturating_add(recovered.stats().pixels_evaluated);
            stats.pixels_changed = stats
                .pixels_changed
                .saturating_add(recovered.stats().pixels_changed);
            stats.layer_snapshots_applied = stats
                .layer_snapshots_applied
                .saturating_add(recovered.stats().layer_snapshots_applied);
            parts.push(DocumentLayerParts {
                id: layer.id(),
                name: layer.name().to_owned(),
                visible: layer.visible(),
                opacity: layer.opacity(),
                raster: recovered.into_raster(),
            });
        }
        let document = Document::from_layer_parts_at_revision(
            self.metadata.width(),
            self.metadata.height(),
            self.metadata.tile_size(),
            self.metadata.active_layer(),
            parts,
            self.metadata.revision(),
        )?;
        Ok(GpuRecoveredDocument { document, stats })
    }

    pub fn build_checkpoint(
        &self,
    ) -> Result<GpuDocumentCheckpoint, GpuDocumentCheckpointBuildError> {
        let recovered = self.recover_document()?;
        let revision = recovered.document.revision();
        let stats = recovered.stats;
        let snapshot = checkpoint::snapshot_document(&recovered.document)?;
        Ok(GpuDocumentCheckpoint {
            revision,
            snapshot,
            recovery_stats: stats,
        })
    }
}

impl GpuRevisionedPayload for GpuDocumentRecoverySnapshot {
    fn revision(&self) -> DocumentRevision {
        self.revision()
    }
}

pub struct GpuRecoveredDocument {
    document: Document,
    stats: GpuDocumentRecoveryStats,
}

pub struct GpuDocumentCheckpoint {
    revision: DocumentRevision,
    snapshot: DocumentSnapshot,
    recovery_stats: GpuDocumentRecoveryStats,
}

impl GpuDocumentCheckpoint {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn recovery_stats(&self) -> GpuDocumentRecoveryStats {
        self.recovery_stats
    }

    pub fn encode(&self) -> Result<(Vec<u8>, CheckpointSummary), CheckpointError> {
        checkpoint::encode_document_snapshot(&self.snapshot)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<CheckpointSummary, CheckpointError> {
        checkpoint::save_document_snapshot_atomic(path, &self.snapshot)
    }
}

impl GpuRecoveredDocument {
    pub const fn document(&self) -> &Document {
        &self.document
    }

    pub fn into_document(self) -> Document {
        self.document
    }

    pub const fn stats(&self) -> GpuDocumentRecoveryStats {
        self.stats
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuDocumentRecoveryStats {
    pub layers_recovered: u32,
    pub command_visits: u64,
    pub commands_applied: u64,
    pub pixels_evaluated: u64,
    pub pixels_changed: u64,
    pub layer_snapshots_applied: u32,
}

pub struct GpuDocumentRecoverySnapshotFailure {
    pub error: GpuDocumentRecoverySnapshotError,
    pub metadata: GpuDocumentMetadataSnapshot,
    pub rasters: GpuRecoveryTimelineSnapshot<GpuRasterRecoveryCommand>,
}

impl fmt::Debug for GpuDocumentRecoverySnapshotFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuDocumentRecoverySnapshotFailure")
            .field("error", &self.error)
            .field("metadata_revision", &self.metadata.revision())
            .field("raster_revision", &self.rasters.target_revision())
            .finish()
    }
}

impl fmt::Display for GpuDocumentRecoverySnapshotFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuDocumentRecoverySnapshotFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuDocumentRecoverySnapshotError {
    RevisionMismatch {
        metadata: DocumentRevision,
        rasters: DocumentRevision,
    },
    GeometryMismatch {
        metadata_dimensions: [u32; 2],
        raster_dimensions: [u32; 2],
        metadata_tile_size: u32,
        raster_tile_size: u32,
    },
}

impl fmt::Display for GpuDocumentRecoverySnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionMismatch { metadata, rasters } => write!(
                formatter,
                "GPU recovery metadata revision {} does not match raster revision {}",
                metadata.get(),
                rasters.get()
            ),
            Self::GeometryMismatch {
                metadata_dimensions,
                raster_dimensions,
                metadata_tile_size,
                raster_tile_size,
            } => write!(
                formatter,
                "GPU recovery metadata {}x{} / tile {} does not match rasters {}x{} / tile {}",
                metadata_dimensions[0],
                metadata_dimensions[1],
                metadata_tile_size,
                raster_dimensions[0],
                raster_dimensions[1],
                raster_tile_size
            ),
        }
    }
}

impl Error for GpuDocumentRecoverySnapshotError {}

#[derive(Debug)]
pub enum GpuDocumentRecoveryError {
    Raster {
        layer: LayerId,
        error: GpuRasterRecoveryReplayError,
    },
    Document(DocumentError),
}

impl fmt::Display for GpuDocumentRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raster { layer, error } => {
                write!(
                    formatter,
                    "could not recover GPU layer {}: {error}",
                    layer.get()
                )
            }
            Self::Document(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuDocumentRecoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Raster { error, .. } => Some(error),
            Self::Document(error) => Some(error),
        }
    }
}

impl From<DocumentError> for GpuDocumentRecoveryError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

#[derive(Debug)]
pub enum GpuDocumentCheckpointBuildError {
    Recovery(GpuDocumentRecoveryError),
    Checkpoint(CheckpointError),
}

impl fmt::Display for GpuDocumentCheckpointBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recovery(error) => error.fmt(formatter),
            Self::Checkpoint(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuDocumentCheckpointBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Recovery(error) => Some(error),
            Self::Checkpoint(error) => Some(error),
        }
    }
}

impl From<GpuDocumentRecoveryError> for GpuDocumentCheckpointBuildError {
    fn from(error: GpuDocumentRecoveryError) -> Self {
        Self::Recovery(error)
    }
}

impl From<CheckpointError> for GpuDocumentCheckpointBuildError {
    fn from(error: CheckpointError) -> Self {
        Self::Checkpoint(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gpu_document_mirror::GpuCpuMirror,
        gpu_layer_recovery::GpuExactLayerRecoveryCommand,
        gpu_recovery_journal::GpuRecoveryJournalError,
        gpu_recovery_timeline::GpuRecoveryTimeline,
        gpu_round_recovery::GpuRoundRecoveryCommand,
        raster::{LinearRgba, RasterLayer},
        stroke::{RoundBrushRecipeV1, RoundContact, RoundPathCommand, StrokeMaterial},
    };

    fn round_command(layer: LayerId) -> GpuRasterRecoveryCommand {
        let at = RoundContact {
            center: [8.5, 8.5],
            radius: 2.0,
            elapsed_micros: 0,
        };
        GpuRoundRecoveryCommand::new(
            layer,
            RoundBrushRecipeV1::with_minimum_pressure_fraction(
                StrokeMaterial::paint([1.0, 0.0, 0.0], 1.0, 1.0).unwrap(),
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
        .into()
    }

    fn record(
        timeline: &mut GpuRecoveryTimeline<GpuRasterRecoveryCommand>,
        revision: DocumentRevision,
        command: GpuRasterRecoveryCommand,
    ) -> Result<(), GpuRecoveryJournalError> {
        let byte_len = command.retained_byte_len();
        timeline
            .record(revision, byte_len, command)
            .map_err(|failure| failure.error)
    }

    #[test]
    fn metadata_and_rasters_reconstruct_one_editable_revision_after_device_loss() {
        let mut document = Document::new(32, 32, 16).unwrap();
        let painted_layer = document.active_layer_id();
        let gesture = document.active_layer_mut().begin_gesture().unwrap();
        document
            .active_layer_mut()
            .set_pixel(gesture, 8, 8, LinearRgba::premultiplied(1.0, 0.0, 0.0, 1.0))
            .unwrap();
        document.active_layer_mut().commit_gesture(gesture).unwrap();
        document.record_active_raster_edit().unwrap();

        let base = GpuCpuMirror::new(32, 32, 16, DocumentRevision::INITIAL)
            .unwrap()
            .snapshot();
        let mut timeline = GpuRecoveryTimeline::new(base, 16, 1_000_000).unwrap();
        record(
            &mut timeline,
            document.revision(),
            round_command(painted_layer),
        )
        .unwrap();

        let second = document.create_layer("Color").unwrap();
        record(
            &mut timeline,
            document.revision(),
            GpuRasterRecoveryCommand::MetadataOnly,
        )
        .unwrap();
        document.rename_layer(second, "Highlights").unwrap();
        record(
            &mut timeline,
            document.revision(),
            GpuRasterRecoveryCommand::MetadataOnly,
        )
        .unwrap();
        document.set_layer_opacity(second, 0.5).unwrap();
        record(
            &mut timeline,
            document.revision(),
            GpuRasterRecoveryCommand::MetadataOnly,
        )
        .unwrap();
        document.set_layer_visibility(painted_layer, false).unwrap();
        record(
            &mut timeline,
            document.revision(),
            GpuRasterRecoveryCommand::MetadataOnly,
        )
        .unwrap();
        document.move_layer(second, 0).unwrap();
        record(
            &mut timeline,
            document.revision(),
            GpuRasterRecoveryCommand::MetadataOnly,
        )
        .unwrap();

        let metadata = GpuDocumentMetadataSnapshot::from_document(&document);
        let snapshot = GpuDocumentRecoverySnapshot::new(metadata, timeline.snapshot()).unwrap();
        let recovered = snapshot.recover_document().unwrap();
        let recovered_document = recovered.document();

        assert_eq!(recovered_document.revision(), document.revision());
        assert_eq!(recovered_document.active_layer_id(), second);
        assert_eq!(recovered_document.layers().len(), 2);
        assert_eq!(recovered_document.layers()[0].id(), second);
        assert_eq!(recovered_document.layers()[0].name(), "Highlights");
        assert_eq!(recovered_document.layers()[0].opacity(), 0.5);
        assert!(!recovered_document.layers()[1].visible());
        assert_eq!(
            recovered_document
                .layer_raster(painted_layer)
                .unwrap()
                .pixel(8, 8),
            Some(LinearRgba::premultiplied(1.0, 0.0, 0.0, 1.0))
        );
        assert_eq!(recovered_document.undo_depth(), 0);
        assert_eq!(recovered.stats().layers_recovered, 2);
        assert_eq!(recovered.stats().commands_applied, 1);

        let checkpoint = snapshot.build_checkpoint().unwrap();
        assert_eq!(checkpoint.revision(), document.revision());
        assert_eq!(checkpoint.recovery_stats(), recovered.stats());
        let (encoded, summary) = checkpoint.encode().unwrap();
        assert_eq!(summary.layer_count, 2);
        let reopened = checkpoint::decode_document(&encoded).unwrap();
        assert_eq!(reopened.active_layer_id(), second);
        assert_eq!(reopened.layers()[0].name(), "Highlights");
        assert_eq!(
            reopened.layer_raster(painted_layer).unwrap().pixel(8, 8),
            Some(LinearRgba::premultiplied(1.0, 0.0, 0.0, 1.0))
        );
    }

    #[test]
    fn pairing_rejects_revision_mismatch_and_returns_both_snapshots() {
        let document = Document::new(32, 32, 16).unwrap();
        let metadata = GpuDocumentMetadataSnapshot::from_document(&document);
        let base = GpuCpuMirror::new(32, 32, 16, DocumentRevision::INITIAL)
            .unwrap()
            .snapshot();
        let mut timeline = GpuRecoveryTimeline::new(base, 4, 1_024).unwrap();
        record(
            &mut timeline,
            DocumentRevision::from_raw(1),
            GpuRasterRecoveryCommand::MetadataOnly,
        )
        .unwrap();
        let failure = match GpuDocumentRecoverySnapshot::new(metadata, timeline.snapshot()) {
            Ok(_) => panic!("mismatched document recovery snapshots unexpectedly paired"),
            Err(failure) => failure,
        };
        assert_eq!(failure.metadata.revision(), DocumentRevision::INITIAL);
        assert_eq!(failure.rasters.target_revision().get(), 1);
        assert!(matches!(
            failure.error,
            GpuDocumentRecoverySnapshotError::RevisionMismatch { .. }
        ));
    }

    #[test]
    fn imported_sparse_layer_pixels_survive_loss_and_later_live_mutation() {
        let original = LinearRgba::premultiplied(0.2, 0.4, 0.6, 0.8);
        let later = LinearRgba::premultiplied(0.8, 0.4, 0.2, 0.8);
        let mut document = Document::new(32, 32, 16).unwrap();
        let mut imported = RasterLayer::new(32, 32, 16).unwrap();
        let gesture = imported.begin_gesture().unwrap();
        imported.set_pixel(gesture, 20, 4, original).unwrap();
        imported.commit_gesture(gesture).unwrap();
        let (imported_id, _) = document.insert_raster_layer("Imported", imported).unwrap();

        let base = GpuCpuMirror::new(32, 32, 16, DocumentRevision::INITIAL)
            .unwrap()
            .snapshot();
        let mut timeline = GpuRecoveryTimeline::new(base, 4, 1_000_000).unwrap();
        let imported_snapshot = GpuExactLayerRecoveryCommand::from_raster(
            imported_id,
            document.layer_raster(imported_id).unwrap(),
        )
        .unwrap();
        record(&mut timeline, document.revision(), imported_snapshot.into()).unwrap();
        let snapshot = GpuDocumentRecoverySnapshot::new(
            GpuDocumentMetadataSnapshot::from_document(&document),
            timeline.snapshot(),
        )
        .unwrap();

        let gesture = document.active_layer_mut().begin_gesture().unwrap();
        document
            .active_layer_mut()
            .set_pixel(gesture, 20, 4, later)
            .unwrap();
        document.active_layer_mut().commit_gesture(gesture).unwrap();
        document.record_active_raster_edit().unwrap();

        let recovered = snapshot.recover_document().unwrap();
        assert_eq!(
            recovered.document().revision(),
            DocumentRevision::from_raw(1)
        );
        assert_eq!(
            recovered
                .document()
                .layer_raster(imported_id)
                .unwrap()
                .pixel(20, 4),
            Some(original)
        );
        assert_eq!(
            document.layer_raster(imported_id).unwrap().pixel(20, 4),
            Some(later)
        );
        assert_eq!(recovered.stats().layer_snapshots_applied, 1);
        assert_eq!(recovered.stats().commands_applied, 1);
        assert_eq!(recovered.stats().command_visits, 2);
    }

    #[test]
    fn exact_layer_snapshot_restores_a_deletion_after_the_base_advanced() {
        let color = LinearRgba::premultiplied(0.4, 0.2, 0.1, 0.5);
        let mut document = Document::new(32, 32, 16).unwrap();
        let mut imported = RasterLayer::new(32, 32, 16).unwrap();
        let gesture = imported.begin_gesture().unwrap();
        imported.set_pixel(gesture, 20, 4, color).unwrap();
        imported.commit_gesture(gesture).unwrap();
        let (restored_id, _) = document.insert_raster_layer("Reference", imported).unwrap();
        document.delete_layer(restored_id).unwrap();
        assert_eq!(document.revision(), DocumentRevision::from_raw(2));
        document.undo().unwrap().unwrap();
        assert_eq!(document.revision(), DocumentRevision::from_raw(3));

        let base = GpuCpuMirror::new(32, 32, 16, DocumentRevision::from_raw(2))
            .unwrap()
            .snapshot();
        let mut timeline = GpuRecoveryTimeline::new(base, 4, 1_000_000).unwrap();
        let restored = GpuExactLayerRecoveryCommand::from_raster(
            restored_id,
            document.layer_raster(restored_id).unwrap(),
        )
        .unwrap();
        record(&mut timeline, document.revision(), restored.into()).unwrap();
        let snapshot = GpuDocumentRecoverySnapshot::new(
            GpuDocumentMetadataSnapshot::from_document(&document),
            timeline.snapshot(),
        )
        .unwrap();

        let recovered = snapshot.recover_document().unwrap();
        assert_eq!(
            recovered.document().revision(),
            DocumentRevision::from_raw(3)
        );
        assert_eq!(recovered.document().layers().len(), 2);
        assert_eq!(
            recovered
                .document()
                .layer_raster(restored_id)
                .unwrap()
                .pixel(20, 4),
            Some(color)
        );
        assert_eq!(recovered.stats().layer_snapshots_applied, 1);
    }
}
