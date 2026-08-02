use crate::{
    document::DocumentRevision,
    gpu_atlas::{AtlasLayout, SparseAtlasPlanner},
    gpu_document_history::{
        GpuDocumentHistory, GpuDocumentHistoryError, GpuHistoryDirection, GpuHistoryEntry,
        GpuHistoryId, GpuHistoryRecordError, GpuHistoryRecordPreview, DEFAULT_GPU_HISTORY_BYTES,
        DEFAULT_GPU_HISTORY_ENTRIES,
    },
    gpu_document_mirror::{
        encode_gpu_mirror_revision_capture, GpuMirrorPlanError, GpuMirrorReadbackError,
        GpuMirrorReadbackPlan, GpuMirrorRevisionCapture, DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
    },
    gpu_document_mirror_dispatcher::{
        GpuMirrorDispatchError, GpuMirrorDispatcher, DEFAULT_GPU_MIRROR_SNAPSHOT_BYTES,
    },
    gpu_document_target::{
        ColorCommitStats, EncodedGpuDocumentCommit, EncodedGpuUndoSwap, GpuDocumentTarget,
        GpuDocumentTargetError, GpuUndoSwapStats,
    },
    gpu_history_recovery::{
        GpuHistoryRecoveryEntry, DEFAULT_GPU_HISTORY_RECOVERY_BYTES,
        DEFAULT_GPU_HISTORY_RECOVERY_ENTRIES,
    },
    gpu_live_recovery::{
        GpuLiveRecovery, GpuLiveRecoveryError, GpuMirrorRecoveryPurpose,
        PreparedGpuHistoryRecoveryRecord, PreparedGpuHistoryRecoverySwap,
    },
    gpu_recovery_journal::{
        DEFAULT_GPU_RECOVERY_JOURNAL_BYTES, DEFAULT_GPU_RECOVERY_JOURNAL_ENTRIES,
    },
    gpu_recovery_replay::GpuRasterRecoveryCommand,
};
use std::{collections::VecDeque, error::Error, fmt, iter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuResidentDocumentLimits {
    pub history_entries: usize,
    pub history_bytes: u64,
    pub mirror_snapshot_bytes: u64,
    pub mirror_staging_bytes: u64,
    pub recovery_journal_entries: usize,
    pub recovery_journal_bytes: u64,
    pub recovery_spill_entries: usize,
    pub recovery_spill_bytes: u64,
}

impl Default for GpuResidentDocumentLimits {
    fn default() -> Self {
        Self {
            history_entries: DEFAULT_GPU_HISTORY_ENTRIES,
            history_bytes: DEFAULT_GPU_HISTORY_BYTES,
            mirror_snapshot_bytes: DEFAULT_GPU_MIRROR_SNAPSHOT_BYTES,
            mirror_staging_bytes: DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
            recovery_journal_entries: DEFAULT_GPU_RECOVERY_JOURNAL_ENTRIES,
            recovery_journal_bytes: DEFAULT_GPU_RECOVERY_JOURNAL_BYTES,
            recovery_spill_entries: DEFAULT_GPU_HISTORY_RECOVERY_ENTRIES,
            recovery_spill_bytes: DEFAULT_GPU_HISTORY_RECOVERY_BYTES,
        }
    }
}

pub struct GpuResidentDocument {
    atlas: SparseAtlasPlanner,
    history: GpuDocumentHistory,
    mirror: GpuMirrorDispatcher,
    recovery: GpuLiveRecovery,
    mirror_purposes: VecDeque<GpuMirrorRecoveryPurpose>,
}

impl GpuResidentDocument {
    pub fn new(
        width: u32,
        height: u32,
        layout: AtlasLayout,
        initial_revision: DocumentRevision,
        limits: GpuResidentDocumentLimits,
    ) -> Result<Self, GpuResidentDocumentError> {
        let atlas = SparseAtlasPlanner::new(layout);
        let history = GpuDocumentHistory::new(limits.history_entries, limits.history_bytes)?;
        let mirror = GpuMirrorDispatcher::new(
            width,
            height,
            layout.tile_size(),
            initial_revision,
            limits.mirror_snapshot_bytes,
            limits.mirror_staging_bytes,
        )?;
        let recovery = GpuLiveRecovery::new(
            mirror.snapshot(),
            limits.recovery_journal_entries,
            limits.recovery_journal_bytes,
            limits.recovery_spill_entries,
            limits.recovery_spill_bytes,
        )?;
        Ok(Self {
            atlas,
            history,
            mirror,
            recovery,
            mirror_purposes: VecDeque::new(),
        })
    }

    pub fn prepare_commit(
        &self,
        target: &GpuDocumentTarget,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        encoded: EncodedGpuDocumentCommit,
        recovery_command: GpuRasterRecoveryCommand,
    ) -> Result<PreparedGpuResidentDocumentCommit, Box<GpuResidentDocumentPrepareFailure>> {
        if target.layout() != self.atlas.layout() {
            return Err(prepare_failure(
                GpuResidentDocumentError::TargetLayoutMismatch {
                    expected: self.atlas.layout(),
                    actual: target.layout(),
                },
                encoded,
                recovery_command,
            ));
        }
        if let Err(error) = target.check_encoded_commit(&encoded) {
            return Err(prepare_failure(error.into(), encoded, recovery_command));
        }
        let source_revision = self.recovery.revision();
        let Some(revision) = source_revision.checked_next() else {
            return Err(prepare_failure(
                GpuResidentDocumentError::RevisionExhausted,
                encoded,
                recovery_command,
            ));
        };
        let history = match self.history.check_record(&self.atlas, encoded.memento()) {
            Ok(preview) => preview,
            Err(error) => {
                return Err(prepare_failure(error.into(), encoded, recovery_command));
            }
        };
        let plan = match GpuMirrorReadbackPlan::from_memento(
            source_revision,
            revision,
            encoded.memento(),
            self.mirror.max_staging_bytes(),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                return Err(prepare_failure(error.into(), encoded, recovery_command));
            }
        };
        if let Err(error) = self.mirror.check_plan(&plan) {
            return Err(prepare_failure(error.into(), encoded, recovery_command));
        }
        let recovery = match self.recovery.prepare_history_record(
            history.id(),
            history.evicted_ids(),
            revision,
            recovery_command,
        ) {
            Ok(recovery) => recovery,
            Err(failure) => {
                return Err(prepare_failure(
                    failure.error.into(),
                    encoded,
                    failure.command,
                ));
            }
        };
        let capture = match encode_gpu_mirror_revision_capture(target, device, encoder, &plan) {
            Ok(capture) => capture,
            Err(error) => {
                return Err(prepare_failure(
                    error.into(),
                    encoded,
                    recovery.into_command(),
                ));
            }
        };
        if let Err(error) = self.mirror.check_prepared_capture(&plan, &capture) {
            return Err(prepare_failure(
                error.into(),
                encoded,
                recovery.into_command(),
            ));
        }
        Ok(PreparedGpuResidentDocumentCommit {
            revision,
            encoded,
            history,
            recovery,
            plan,
            capture,
        })
    }

    pub fn submit_commit(
        &mut self,
        queue: &wgpu::Queue,
        target: &mut GpuDocumentTarget,
        encoder: wgpu::CommandEncoder,
        mut prepared: PreparedGpuResidentDocumentCommit,
    ) -> Result<GpuResidentDocumentCommit, Box<GpuResidentDocumentSubmitFailure>> {
        if let Err(error) = self.check_prepared_commit(target, &prepared) {
            return Err(Box::new(GpuResidentDocumentSubmitFailure {
                error,
                encoder,
                prepared,
            }));
        }

        queue.submit(iter::once(encoder.finish()));
        let stats = prepared.encoded.stats();
        let memento = target
            .commit_submitted_with_memento(prepared.encoded)
            .expect("the resident document commit token was checked before submission");
        prepared
            .capture
            .capture_submitted()
            .expect("a newly prepared mirror capture has not been acknowledged");
        let history = match self.history.record(&mut self.atlas, memento) {
            Ok(history) => history,
            Err(_) => {
                unreachable!("GPU history was checked immediately before submission")
            }
        };
        assert!(
            prepared.history.matches_record(&history),
            "submitted GPU history did not match its preflighted identity"
        );
        let recovery = self
            .recovery
            .commit_history_record(prepared.recovery)
            .expect("live recovery was checked immediately before submission");
        assert_eq!(history.id, recovery.id);
        assert!(history.evicted.iter().map(GpuHistoryEntry::id).eq(recovery
            .evicted_spills
            .iter()
            .map(GpuHistoryRecoveryEntry::id)));
        self.mirror
            .enqueue(&prepared.plan, prepared.capture)
            .expect("mirror enqueue was checked immediately before submission");
        self.mirror_purposes
            .push_back(GpuMirrorRecoveryPurpose::History(history.id));
        Ok(GpuResidentDocumentCommit {
            revision: prepared.revision,
            history_id: history.id,
            stats,
            evicted_history: history.evicted,
            evicted_spills: recovery.evicted_spills,
            freed_spill_bytes: recovery.freed_spill_bytes,
        })
    }

    pub fn prepare_history_swap(
        &mut self,
        target: &mut GpuDocumentTarget,
        device: &wgpu::Device,
        mut encoder: wgpu::CommandEncoder,
        direction: GpuHistoryDirection,
    ) -> Result<Option<PreparedGpuResidentHistorySwap>, Box<GpuResidentHistorySwapPrepareFailure>>
    {
        let Some(revision) = self.revision().checked_next() else {
            return Err(history_swap_prepare_failure(
                GpuResidentDocumentError::RevisionExhausted,
            ));
        };
        let history_id = match self.history.check_begin(direction) {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(None),
            Err(error) => return Err(history_swap_prepare_failure(error.into())),
        };
        let recovery = match self
            .recovery
            .prepare_history_swap(history_id, direction, revision)
        {
            Ok(recovery) => recovery,
            Err(error) => return Err(history_swap_prepare_failure(error.into())),
        };
        let began = match direction {
            GpuHistoryDirection::Undo => self.history.begin_undo(),
            GpuHistoryDirection::Redo => self.history.begin_redo(),
        }
        .expect("GPU history was checked immediately before beginning a swap");
        assert!(began, "a checked GPU history swap has an entry");
        debug_assert_eq!(self.history.pending_id(), Some(history_id));

        let encoded = {
            let memento = self
                .history
                .pending_memento_mut()
                .expect("a begun GPU history swap retains its memento");
            match target.encode_undo_swap(device, &mut encoder, memento) {
                Ok(encoded) => encoded,
                Err(error) => {
                    self.history
                        .cancel_pending()
                        .expect("failed GPU swap encoding leaves history pending");
                    return Err(history_swap_prepare_failure(error.into()));
                }
            }
        };
        let plan = {
            let memento = self
                .history
                .pending_memento_mut()
                .expect("an encoded GPU history swap retains its memento");
            GpuMirrorReadbackPlan::from_memento(
                recovery.source_revision(),
                revision,
                memento,
                self.mirror.max_staging_bytes(),
            )
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                self.discard_encoded_history_swap(target, encoded);
                return Err(history_swap_prepare_failure(error.into()));
            }
        };
        if let Err(error) = self.mirror.check_plan(&plan) {
            self.discard_encoded_history_swap(target, encoded);
            return Err(history_swap_prepare_failure(error.into()));
        }
        let capture = match encode_gpu_mirror_revision_capture(target, device, &mut encoder, &plan)
        {
            Ok(capture) => capture,
            Err(error) => {
                self.discard_encoded_history_swap(target, encoded);
                return Err(history_swap_prepare_failure(error.into()));
            }
        };
        if let Err(error) = self.mirror.check_prepared_capture(&plan, &capture) {
            self.discard_encoded_history_swap(target, encoded);
            return Err(history_swap_prepare_failure(error.into()));
        }
        Ok(Some(PreparedGpuResidentHistorySwap {
            revision,
            history_id,
            direction,
            encoder,
            encoded,
            recovery,
            plan,
            capture,
        }))
    }

    pub fn submit_history_swap(
        &mut self,
        queue: &wgpu::Queue,
        target: &mut GpuDocumentTarget,
        mut prepared: PreparedGpuResidentHistorySwap,
    ) -> Result<GpuResidentHistorySwapCommit, Box<GpuResidentHistorySwapSubmitFailure>> {
        if let Err(error) = self.check_prepared_history_swap(target, &prepared) {
            return Err(Box::new(GpuResidentHistorySwapSubmitFailure {
                error,
                prepared,
            }));
        }

        queue.submit(iter::once(prepared.encoder.finish()));
        let stats = prepared.encoded.stats();
        target
            .undo_swap_submitted(prepared.encoded)
            .expect("the resident history swap token was checked before submission");
        let history_id = self
            .history
            .finish_pending()
            .expect("the resident history swap was checked before submission");
        let recovery = self
            .recovery
            .commit_history_swap(prepared.recovery)
            .expect("live recovery was checked before history swap submission");
        prepared
            .capture
            .capture_submitted()
            .expect("a newly prepared mirror capture has not been acknowledged");
        self.mirror
            .enqueue(&prepared.plan, prepared.capture)
            .expect("mirror enqueue was checked before history swap submission");
        self.mirror_purposes
            .push_back(GpuMirrorRecoveryPurpose::ReconcileOnly);
        assert_eq!(history_id, prepared.history_id);
        assert_eq!(recovery.id, prepared.history_id);
        assert_eq!(recovery.direction, prepared.direction);
        Ok(GpuResidentHistorySwapCommit {
            revision: prepared.revision,
            history_id,
            direction: prepared.direction,
            stats,
        })
    }

    pub fn discard_history_swap(
        &mut self,
        target: &mut GpuDocumentTarget,
        prepared: PreparedGpuResidentHistorySwap,
    ) -> Result<GpuHistoryId, GpuResidentDocumentError> {
        let PreparedGpuResidentHistorySwap { encoded, .. } = prepared;
        let memento = self.history.pending_memento_mut()?;
        target.undo_swap_discarded(encoded, memento)?;
        Ok(self.history.cancel_pending()?)
    }

    fn discard_encoded_history_swap(
        &mut self,
        target: &mut GpuDocumentTarget,
        encoded: EncodedGpuUndoSwap,
    ) {
        let memento = self
            .history
            .pending_memento_mut()
            .expect("an encoded GPU swap retains its pending history memento");
        target
            .undo_swap_discarded(encoded, memento)
            .expect("a newly encoded GPU swap can be discarded");
        self.history
            .cancel_pending()
            .expect("discarding a newly encoded GPU swap restores pending history");
    }

    fn check_prepared_history_swap(
        &self,
        target: &GpuDocumentTarget,
        prepared: &PreparedGpuResidentHistorySwap,
    ) -> Result<(), GpuResidentDocumentError> {
        target.check_encoded_undo_swap(&prepared.encoded)?;
        if self.history.pending_id() != Some(prepared.history_id)
            || self.history.pending_direction() != Some(prepared.direction)
        {
            return Err(GpuResidentDocumentError::HistorySwapChanged);
        }
        self.recovery
            .check_prepared_history_swap(&prepared.recovery)?;
        self.mirror
            .check_prepared_capture(&prepared.plan, &prepared.capture)?;
        Ok(())
    }

    fn check_prepared_commit(
        &self,
        target: &GpuDocumentTarget,
        prepared: &PreparedGpuResidentDocumentCommit,
    ) -> Result<(), GpuResidentDocumentError> {
        target.check_encoded_commit(&prepared.encoded)?;
        let history = self
            .history
            .check_record(&self.atlas, prepared.encoded.memento())?;
        if history != prepared.history {
            return Err(GpuResidentDocumentError::HistoryPreviewChanged);
        }
        self.recovery
            .check_prepared_history_record(&prepared.recovery)?;
        self.mirror
            .check_prepared_capture(&prepared.plan, &prepared.capture)?;
        Ok(())
    }

    pub const fn atlas(&self) -> &SparseAtlasPlanner {
        &self.atlas
    }

    pub const fn history(&self) -> &GpuDocumentHistory {
        &self.history
    }

    pub const fn mirror(&self) -> &GpuMirrorDispatcher {
        &self.mirror
    }

    pub const fn recovery(&self) -> &GpuLiveRecovery {
        &self.recovery
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.recovery.revision()
    }

    pub fn pending_mirror_purpose_count(&self) -> usize {
        self.mirror_purposes.len()
    }
}

