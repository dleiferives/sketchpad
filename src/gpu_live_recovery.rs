use crate::{
    document::DocumentRevision,
    gpu_document_history::{GpuHistoryDirection, GpuHistoryId},
    gpu_document_mirror::GpuCpuMirrorSnapshot,
    gpu_history_recovery::{
        GpuHistoryRecoveryAttachment, GpuHistoryRecoveryEntry, GpuHistoryRecoveryError,
        GpuHistoryRecoverySpills,
    },
    gpu_raster_recovery::GpuExactRasterRecoveryTransition,
    gpu_recovery_journal::GpuRecoveryJournalError,
    gpu_recovery_replay::GpuRasterRecoveryCommand,
    gpu_recovery_timeline::{
        GpuRecoveryBaseAdvance, GpuRecoveryTimeline, GpuRecoveryTimelineError,
    },
};
use std::{error::Error, fmt};

pub struct GpuLiveRecovery {
    timeline: GpuRecoveryTimeline<GpuRasterRecoveryCommand>,
    spills: GpuHistoryRecoverySpills,
}

impl GpuLiveRecovery {
    pub fn new(
        base: GpuCpuMirrorSnapshot,
        journal_entries: usize,
        journal_bytes: u64,
        spill_entries: usize,
        spill_bytes: u64,
    ) -> Result<Self, GpuLiveRecoveryError> {
        let timeline = GpuRecoveryTimeline::new(base, journal_entries, journal_bytes)?;
        let spills = GpuHistoryRecoverySpills::new(spill_entries, spill_bytes)?;
        Ok(Self { timeline, spills })
    }

    pub fn from_parts(
        timeline: GpuRecoveryTimeline<GpuRasterRecoveryCommand>,
        spills: GpuHistoryRecoverySpills,
    ) -> Self {
        Self { timeline, spills }
    }

    pub fn prepare_history_record(
        &self,
        id: GpuHistoryId,
        evicted_ids: &[GpuHistoryId],
        revision: DocumentRevision,
        command: GpuRasterRecoveryCommand,
    ) -> Result<PreparedGpuHistoryRecoveryRecord, Box<GpuHistoryRecoveryPrepareFailure>> {
        let source_revision = self.timeline.target_revision();
        let byte_len = command.retained_byte_len();
        if let Err(error) = self.timeline.check_record(revision, byte_len) {
            return Err(prepare_failure(error.into(), command));
        }
        if let Err(error) = self
            .spills
            .check_replace(evicted_ids, id, source_revision, revision)
        {
            return Err(prepare_failure(error.into(), command));
        }
        Ok(PreparedGpuHistoryRecoveryRecord {
            id,
            evicted_ids: evicted_ids.into(),
            source_revision,
            revision,
            byte_len,
            command,
        })
    }

    pub fn commit_history_record(
        &mut self,
        prepared: PreparedGpuHistoryRecoveryRecord,
    ) -> Result<GpuHistoryRecoveryRecordCommit, Box<GpuHistoryRecoveryCommitFailure>> {
        if let Err(error) = self.check_prepared_history_record(&prepared) {
            return Err(Box::new(GpuHistoryRecoveryCommitFailure {
                error,
                prepared,
            }));
        }
        let PreparedGpuHistoryRecoveryRecord {
            id,
            evicted_ids,
            source_revision,
            revision,
            byte_len,
            command,
        } = prepared;
        let replacement = self
            .spills
            .replace(&evicted_ids, id, source_revision, revision)
            .expect("the live recovery record was rechecked before spill replacement");
        match self.timeline.record(revision, byte_len, command) {
            Ok(()) => {}
            Err(_) => {
                unreachable!("the live recovery record was rechecked before journal insertion")
            }
        }
        Ok(GpuHistoryRecoveryRecordCommit {
            id,
            revision,
            freed_spill_bytes: replacement.freed_bytes,
            evicted_spills: replacement.evicted,
        })
    }