pub struct PreparedGpuResidentDocumentCommit {
    revision: DocumentRevision,
    encoded: EncodedGpuDocumentCommit,
    history: GpuHistoryRecordPreview,
    recovery: PreparedGpuHistoryRecoveryRecord,
    plan: GpuMirrorReadbackPlan,
    capture: GpuMirrorRevisionCapture,
}

impl PreparedGpuResidentDocumentCommit {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn stats(&self) -> ColorCommitStats {
        self.encoded.stats()
    }

    pub const fn history(&self) -> &GpuHistoryRecordPreview {
        &self.history
    }

    pub const fn mirror_plan(&self) -> &GpuMirrorReadbackPlan {
        &self.plan
    }

    pub fn into_discard_parts(self) -> (EncodedGpuDocumentCommit, GpuRasterRecoveryCommand) {
        (self.encoded, self.recovery.into_command())
    }
}

pub struct GpuResidentDocumentCommit {
    pub revision: DocumentRevision,
    pub history_id: GpuHistoryId,
    pub stats: ColorCommitStats,
    pub evicted_history: Vec<GpuHistoryEntry>,
    pub evicted_spills: Vec<GpuHistoryRecoveryEntry>,
    pub freed_spill_bytes: u64,
}

pub struct PreparedGpuResidentHistorySwap {
    revision: DocumentRevision,
    history_id: GpuHistoryId,
    direction: GpuHistoryDirection,
    encoder: wgpu::CommandEncoder,
    encoded: EncodedGpuUndoSwap,
    recovery: PreparedGpuHistoryRecoverySwap,
    plan: GpuMirrorReadbackPlan,
    capture: GpuMirrorRevisionCapture,
}

impl PreparedGpuResidentHistorySwap {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn history_id(&self) -> GpuHistoryId {
        self.history_id
    }

    pub const fn direction(&self) -> GpuHistoryDirection {
        self.direction
    }

    pub const fn stats(&self) -> GpuUndoSwapStats {
        self.encoded.stats()
    }
}

pub struct GpuResidentHistorySwapCommit {
    pub revision: DocumentRevision,
    pub history_id: GpuHistoryId,
    pub direction: GpuHistoryDirection,
    pub stats: GpuUndoSwapStats,
}

pub struct GpuResidentHistorySwapPrepareFailure {
    pub error: GpuResidentDocumentError,
}

impl fmt::Debug for GpuResidentHistorySwapPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuResidentHistorySwapPrepareFailure")
            .field("error", &self.error)
            .finish()
    }
}

impl fmt::Display for GpuResidentHistorySwapPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuResidentHistorySwapPrepareFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

fn history_swap_prepare_failure(
    error: GpuResidentDocumentError,
) -> Box<GpuResidentHistorySwapPrepareFailure> {
    Box::new(GpuResidentHistorySwapPrepareFailure { error })
}

pub struct GpuResidentHistorySwapSubmitFailure {
    pub error: GpuResidentDocumentError,
    pub prepared: PreparedGpuResidentHistorySwap,
}