    pub fn check_prepared_history_record(
        &self,
        prepared: &PreparedGpuHistoryRecoveryRecord,
    ) -> Result<(), GpuLiveRecoveryError> {
        let actual_source = self.timeline.target_revision();
        if actual_source != prepared.source_revision {
            return Err(GpuLiveRecoveryError::PreparedSourceChanged {
                prepared: prepared.source_revision,
                actual: actual_source,
            });
        }
        self.timeline
            .check_record(prepared.revision, prepared.byte_len)?;
        self.spills.check_replace(
            &prepared.evicted_ids,
            prepared.id,
            prepared.source_revision,
            prepared.revision,
        )?;
        Ok(())
    }

    pub fn prepare_history_swap(
        &self,
        id: GpuHistoryId,
        direction: GpuHistoryDirection,
        revision: DocumentRevision,
    ) -> Result<PreparedGpuHistoryRecoverySwap, GpuLiveRecoveryError> {
        let command =
            GpuRasterRecoveryCommand::from(self.spills.exact_command(id, direction)?.clone());
        let byte_len = command.retained_byte_len();
        self.timeline.check_record(revision, byte_len)?;
        Ok(PreparedGpuHistoryRecoverySwap {
            id,
            direction,
            source_revision: self.timeline.target_revision(),
            revision,
            byte_len,
            command,
        })
    }

    pub fn commit_history_swap(
        &mut self,
        prepared: PreparedGpuHistoryRecoverySwap,
    ) -> Result<GpuHistoryRecoverySwapCommit, Box<GpuHistoryRecoverySwapCommitFailure>> {
        if let Err(error) = self.check_prepared_history_swap(&prepared) {
            return Err(Box::new(GpuHistoryRecoverySwapCommitFailure {
                error,
                prepared,
            }));
        }

        let PreparedGpuHistoryRecoverySwap {
            id,
            direction,
            revision,
            byte_len,
            command,
            ..
        } = prepared;
        match self.timeline.record(revision, byte_len, command) {
            Ok(()) => {}
            Err(_) => {
                unreachable!("the live recovery swap was rechecked before journal insertion")
            }
        }
        Ok(GpuHistoryRecoverySwapCommit {
            id,
            direction,
            revision,
        })
    }

    pub fn check_prepared_history_swap(
        &self,
        prepared: &PreparedGpuHistoryRecoverySwap,
    ) -> Result<(), GpuLiveRecoveryError> {
        let actual_source = self.timeline.target_revision();
        if actual_source != prepared.source_revision {
            return Err(GpuLiveRecoveryError::PreparedSourceChanged {
                prepared: prepared.source_revision,
                actual: actual_source,
            });
        }
        self.timeline
            .check_record(prepared.revision, prepared.byte_len)?;
        Ok(())
    }

    pub fn prepare_mirror_handoff(
        &self,
        purpose: GpuMirrorRecoveryPurpose,
        snapshot: GpuCpuMirrorSnapshot,
        transition: GpuExactRasterRecoveryTransition,
    ) -> Result<PreparedGpuMirrorRecoveryHandoff, Box<GpuMirrorRecoveryPrepareFailure>> {
        let (retired_records, retired_bytes, attachment) =
            match self.check_mirror_handoff(purpose, &snapshot, &transition) {
                Ok(preview) => preview,
                Err(error) => {
                    return Err(Box::new(GpuMirrorRecoveryPrepareFailure {
                        error,
                        snapshot,
                        transition,
                    }));
                }
            };
        Ok(PreparedGpuMirrorRecoveryHandoff {
            purpose,
            retired_records,
            retired_bytes,
            attachment,
            snapshot,
            transition,
        })
    }

    pub fn commit_mirror_handoff(
        &mut self,
        prepared: PreparedGpuMirrorRecoveryHandoff,
    ) -> Result<GpuMirrorRecoveryCommit, Box<GpuMirrorRecoveryCommitFailure>> {
        if let Err(error) =
            self.check_mirror_handoff(prepared.purpose, &prepared.snapshot, &prepared.transition)
        {
            return Err(Box::new(GpuMirrorRecoveryCommitFailure { error, prepared }));
        }
        let PreparedGpuMirrorRecoveryHandoff {
            purpose,
            snapshot,
            transition,
            ..
        } = prepared;
        let (attachment, unused_transition) = match purpose {
            GpuMirrorRecoveryPurpose::History(_) => (
                Some(
                    self.spills
                        .attach(transition)
                        .expect("the mirror handoff was rechecked before spill attachment"),
                ),
                None,
            ),
            GpuMirrorRecoveryPurpose::ReconcileOnly => (None, Some(transition)),
        };
        let advance = self
            .timeline
            .advance_base(snapshot)
            .expect("the mirror handoff was rechecked before recovery-base advancement");
        Ok(GpuMirrorRecoveryCommit {
            purpose,
            attachment,
            unused_transition,
            advance,
        })
    }