impl fmt::Debug for GpuResidentHistorySwapSubmitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuResidentHistorySwapSubmitFailure")
            .field("error", &self.error)
            .field("revision", &self.prepared.revision)
            .field("history_id", &self.prepared.history_id)
            .field("direction", &self.prepared.direction)
            .finish()
    }
}

impl fmt::Display for GpuResidentHistorySwapSubmitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuResidentHistorySwapSubmitFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

pub struct GpuResidentDocumentPrepareFailure {
    pub error: GpuResidentDocumentError,
    pub encoded: EncodedGpuDocumentCommit,
    pub recovery_command: GpuRasterRecoveryCommand,
}

impl fmt::Debug for GpuResidentDocumentPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuResidentDocumentPrepareFailure")
            .field("error", &self.error)
            .field("recovery_bytes", &self.recovery_command.retained_byte_len())
            .finish()
    }
}

impl fmt::Display for GpuResidentDocumentPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuResidentDocumentPrepareFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

fn prepare_failure(
    error: GpuResidentDocumentError,
    encoded: EncodedGpuDocumentCommit,
    recovery_command: GpuRasterRecoveryCommand,
) -> Box<GpuResidentDocumentPrepareFailure> {
    Box::new(GpuResidentDocumentPrepareFailure {
        error,
        encoded,
        recovery_command,
    })
}