    fn check_mirror_handoff(
        &self,
        purpose: GpuMirrorRecoveryPurpose,
        snapshot: &GpuCpuMirrorSnapshot,
        transition: &GpuExactRasterRecoveryTransition,
    ) -> Result<(usize, u64, Option<GpuHistoryRecoveryAttachment>), GpuLiveRecoveryError> {
        if snapshot.revision() != transition.revision() {
            return Err(GpuLiveRecoveryError::MirrorRevisionMismatch {
                snapshot: snapshot.revision(),
                transition: transition.revision(),
            });
        }
        if transition.source_revision() != self.timeline.base_revision() {
            return Err(GpuLiveRecoveryError::MirrorSourceMismatch {
                base: self.timeline.base_revision(),
                transition: transition.source_revision(),
            });
        }
        let retirement = self.timeline.check_advance_base(snapshot)?;
        let attachment = match purpose {
            GpuMirrorRecoveryPurpose::History(expected_id) => {
                let actual_id = self.spills.id_for_revision(transition.revision());
                if actual_id != Some(expected_id) {
                    return Err(GpuLiveRecoveryError::HistorySpillMismatch {
                        revision: transition.revision(),
                        expected: expected_id,
                        actual: actual_id,
                    });
                }
                Some(self.spills.check_attach(transition)?)
            }
            GpuMirrorRecoveryPurpose::ReconcileOnly => {
                if let Some(id) = self.spills.id_for_revision(transition.revision()) {
                    return Err(GpuLiveRecoveryError::HistorySpillWouldBeDiscarded {
                        revision: transition.revision(),
                        id,
                    });
                }
                None
            }
        };
        Ok((
            retirement.record_count,
            retirement.retired_bytes,
            attachment,
        ))
    }

    pub const fn timeline(&self) -> &GpuRecoveryTimeline<GpuRasterRecoveryCommand> {
        &self.timeline
    }

    pub const fn spills(&self) -> &GpuHistoryRecoverySpills {
        &self.spills
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.timeline.target_revision()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedGpuHistoryRecoveryRecord {
    id: GpuHistoryId,
    evicted_ids: Box<[GpuHistoryId]>,
    source_revision: DocumentRevision,
    revision: DocumentRevision,
    byte_len: u64,
    command: GpuRasterRecoveryCommand,
}

impl PreparedGpuHistoryRecoveryRecord {
    pub const fn id(&self) -> GpuHistoryId {
        self.id
    }

    pub fn evicted_ids(&self) -> &[GpuHistoryId] {
        &self.evicted_ids
    }

    pub const fn source_revision(&self) -> DocumentRevision {
        self.source_revision
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub const fn command(&self) -> &GpuRasterRecoveryCommand {
        &self.command
    }

    pub fn into_command(self) -> GpuRasterRecoveryCommand {
        self.command
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedGpuHistoryRecoverySwap {
    id: GpuHistoryId,
    direction: GpuHistoryDirection,
    source_revision: DocumentRevision,
    revision: DocumentRevision,
    byte_len: u64,
    command: GpuRasterRecoveryCommand,
}

impl PreparedGpuHistoryRecoverySwap {
    pub const fn id(&self) -> GpuHistoryId {
        self.id
    }

    pub const fn direction(&self) -> GpuHistoryDirection {
        self.direction
    }

    pub const fn source_revision(&self) -> DocumentRevision {
        self.source_revision
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub const fn command(&self) -> &GpuRasterRecoveryCommand {
        &self.command
    }

    pub fn into_command(self) -> GpuRasterRecoveryCommand {
        self.command
    }
}

pub struct GpuHistoryRecoveryRecordCommit {
    pub id: GpuHistoryId,
    pub revision: DocumentRevision,
    pub freed_spill_bytes: u64,
    pub evicted_spills: Vec<GpuHistoryRecoveryEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMirrorRecoveryPurpose {
    History(GpuHistoryId),
    ReconcileOnly,
}

pub struct PreparedGpuMirrorRecoveryHandoff {
    purpose: GpuMirrorRecoveryPurpose,
    retired_records: usize,
    retired_bytes: u64,
    attachment: Option<GpuHistoryRecoveryAttachment>,
    snapshot: GpuCpuMirrorSnapshot,
    transition: GpuExactRasterRecoveryTransition,
}

impl PreparedGpuMirrorRecoveryHandoff {
    pub const fn purpose(&self) -> GpuMirrorRecoveryPurpose {
        self.purpose
    }

    pub const fn retired_records(&self) -> usize {
        self.retired_records
    }

    pub const fn retired_bytes(&self) -> u64 {
        self.retired_bytes
    }

    pub const fn attachment(&self) -> Option<GpuHistoryRecoveryAttachment> {
        self.attachment
    }

    pub const fn snapshot(&self) -> &GpuCpuMirrorSnapshot {
        &self.snapshot
    }

    pub const fn transition(&self) -> &GpuExactRasterRecoveryTransition {
        &self.transition
    }
}

pub struct GpuMirrorRecoveryCommit {
    pub purpose: GpuMirrorRecoveryPurpose,
    pub attachment: Option<GpuHistoryRecoveryAttachment>,
    pub unused_transition: Option<GpuExactRasterRecoveryTransition>,
    pub advance: GpuRecoveryBaseAdvance<GpuRasterRecoveryCommand>,
}

pub struct GpuMirrorRecoveryPrepareFailure {
    pub error: GpuLiveRecoveryError,
    pub snapshot: GpuCpuMirrorSnapshot,
    pub transition: GpuExactRasterRecoveryTransition,
}

impl fmt::Debug for GpuMirrorRecoveryPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuMirrorRecoveryPrepareFailure")
            .field("error", &self.error)
            .field("snapshot_revision", &self.snapshot.revision())
            .field("transition_revision", &self.transition.revision())
            .finish()
    }
}

impl fmt::Display for GpuMirrorRecoveryPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuMirrorRecoveryPrepareFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

pub struct GpuMirrorRecoveryCommitFailure {
    pub error: GpuLiveRecoveryError,
    pub prepared: PreparedGpuMirrorRecoveryHandoff,
}

impl fmt::Debug for GpuMirrorRecoveryCommitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuMirrorRecoveryCommitFailure")
            .field("error", &self.error)
            .field("purpose", &self.prepared.purpose)
            .field("revision", &self.prepared.snapshot.revision())
            .finish()
    }
}

impl fmt::Display for GpuMirrorRecoveryCommitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuMirrorRecoveryCommitFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuHistoryRecoverySwapCommit {
    pub id: GpuHistoryId,
    pub direction: GpuHistoryDirection,
    pub revision: DocumentRevision,
}

pub struct GpuHistoryRecoveryPrepareFailure {
    pub error: GpuLiveRecoveryError,
    pub command: GpuRasterRecoveryCommand,
}

impl fmt::Debug for GpuHistoryRecoveryPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuHistoryRecoveryPrepareFailure")
            .field("error", &self.error)
            .field("command_bytes", &self.command.retained_byte_len())
            .finish()
    }
}

impl fmt::Display for GpuHistoryRecoveryPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuHistoryRecoveryPrepareFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

fn prepare_failure(
    error: GpuLiveRecoveryError,
    command: GpuRasterRecoveryCommand,
) -> Box<GpuHistoryRecoveryPrepareFailure> {
    Box::new(GpuHistoryRecoveryPrepareFailure { error, command })
}

pub struct GpuHistoryRecoveryCommitFailure {
    pub error: GpuLiveRecoveryError,
    pub prepared: PreparedGpuHistoryRecoveryRecord,
}

impl fmt::Debug for GpuHistoryRecoveryCommitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuHistoryRecoveryCommitFailure")
            .field("error", &self.error)
            .field("history_id", &self.prepared.id)
            .field("revision", &self.prepared.revision)
            .finish()
    }
}

impl fmt::Display for GpuHistoryRecoveryCommitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuHistoryRecoveryCommitFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