pub struct GpuResidentDocumentSubmitFailure {
    pub error: GpuResidentDocumentError,
    pub encoder: wgpu::CommandEncoder,
    pub prepared: PreparedGpuResidentDocumentCommit,
}

impl fmt::Debug for GpuResidentDocumentSubmitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuResidentDocumentSubmitFailure")
            .field("error", &self.error)
            .field("revision", &self.prepared.revision)
            .finish()
    }
}

impl fmt::Display for GpuResidentDocumentSubmitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuResidentDocumentSubmitFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug)]
pub enum GpuResidentDocumentError {
    RevisionExhausted,
    TargetLayoutMismatch {
        expected: AtlasLayout,
        actual: AtlasLayout,
    },
    HistoryPreviewChanged,
    HistorySwapChanged,
    History(GpuDocumentHistoryError),
    HistoryRecord(GpuHistoryRecordError),
    Target(GpuDocumentTargetError),
    MirrorPlan(GpuMirrorPlanError),
    MirrorReadback(GpuMirrorReadbackError),
    MirrorDispatch(GpuMirrorDispatchError),
    Recovery(GpuLiveRecoveryError),
}

impl fmt::Display for GpuResidentDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionExhausted => {
                write!(formatter, "GPU document revision space is exhausted")
            }
            Self::TargetLayoutMismatch { expected, actual } => write!(
                formatter,
                "GPU document target layout {actual:?} does not match owner {expected:?}"
            ),
            Self::HistoryPreviewChanged => {
                write!(
                    formatter,
                    "GPU history changed after document commit preparation"
                )
            }
            Self::HistorySwapChanged => {
                write!(formatter, "GPU history changed after swap preparation")
            }
            Self::History(error) => error.fmt(formatter),
            Self::HistoryRecord(error) => error.fmt(formatter),
            Self::Target(error) => error.fmt(formatter),
            Self::MirrorPlan(error) => error.fmt(formatter),
            Self::MirrorReadback(error) => error.fmt(formatter),
            Self::MirrorDispatch(error) => error.fmt(formatter),
            Self::Recovery(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuResidentDocumentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::History(error) => Some(error),
            Self::HistoryRecord(error) => Some(error),
            Self::Target(error) => Some(error),
            Self::MirrorPlan(error) => Some(error),
            Self::MirrorReadback(error) => Some(error),
            Self::MirrorDispatch(error) => Some(error),
            Self::Recovery(error) => Some(error),
            _ => None,
        }
    }
}