pub struct GpuHistoryRecoverySwapCommitFailure {
    pub error: GpuLiveRecoveryError,
    pub prepared: PreparedGpuHistoryRecoverySwap,
}

impl fmt::Debug for GpuHistoryRecoverySwapCommitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuHistoryRecoverySwapCommitFailure")
            .field("error", &self.error)
            .field("history_id", &self.prepared.id)
            .field("direction", &self.prepared.direction)
            .field("revision", &self.prepared.revision)
            .finish()
    }
}

impl fmt::Display for GpuHistoryRecoverySwapCommitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuHistoryRecoverySwapCommitFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuLiveRecoveryError {
    Journal(GpuRecoveryJournalError),
    Spills(GpuHistoryRecoveryError),
    Timeline(GpuRecoveryTimelineError),
    PreparedSourceChanged {
        prepared: DocumentRevision,
        actual: DocumentRevision,
    },
    MirrorRevisionMismatch {
        snapshot: DocumentRevision,
        transition: DocumentRevision,
    },
    MirrorSourceMismatch {
        base: DocumentRevision,
        transition: DocumentRevision,
    },
    HistorySpillMismatch {
        revision: DocumentRevision,
        expected: GpuHistoryId,
        actual: Option<GpuHistoryId>,
    },
    HistorySpillWouldBeDiscarded {
        revision: DocumentRevision,
        id: GpuHistoryId,
    },
}

impl fmt::Display for GpuLiveRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Journal(error) => error.fmt(formatter),
            Self::Spills(error) => error.fmt(formatter),
            Self::Timeline(error) => error.fmt(formatter),
            Self::PreparedSourceChanged { prepared, actual } => write!(
                formatter,
                "prepared GPU recovery source revision {} changed to {}",
                prepared.get(),
                actual.get()
            ),
            Self::MirrorRevisionMismatch {
                snapshot,
                transition,
            } => write!(
                formatter,
                "GPU mirror snapshot revision {} does not match transition {}",
                snapshot.get(),
                transition.get()
            ),
            Self::MirrorSourceMismatch { base, transition } => write!(
                formatter,
                "GPU mirror transition starts at revision {}, not recovery base {}",
                transition.get(),
                base.get()
            ),
            Self::HistorySpillMismatch {
                revision,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU mirror revision {} expected history spill {}, got {:?}",
                revision.get(),
                expected.get(),
                actual.map(GpuHistoryId::get)
            ),
            Self::HistorySpillWouldBeDiscarded { revision, id } => write!(
                formatter,
                "GPU mirror revision {} still belongs to history spill {}",
                revision.get(),
                id.get()
            ),
        }
    }
}

impl Error for GpuLiveRecoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Journal(error) => Some(error),
            Self::Spills(error) => Some(error),
            Self::Timeline(error) => Some(error),
            Self::PreparedSourceChanged { .. }
            | Self::MirrorRevisionMismatch { .. }
            | Self::MirrorSourceMismatch { .. }
            | Self::HistorySpillMismatch { .. }
            | Self::HistorySpillWouldBeDiscarded { .. } => None,
        }
    }
}

impl From<GpuRecoveryJournalError> for GpuLiveRecoveryError {
    fn from(error: GpuRecoveryJournalError) -> Self {
        Self::Journal(error)
    }
}

impl From<GpuHistoryRecoveryError> for GpuLiveRecoveryError {
    fn from(error: GpuHistoryRecoveryError) -> Self {
        Self::Spills(error)
    }
}