impl From<GpuDocumentHistoryError> for GpuResidentDocumentError {
    fn from(error: GpuDocumentHistoryError) -> Self {
        Self::History(error)
    }
}

impl From<GpuHistoryRecordError> for GpuResidentDocumentError {
    fn from(error: GpuHistoryRecordError) -> Self {
        Self::HistoryRecord(error)
    }
}

impl From<GpuDocumentTargetError> for GpuResidentDocumentError {
    fn from(error: GpuDocumentTargetError) -> Self {
        Self::Target(error)
    }
}

impl From<GpuMirrorPlanError> for GpuResidentDocumentError {
    fn from(error: GpuMirrorPlanError) -> Self {
        Self::MirrorPlan(error)
    }
}

impl From<GpuMirrorReadbackError> for GpuResidentDocumentError {
    fn from(error: GpuMirrorReadbackError) -> Self {
        Self::MirrorReadback(error)
    }
}

impl From<GpuMirrorDispatchError> for GpuResidentDocumentError {
    fn from(error: GpuMirrorDispatchError) -> Self {
        Self::MirrorDispatch(error)
    }
}

impl From<GpuLiveRecoveryError> for GpuResidentDocumentError {
    fn from(error: GpuLiveRecoveryError) -> Self {
        Self::Recovery(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_document_undo::GPU_UNDO_BLOCK_BYTES;

    fn limits() -> GpuResidentDocumentLimits {
        GpuResidentDocumentLimits {
            history_entries: 7,
            history_bytes: 8 * GPU_UNDO_BLOCK_BYTES,
            mirror_snapshot_bytes: 9 * GPU_UNDO_BLOCK_BYTES,
            mirror_staging_bytes: 2 * GPU_UNDO_BLOCK_BYTES,
            recovery_journal_entries: 11,
            recovery_journal_bytes: 12 * GPU_UNDO_BLOCK_BYTES,
            recovery_spill_entries: 7,
            recovery_spill_bytes: 13 * GPU_UNDO_BLOCK_BYTES,
        }
    }

    #[test]
    fn owner_starts_all_subsystems_at_one_revision_and_budget() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let initial_revision = DocumentRevision::from_raw(19);
        let limits = limits();
        let document = GpuResidentDocument::new(48, 40, layout, initial_revision, limits).unwrap();

        assert_eq!(document.atlas().layout(), layout);
        assert_eq!(document.history().max_entries(), limits.history_entries);
        assert_eq!(document.history().max_bytes(), limits.history_bytes);
        assert_eq!(
            document.mirror().max_snapshot_bytes(),
            limits.mirror_snapshot_bytes
        );
        assert_eq!(
            document.mirror().max_staging_bytes(),
            limits.mirror_staging_bytes
        );
        assert_eq!(document.revision(), initial_revision);
        assert_eq!(document.mirror().snapshot().revision(), initial_revision);
        assert_eq!(
            document.recovery().timeline().base().revision(),
            initial_revision
        );
        assert_eq!(document.history().undo_depth(), 0);
        assert_eq!(document.mirror().pending_revision_count(), 0);
        assert_eq!(document.pending_mirror_purpose_count(), 0);
    }

    #[test]
    fn invalid_history_limit_fails_before_constructing_an_owner() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let result = GpuResidentDocument::new(
            48,
            40,
            layout,
            DocumentRevision::INITIAL,
            GpuResidentDocumentLimits {
                history_entries: 0,
                ..limits()
            },
        );

        assert!(matches!(
            result,
            Err(GpuResidentDocumentError::History(
                GpuDocumentHistoryError::InvalidMaximumEntries
            ))
        ));
    }

    #[test]
    fn invalid_mirror_budget_is_reported_at_the_owner_boundary() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let result = GpuResidentDocument::new(
            48,
            40,
            layout,
            DocumentRevision::INITIAL,
            GpuResidentDocumentLimits {
                mirror_staging_bytes: GPU_UNDO_BLOCK_BYTES - 1,
                ..limits()
            },
        );

        assert!(matches!(
            result,
            Err(GpuResidentDocumentError::MirrorDispatch(
                GpuMirrorDispatchError::InvalidStagingBudget { .. }
            ))
        ));
    }
}