impl From<GpuRecoveryTimelineError> for GpuLiveRecoveryError {
    fn from(error: GpuRecoveryTimelineError) -> Self {
        Self::Timeline(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::LayerId,
        gpu_atlas::{AtlasLayout, LayerTileKey, SparseAtlasPlanner},
        gpu_document_mirror::{
            GpuCpuMirror, GpuMirrorPatchBatch, GpuMirrorPatchRegion, GpuMirrorReadbackPlan,
            GpuMirrorReconciler,
        },
        gpu_document_undo::{GpuMementoResidentState, GpuUndoCapturePlan, GPU_UNDO_BLOCK_BYTES},
        gpu_raster_recovery::GpuExactRasterRecoveryTransition,
        gpu_round_target::ActiveRoundMaskTile,
        raster::{LinearRgba, RectU32, TileCoord},
    };

    fn revision(value: u64) -> DocumentRevision {
        DocumentRevision::from_raw(value)
    }

    fn empty_state(spill_entries: usize) -> GpuLiveRecovery {
        let base = GpuCpuMirror::new(32, 32, 32, DocumentRevision::INITIAL)
            .unwrap()
            .snapshot();
        GpuLiveRecovery::new(base, 8, 1_000_000, spill_entries, 1_000_000).unwrap()
    }

    fn first_mirror_handoff() -> (GpuExactRasterRecoveryTransition, GpuCpuMirrorSnapshot) {
        let layout = AtlasLayout::new(32, 32, 1).unwrap();
        let layer = LayerId::from_raw(4);
        let key = LayerTileKey::new(layer, TileCoord::new(0, 0));
        let mut atlas = SparseAtlasPlanner::new(layout);
        let slot = atlas.allocate(key).unwrap().slot;
        let capture = GpuUndoCapturePlan::from_active_tiles(
            layout,
            &[ActiveRoundMaskTile {
                key,
                slot,
                local_damage: RectU32::from_xywh(0, 0, 16, 16).unwrap(),
            }],
        )
        .unwrap();
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::INITIAL,
            revision(1),
            &capture,
            &[GpuMementoResidentState {
                key,
                slot,
                document_initialized: true,
                memento_initialized: false,
            }],
            GPU_UNDO_BLOCK_BYTES,
        )
        .unwrap();
        let batch = &plan.batches()[0];
        let mapped = GpuMirrorPatchBatch::from_test_parts(
            revision(1),
            batch.index(),
            vec![GpuMirrorPatchRegion {
                key,
                local_bounds: batch.regions()[0].local_bounds,
                initialized: true,
                pixels: vec![LinearRgba::premultiplied(0.5, 0.0, 0.0, 0.5); 16 * 16].into(),
            }],
            batch.byte_len(),
        );
        let before = GpuCpuMirror::new(32, 32, 32, DocumentRevision::INITIAL)
            .unwrap()
            .snapshot();
        let transition = GpuExactRasterRecoveryTransition::from_mirror_revision(
            &before,
            &plan,
            vec![mapped.clone()],
        )
        .unwrap();
        let mut reconciler =
            GpuMirrorReconciler::new(32, 32, 32, DocumentRevision::INITIAL).unwrap();
        reconciler.register_plan(&plan).unwrap();
        reconciler.complete_batch(mapped).unwrap();
        (transition, reconciler.snapshot())
    }

    fn ready_state() -> (GpuLiveRecovery, GpuHistoryId) {
        let (transition, snapshot) = first_mirror_handoff();
        let timeline = GpuRecoveryTimeline::new(snapshot, 8, 1_000_000).unwrap();
        let id = GpuHistoryId::from_raw(1);
        let mut spills = GpuHistoryRecoverySpills::new(8, 1_000_000).unwrap();
        spills
            .register(id, DocumentRevision::INITIAL, revision(1))
            .unwrap();
        spills.attach(transition).unwrap();
        (GpuLiveRecovery::from_parts(timeline, spills), id)
    }

    #[test]
    fn new_history_record_is_prepared_without_mutation_then_committed_once() {
        let mut state = empty_state(2);
        let id = GpuHistoryId::from_raw(1);
        let prepared = state
            .prepare_history_record(id, &[], revision(1), GpuRasterRecoveryCommand::MetadataOnly)
            .unwrap();
        assert_eq!(prepared.id(), id);
        assert_eq!(prepared.source_revision(), DocumentRevision::INITIAL);
        assert_eq!(prepared.revision(), revision(1));
        assert_eq!(state.revision(), DocumentRevision::INITIAL);
        assert!(state.spills().is_empty());
        assert!(state.timeline().journal().is_empty());

        let committed = state.commit_history_record(prepared).unwrap();
        assert_eq!(committed.id, id);
        assert_eq!(committed.revision, revision(1));
        assert!(committed.evicted_spills.is_empty());
        assert_eq!(state.revision(), revision(1));
        assert_eq!(state.timeline().journal().len(), 1);
        assert_eq!(state.spills().pending_len(), 1);
    }

    #[test]
    fn branch_replacement_is_preflighted_and_returns_evicted_spill_ownership() {
        let mut state = empty_state(1);
        let first_id = GpuHistoryId::from_raw(1);
        let first = state
            .prepare_history_record(
                first_id,
                &[],
                revision(1),
                GpuRasterRecoveryCommand::MetadataOnly,
            )
            .unwrap();
        state.commit_history_record(first).unwrap();

        let second_id = GpuHistoryId::from_raw(2);
        let failure = state
            .prepare_history_record(
                second_id,
                &[],
                revision(2),
                GpuRasterRecoveryCommand::MetadataOnly,
            )
            .unwrap_err();
        assert!(matches!(
            failure.error,
            GpuLiveRecoveryError::Spills(GpuHistoryRecoveryError::EntryLimitExhausted { .. })
        ));
        assert_eq!(failure.command, GpuRasterRecoveryCommand::MetadataOnly);
        assert_eq!(state.revision(), revision(1));

        let second = state
            .prepare_history_record(
                second_id,
                &[first_id],
                revision(2),
                GpuRasterRecoveryCommand::MetadataOnly,
            )
            .unwrap();
        let committed = state.commit_history_record(second).unwrap();
        assert_eq!(committed.evicted_spills.len(), 1);
        assert_eq!(committed.evicted_spills[0].id(), first_id);
        assert_eq!(state.revision(), revision(2));
        assert!(state.spills().entry(first_id).is_none());
        assert!(state.spills().entry(second_id).is_some());
        assert_eq!(state.timeline().journal().len(), 2);
    }

    #[test]
    fn stale_prepared_record_returns_its_complete_command() {
        let mut state = empty_state(2);
        let stale = state
            .prepare_history_record(
                GpuHistoryId::from_raw(1),
                &[],
                revision(1),
                GpuRasterRecoveryCommand::MetadataOnly,
            )
            .unwrap();
        let winner = state
            .prepare_history_record(
                GpuHistoryId::from_raw(2),
                &[],
                revision(1),
                GpuRasterRecoveryCommand::MetadataOnly,
            )
            .unwrap();
        state.commit_history_record(winner).unwrap();

        let failure = match state.commit_history_record(stale) {
            Ok(_) => panic!("stale recovery record unexpectedly committed"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.error,
            GpuLiveRecoveryError::PreparedSourceChanged {
                prepared: DocumentRevision::INITIAL,
                actual: revision(1),
            }
        );
        assert_eq!(
            failure.prepared.into_command(),
            GpuRasterRecoveryCommand::MetadataOnly
        );
        assert_eq!(state.timeline().journal().len(), 1);
        assert_eq!(state.spills().len(), 1);
    }

    #[test]
    fn undo_waits_for_an_exact_spill_then_records_the_before_side() {
        let mut pending = empty_state(2);
        let id = GpuHistoryId::from_raw(1);
        let first = pending
            .prepare_history_record(id, &[], revision(1), GpuRasterRecoveryCommand::MetadataOnly)
            .unwrap();
        pending.commit_history_record(first).unwrap();
        assert!(matches!(
            pending.prepare_history_swap(id, GpuHistoryDirection::Undo, revision(2)),
            Err(GpuLiveRecoveryError::Spills(
                GpuHistoryRecoveryError::TransitionPending { .. }
            ))
        ));
        assert_eq!(pending.revision(), revision(1));

        let (mut ready, ready_id) = ready_state();
        let prepared = ready
            .prepare_history_swap(ready_id, GpuHistoryDirection::Undo, revision(2))
            .unwrap();
        let stored = ready
            .spills()
            .entry(ready_id)
            .unwrap()
            .transition()
            .unwrap();
        assert!(matches!(
            prepared.command(),
            GpuRasterRecoveryCommand::Exact(command) if command == stored.before()
        ));
        assert_eq!(ready.revision(), revision(1));

        let committed = ready.commit_history_swap(prepared).unwrap();
        assert_eq!(committed.id, ready_id);
        assert_eq!(committed.direction, GpuHistoryDirection::Undo);
        assert_eq!(committed.revision, revision(2));
        assert_eq!(ready.revision(), revision(2));
        assert_eq!(ready.timeline().journal().len(), 1);
    }

    #[test]
    fn mapped_history_attaches_its_spill_and_retires_the_same_revision() {
        let mut state = empty_state(2);
        let id = GpuHistoryId::from_raw(1);
        let record = state
            .prepare_history_record(id, &[], revision(1), GpuRasterRecoveryCommand::MetadataOnly)
            .unwrap();
        let record_bytes = record.byte_len();
        state.commit_history_record(record).unwrap();
        let (transition, snapshot) = first_mirror_handoff();

        let prepared = state
            .prepare_mirror_handoff(GpuMirrorRecoveryPurpose::History(id), snapshot, transition)
            .unwrap();
        assert_eq!(prepared.purpose(), GpuMirrorRecoveryPurpose::History(id));
        assert_eq!(prepared.retired_records(), 1);
        assert_eq!(prepared.retired_bytes(), record_bytes);
        assert_eq!(prepared.attachment().unwrap().id, id);
        assert_eq!(state.timeline().base_revision(), DocumentRevision::INITIAL);
        assert_eq!(state.timeline().journal().len(), 1);
        assert_eq!(state.spills().pending_len(), 1);

        let committed = state.commit_mirror_handoff(prepared).unwrap();
        assert_eq!(committed.purpose, GpuMirrorRecoveryPurpose::History(id));
        assert_eq!(committed.attachment.unwrap().id, id);
        assert!(committed.unused_transition.is_none());
        assert_eq!(committed.advance.retirement.records.len(), 1);
        assert_eq!(state.timeline().base_revision(), revision(1));
        assert!(state.timeline().journal().is_empty());
        assert_eq!(state.spills().ready_len(), 1);
        assert_eq!(state.spills().pending_len(), 0);
    }

    #[test]
    fn reconcile_only_handoff_returns_the_unneeded_transition() {
        let base = GpuCpuMirror::new(32, 32, 32, DocumentRevision::INITIAL)
            .unwrap()
            .snapshot();
        let mut timeline = GpuRecoveryTimeline::new(base, 8, 1_000_000).unwrap();
        let command = GpuRasterRecoveryCommand::MetadataOnly;
        timeline
            .record(revision(1), command.retained_byte_len(), command)
            .unwrap();
        let spills = GpuHistoryRecoverySpills::new(8, 1_000_000).unwrap();
        let mut state = GpuLiveRecovery::from_parts(timeline, spills);
        let (transition, snapshot) = first_mirror_handoff();

        let prepared = state
            .prepare_mirror_handoff(
                GpuMirrorRecoveryPurpose::ReconcileOnly,
                snapshot,
                transition,
            )
            .unwrap();
        assert!(prepared.attachment().is_none());
        let committed = state.commit_mirror_handoff(prepared).unwrap();
        assert!(committed.attachment.is_none());
        assert_eq!(committed.unused_transition.unwrap().revision(), revision(1));
        assert_eq!(state.timeline().base_revision(), revision(1));
        assert!(state.timeline().journal().is_empty());
        assert!(state.spills().is_empty());
    }

    #[test]
    fn wrong_mirror_purpose_returns_snapshot_and_transition_without_retirement() {
        let mut state = empty_state(2);
        let id = GpuHistoryId::from_raw(1);
        let record = state
            .prepare_history_record(id, &[], revision(1), GpuRasterRecoveryCommand::MetadataOnly)
            .unwrap();
        state.commit_history_record(record).unwrap();
        let (transition, snapshot) = first_mirror_handoff();

        let failure = match state.prepare_mirror_handoff(
            GpuMirrorRecoveryPurpose::ReconcileOnly,
            snapshot,
            transition,
        ) {
            Ok(_) => panic!("history mirror was allowed to discard its spill"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.error,
            GpuLiveRecoveryError::HistorySpillWouldBeDiscarded {
                revision: revision(1),
                id,
            }
        );
        assert_eq!(failure.snapshot.revision(), revision(1));
        assert_eq!(failure.transition.revision(), revision(1));
        assert_eq!(state.timeline().base_revision(), DocumentRevision::INITIAL);
        assert_eq!(state.timeline().journal().len(), 1);
        assert_eq!(state.spills().pending_len(), 1);
    }
}
