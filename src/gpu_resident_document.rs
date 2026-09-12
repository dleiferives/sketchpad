use crate::history_storage::{ArchiveError, ArchiveUsage, HistoryStore};
use crate::{
    document::{Document, DocumentRevision, LayerId},
    document_metadata::{
        DocumentMetadata, DocumentMetadataEdit, DocumentMetadataEditDirection,
        DocumentMetadataError,
    },
    gpu_atlas::{AtlasAllocation, AtlasError, AtlasLayout, LayerTileKey, SparseAtlasPlanner},
    gpu_document_history::{
        GpuDocumentHistory, GpuDocumentHistoryError, GpuHistoryDirection, GpuHistoryEntry,
        GpuHistoryEntryKind, GpuHistoryId, GpuHistoryRecordError, GpuHistoryRecordPreview,
        DEFAULT_GPU_HISTORY_BYTES, DEFAULT_GPU_HISTORY_ENTRIES,
    },
    gpu_document_mirror::{
        encode_gpu_mirror_revision_capture, GpuCpuMirror, GpuCpuMirrorSnapshot, GpuMirrorPlanError,
        GpuMirrorReadbackError, GpuMirrorReadbackPlan, GpuMirrorRevisionCapture,
        DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
    },
    gpu_document_mirror_dispatcher::{
        GpuMirrorDispatchCompletion, GpuMirrorDispatchError, GpuMirrorDispatcher,
        DEFAULT_GPU_MIRROR_SNAPSHOT_BYTES,
    },
    gpu_document_recovery::GpuDocumentRecoverySnapshot,
    gpu_document_target::{
        ColorCommitStats, EncodedGpuDocumentCommit, EncodedGpuUndoSwap, GpuDocumentBootstrapStats,
        GpuDocumentResidentClone, GpuDocumentResidentCloneStats, GpuDocumentResidentUpload,
        GpuDocumentResidentUploadStats, GpuDocumentTarget, GpuDocumentTargetError,
        GpuDocumentTargetId, GpuUndoSwapStats,
    },
    gpu_history_recovery::{
        GpuHistoryRecoveryEntry, DEFAULT_GPU_HISTORY_RECOVERY_BYTES,
        DEFAULT_GPU_HISTORY_RECOVERY_ENTRIES,
    },
    gpu_layer_recovery::{GpuExactLayerRecoveryBuildError, GpuExactLayerRecoveryCommand},
    gpu_live_recovery::{
        GpuLiveRecovery, GpuLiveRecoveryError, GpuMirrorRecoveryCommit, GpuMirrorRecoveryPurpose,
        PreparedGpuHistoryRecoveryRecord, PreparedGpuHistoryRecoverySwap,
    },
    gpu_raster_recovery::GpuExactRasterRecoveryTransition,
    gpu_recovery_journal::{
        DEFAULT_GPU_RECOVERY_JOURNAL_BYTES, DEFAULT_GPU_RECOVERY_JOURNAL_ENTRIES,
    },
    gpu_recovery_replay::{
        replay_gpu_raster_recovery, GpuRasterRecoveryCommand, GpuRasterRecoveryReplayError,
    },
    gpu_round::{RoundMaskBatch, RoundMaskError, RoundMaskScheduler},
    raster::RasterLayer,
    stroke::RoundPathCommand,
};
use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt, iter,
};

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

impl GpuResidentDocumentLimits {
    /// Reserve enough budget for one stroke over every resident tile of a layer.
    /// These are ceilings; buffers are allocated only for actual stroke damage.
    pub fn for_canvas(canvas: [u32; 2], layout: AtlasLayout) -> Self {
        let defaults = Self::default();
        let tiles = u64::from(canvas[0].div_ceil(layout.tile_size()))
            * u64::from(canvas[1].div_ceil(layout.tile_size()));
        let pixels =
            tiles.min(u64::from(layout.total_capacity())) * u64::from(layout.tile_size()).pow(2);
        let history_bytes = defaults
            .history_bytes
            .max(pixels * u64::from(crate::gpu_document_undo::GPU_UNDO_PIXEL_BYTES));
        // Recovery retains before/after pixels plus canonical region metadata.
        let metadata_allowance = 32 * 1024 * 1024;
        Self {
            history_bytes,
            mirror_snapshot_bytes: defaults.mirror_snapshot_bytes.max(history_bytes),
            recovery_journal_bytes: defaults
                .recovery_journal_bytes
                .max(history_bytes + metadata_allowance),
            recovery_spill_bytes: defaults
                .recovery_spill_bytes
                .max(2 * history_bytes + metadata_allowance),
            ..defaults
        }
    }
}

pub struct GpuResidentDocument {
    metadata: DocumentMetadata,
    target_id: GpuDocumentTargetId,
    atlas: SparseAtlasPlanner,
    history: GpuDocumentHistory,
    mirror: GpuMirrorDispatcher,
    recovery: GpuLiveRecovery,
    mirror_purposes: VecDeque<GpuMirrorRecoveryPurpose>,
    pending_mirror_handoff: Option<PendingGpuResidentMirrorHandoff>,
    active_round_stroke: Option<GpuResidentRoundStrokeId>,
    next_round_stroke_id: u64,
    archive: Option<HistoryStore>,
    archive_job: Option<ArchiveJob>,
    archive_failed: std::collections::HashSet<GpuHistoryId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuResidentRoundStrokeId(u64);

impl GpuResidentRoundStrokeId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

struct ArchiveJob {
    id: GpuHistoryId,
    task: Option<
        std::thread::JoinHandle<
            Result<crate::gpu_raster_recovery::GpuExactRasterRecoveryTransition, ArchiveError>,
        >,
    >,
}
impl Drop for ArchiveJob {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

impl GpuResidentDocument {
    pub fn enable_history_archive(&mut self, store: HistoryStore) {
        self.recovery.enable_archive();
        self.archive = Some(store);
    }
    pub fn archive_usage(&self) -> Option<ArchiveUsage> {
        self.archive.as_ref().map(HistoryStore::usage)
    }
    pub fn archive_busy(&self) -> bool {
        self.archive_job.is_some()
    }

    fn finish_archive(&mut self) {
        let Some(mut job) = self.archive_job.take() else {
            return;
        };
        match job
            .task
            .take()
            .unwrap()
            .join()
            .unwrap_or(Err(ArchiveError::Unavailable))
        {
            Ok(transition) => {
                // An edit may have branched or evicted history while compression ran.
                // Choose the memento side from the current stack, not the old job.
                if self.recovery.replace_archived(job.id, transition.clone()) {
                    self.history
                        .archive(job.id, transition.before(), transition.after());
                } else {
                    self.archive_failed.insert(job.id);
                }
            }
            Err(error) => {
                log::warn!("Older undo compression failed; retaining existing history: {error}");
                self.archive_failed.insert(job.id);
            }
        }
    }

    fn start_archive(&mut self, keep_recent: usize) -> bool {
        let Some(store) = self.archive.clone() else {
            return false;
        };
        if self.archive_job.is_some() {
            return false;
        }
        // Bound failed-ID metadata to entries still owned by history.
        self.archive_failed
            .retain(|id| self.recovery.spills().entry(*id).is_some());
        for (id, _) in self.history.archive_candidates(keep_recent) {
            if self.archive_failed.contains(&id) {
                continue;
            }
            let Some(transition) = self
                .recovery
                .spills()
                .entry(id)
                .and_then(|e| e.transition())
                .cloned()
            else {
                continue;
            };
            if transition.is_archived() {
                return self
                    .history
                    .archive(id, transition.before(), transition.after());
            }
            match std::thread::Builder::new()
                .name("undo-compression".into())
                .spawn(move || transition.archived(&store))
            {
                Ok(task) => {
                    self.archive_job = Some(ArchiveJob {
                        id,
                        task: Some(task),
                    })
                }
                Err(_) => {
                    self.archive_failed.insert(id);
                }
            }
            return self.archive_job.is_some();
        }
        false
    }

    /// Compress off the input thread. Keep the nearest two edits on each side
    /// ready on the GPU; memory pressure can archive these too before a commit.
    pub fn poll_history_archive(&mut self) {
        if self
            .archive_job
            .as_ref()
            .is_some_and(|job| job.task.as_ref().unwrap().is_finished())
        {
            self.finish_archive();
        }
        while self.start_archive(2) && self.archive_job.is_none() {}
    }

    /// Called after mirror backpressure, before a new transaction is prepared.
    /// Cache failure only falls back to normal bounded oldest-history eviction.
    pub fn make_history_room(&mut self, incoming: u64) {
        if self.archive.is_none() {
            return;
        }
        let target = self.history.max_bytes().saturating_sub(incoming);
        if self.history.resident_bytes() <= target {
            if self
                .archive_job
                .as_ref()
                .is_some_and(|job| job.task.as_ref().unwrap().is_finished())
            {
                self.finish_archive();
            }
            return;
        }
        self.finish_archive();
        while self.history.resident_bytes() > target {
            if !self.start_archive(0) {
                break;
            }
            self.finish_archive();
        }
    }

    pub fn new(
        metadata: DocumentMetadata,
        target: &GpuDocumentTarget,
        limits: GpuResidentDocumentLimits,
    ) -> Result<Self, GpuResidentDocumentError> {
        Self::new_with_target_identity(metadata, target.layout(), target.id(), limits)
    }

    fn new_with_target_identity(
        metadata: DocumentMetadata,
        layout: AtlasLayout,
        target_id: GpuDocumentTargetId,
        limits: GpuResidentDocumentLimits,
    ) -> Result<Self, GpuResidentDocumentError> {
        if metadata.tile_size() != layout.tile_size() {
            return Err(GpuResidentDocumentError::MetadataTileSizeMismatch {
                metadata: metadata.tile_size(),
                atlas: layout.tile_size(),
            });
        }
        let mirror = GpuCpuMirror::new(
            metadata.width(),
            metadata.height(),
            metadata.tile_size(),
            metadata.revision(),
        )
        .map_err(GpuMirrorDispatchError::from)?;
        Self::new_with_mirror(metadata, layout, target_id, mirror, limits)
    }

    fn new_with_mirror(
        metadata: DocumentMetadata,
        layout: AtlasLayout,
        target_id: GpuDocumentTargetId,
        mirror: GpuCpuMirror,
        limits: GpuResidentDocumentLimits,
    ) -> Result<Self, GpuResidentDocumentError> {
        if metadata.tile_size() != layout.tile_size() {
            return Err(GpuResidentDocumentError::MetadataTileSizeMismatch {
                metadata: metadata.tile_size(),
                atlas: layout.tile_size(),
            });
        }
        if mirror.revision() != metadata.revision() {
            return Err(GpuResidentDocumentError::MirrorRevisionMismatch {
                metadata: metadata.revision(),
                mirror: mirror.revision(),
            });
        }
        let atlas = SparseAtlasPlanner::new(layout);
        let history = GpuDocumentHistory::new(limits.history_entries, limits.history_bytes)?;
        let mirror = GpuMirrorDispatcher::from_mirror(
            mirror,
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
            metadata,
            target_id,
            atlas,
            history,
            mirror,
            recovery,
            mirror_purposes: VecDeque::new(),
            pending_mirror_handoff: None,
            active_round_stroke: None,
            next_round_stroke_id: 1,
            archive: None,
            archive_job: None,
            archive_failed: Default::default(),
        })
    }

    pub fn from_cpu_document(
        document: &Document,
        target: &mut GpuDocumentTarget,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        limits: GpuResidentDocumentLimits,
    ) -> Result<GpuResidentDocumentBootstrap, GpuResidentDocumentError> {
        let metadata = DocumentMetadata::from_document(document);
        let mirror = GpuCpuMirror::from_document(document).map_err(GpuMirrorDispatchError::from)?;
        let mut resident =
            Self::new_with_mirror(metadata, target.layout(), target.id(), mirror, limits)?;
        let mut uploads = Vec::new();
        for layer in document.layers() {
            let raster = document
                .layer_raster(layer.id())
                .expect("every document layer must retain its raster payload");
            let mut tiles = raster.checkpoint_tiles();
            tiles.sort_unstable_by_key(|tile| (tile.coord.y, tile.coord.x));
            for tile in tiles {
                let key = LayerTileKey::new(layer.id(), tile.coord);
                let slot = resident.atlas.allocate(key)?.slot;
                uploads.push(GpuDocumentResidentUpload {
                    key,
                    slot,
                    pixels: tile.pixels,
                });
            }
        }
        let stats = target.upload_initial_residents(device, queue, &uploads)?;
        Ok(GpuResidentDocumentBootstrap {
            document: resident,
            stats,
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
        self.prepare_commit_internal(None, target, device, encoder, encoded, recovery_command)
    }

    pub(crate) fn prepare_round_stroke_commit(
        &self,
        stroke: GpuResidentRoundStrokeId,
        target: &GpuDocumentTarget,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        encoded: EncodedGpuDocumentCommit,
        recovery_command: GpuRasterRecoveryCommand,
    ) -> Result<PreparedGpuResidentDocumentCommit, Box<GpuResidentDocumentPrepareFailure>> {
        self.prepare_commit_internal(
            Some(stroke),
            target,
            device,
            encoder,
            encoded,
            recovery_command,
        )
    }

    fn prepare_commit_internal(
        &self,
        active_stroke: Option<GpuResidentRoundStrokeId>,
        target: &GpuDocumentTarget,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        encoded: EncodedGpuDocumentCommit,
        recovery_command: GpuRasterRecoveryCommand,
    ) -> Result<PreparedGpuResidentDocumentCommit, Box<GpuResidentDocumentPrepareFailure>> {
        if let Err(error) = self.check_active_round_stroke(active_stroke) {
            return Err(prepare_failure(error, encoded, recovery_command));
        }
        if let Err(error) = self.check_target(target) {
            return Err(prepare_failure(error, encoded, recovery_command));
        }
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
            history.evicted_raster_ids(),
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
        assert!(history
            .evicted
            .iter()
            .filter(|entry| entry.kind() == GpuHistoryEntryKind::Raster)
            .map(GpuHistoryEntry::id)
            .eq(recovery
                .evicted_spills
                .iter()
                .map(GpuHistoryRecoveryEntry::id)));
        self.mirror
            .enqueue(&prepared.plan, prepared.capture)
            .expect("mirror enqueue was checked immediately before submission");
        self.mirror_purposes
            .push_back(GpuMirrorRecoveryPurpose::History(history.id));
        self.metadata.set_revision(prepared.revision);
        let reclamation = self.reclaim_unreachable_layers(&history.evicted);
        Ok(GpuResidentDocumentCommit {
            revision: prepared.revision,
            history_id: history.id,
            stats,
            evicted_history: history.evicted,
            evicted_spills: recovery.evicted_spills,
            freed_spill_bytes: recovery.freed_spill_bytes,
            reclamation,
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
        if let Err(error) = self.check_active_round_stroke(None) {
            return Err(history_swap_prepare_failure(error));
        }
        if let Err(error) = self.check_target(target) {
            return Err(history_swap_prepare_failure(error));
        }
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
        if self
            .history
            .check_begin_kind(direction)
            .expect("the same checked history boundary has no pending operation")
            != Some(GpuHistoryEntryKind::Raster)
        {
            return Err(history_swap_prepare_failure(
                GpuDocumentHistoryError::EntryKindMismatch {
                    expected: GpuHistoryEntryKind::Raster,
                    actual: GpuHistoryEntryKind::Metadata,
                }
                .into(),
            ));
        }
        if self
            .archive_job
            .as_ref()
            .is_some_and(|job| job.task.as_ref().unwrap().is_finished())
        {
            self.finish_archive();
        }
        let incoming = self.history.next_hydration_bytes(direction);
        if incoming != 0 {
            self.make_history_room(incoming);
            if self.history.resident_bytes() > self.history.max_bytes().saturating_sub(incoming) {
                return Err(history_swap_prepare_failure(
                    GpuResidentDocumentError::Archive(ArchiveError::Full),
                ));
            }
        }
        if let Err(error) = self.history.hydrate_next(device, direction) {
            return Err(history_swap_prepare_failure(
                GpuResidentDocumentError::Archive(error),
            ));
        }
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
        self.metadata.set_revision(prepared.revision);
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
        self.check_target(target)?;
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

    fn check_target(&self, target: &GpuDocumentTarget) -> Result<(), GpuResidentDocumentError> {
        self.check_target_identity(target.id())
    }

    fn check_target_identity(
        &self,
        actual: GpuDocumentTargetId,
    ) -> Result<(), GpuResidentDocumentError> {
        if actual != self.target_id {
            return Err(GpuResidentDocumentError::TargetMismatch {
                expected: self.target_id,
                actual,
            });
        }
        Ok(())
    }

    pub fn prepare_next_mirror_readback(
        &mut self,
        device: &wgpu::Device,
        mut encoder: wgpu::CommandEncoder,
    ) -> Result<Option<PreparedGpuResidentMirrorReadback>, GpuResidentDocumentError> {
        if let Some(pending) = &self.pending_mirror_handoff {
            return Err(GpuResidentDocumentError::MirrorHandoffPending(
                pending.revision,
            ));
        }
        if !self.mirror.encode_next_readback(device, &mut encoder)? {
            return Ok(None);
        }
        Ok(Some(PreparedGpuResidentMirrorReadback { encoder }))
    }

    pub fn submit_mirror_readback(
        &mut self,
        queue: &wgpu::Queue,
        prepared: PreparedGpuResidentMirrorReadback,
    ) -> Result<GpuResidentMirrorReadbackSubmission, GpuResidentDocumentError> {
        let staging_bytes = self.mirror.staging_byte_len();
        queue.submit(iter::once(prepared.encoder.finish()));
        self.mirror.readback_submitted()?;
        self.mirror.begin_map()?;
        Ok(GpuResidentMirrorReadbackSubmission { staging_bytes })
    }

    pub fn discard_mirror_readback(
        &mut self,
        _prepared: PreparedGpuResidentMirrorReadback,
    ) -> Result<(), GpuResidentDocumentError> {
        self.mirror.readback_discarded()?;
        Ok(())
    }

    pub fn try_finish_mirror(
        &mut self,
    ) -> Result<Option<GpuResidentMirrorCompletion>, GpuResidentDocumentError> {
        if self.pending_mirror_handoff.is_some() {
            return self.retry_pending_mirror_handoff().map(Some);
        }
        let Some(completion) = self.mirror.try_finish()? else {
            return Ok(None);
        };
        let GpuMirrorDispatchCompletion {
            revision,
            batch_index,
            byte_len,
            applied_revisions,
            recovery_transition,
            recovery_snapshot,
        } = completion;
        let Some(transition) = recovery_transition else {
            debug_assert!(recovery_snapshot.is_none());
            return Ok(Some(GpuResidentMirrorCompletion {
                revision,
                batch_index,
                byte_len,
                applied_revisions,
                handoff: None,
            }));
        };
        let requested_purpose = *self
            .mirror_purposes
            .front()
            .expect("every completed resident mirror revision retains its recovery purpose");
        self.pending_mirror_handoff = Some(PendingGpuResidentMirrorHandoff {
            revision,
            batch_index,
            byte_len,
            applied_revisions,
            requested_purpose,
            snapshot: recovery_snapshot
                .expect("a completed mirror transition retains its exact revision snapshot"),
            transition,
            last_error: None,
        });
        self.retry_pending_mirror_handoff().map(Some)
    }

    fn retry_pending_mirror_handoff(
        &mut self,
    ) -> Result<GpuResidentMirrorCompletion, GpuResidentDocumentError> {
        let pending = self
            .pending_mirror_handoff
            .take()
            .expect("mirror handoff retry requires retained ownership");
        let purpose =
            self.resolve_mirror_purpose(pending.requested_purpose, pending.transition.revision());
        let prepared = match self.recovery.prepare_mirror_handoff(
            purpose,
            pending.snapshot,
            pending.transition,
        ) {
            Ok(prepared) => prepared,
            Err(failure) => {
                let error = failure.error;
                self.pending_mirror_handoff = Some(PendingGpuResidentMirrorHandoff {
                    revision: pending.revision,
                    batch_index: pending.batch_index,
                    byte_len: pending.byte_len,
                    applied_revisions: pending.applied_revisions,
                    requested_purpose: pending.requested_purpose,
                    snapshot: failure.snapshot,
                    transition: failure.transition,
                    last_error: Some(error),
                });
                return Err(error.into());
            }
        };
        let handoff = self
            .recovery
            .commit_mirror_handoff(prepared)
            .expect("a resident mirror handoff is committed immediately after preparation");
        let queued_purpose = self
            .mirror_purposes
            .pop_front()
            .expect("a completed resident mirror handoff retains its queued purpose");
        assert_eq!(queued_purpose, pending.requested_purpose);
        self.recovery
            .advance_structural_mirror(self.mirror.snapshot());
        Ok(GpuResidentMirrorCompletion {
            revision: pending.revision,
            batch_index: pending.batch_index,
            byte_len: pending.byte_len,
            applied_revisions: pending.applied_revisions,
            handoff: Some(handoff),
        })
    }

    fn resolve_mirror_purpose(
        &self,
        requested: GpuMirrorRecoveryPurpose,
        revision: DocumentRevision,
    ) -> GpuMirrorRecoveryPurpose {
        match requested {
            GpuMirrorRecoveryPurpose::History(id)
                if self.recovery.spills().id_for_revision(revision) != Some(id) =>
            {
                GpuMirrorRecoveryPurpose::ReconcileOnly
            }
            purpose => purpose,
        }
    }

    fn check_prepared_commit(
        &self,
        target: &GpuDocumentTarget,
        prepared: &PreparedGpuResidentDocumentCommit,
    ) -> Result<(), GpuResidentDocumentError> {
        self.check_target(target)?;
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

    pub const fn active_round_stroke(&self) -> Option<GpuResidentRoundStrokeId> {
        self.active_round_stroke
    }

    pub(crate) fn begin_round_stroke(
        &mut self,
        target: &GpuDocumentTarget,
        layer: crate::document::LayerId,
    ) -> Result<GpuResidentRoundStrokeId, GpuResidentDocumentError> {
        self.check_target(target)?;
        if target.layout() != self.atlas.layout() {
            return Err(GpuResidentDocumentError::TargetLayoutMismatch {
                expected: self.atlas.layout(),
                actual: target.layout(),
            });
        }
        if layer != self.metadata.active_layer() {
            return Err(GpuResidentDocumentError::ActiveRoundStrokeLayerMismatch {
                expected: self.metadata.active_layer(),
                actual: layer,
            });
        }
        self.check_active_round_stroke(None)?;
        let id = GpuResidentRoundStrokeId(self.next_round_stroke_id);
        self.next_round_stroke_id = self
            .next_round_stroke_id
            .checked_add(1)
            .ok_or(GpuResidentDocumentError::ActiveRoundStrokeIdExhausted)?;
        self.active_round_stroke = Some(id);
        Ok(id)
    }

    pub(crate) fn schedule_round_stroke(
        &mut self,
        id: GpuResidentRoundStrokeId,
        scheduler: &mut RoundMaskScheduler,
        commands: &[RoundPathCommand],
    ) -> Result<RoundMaskBatch, GpuResidentDocumentError> {
        self.check_active_round_stroke(Some(id))?;
        if scheduler.layer() != self.metadata.active_layer() {
            return Err(GpuResidentDocumentError::ActiveRoundStrokeLayerMismatch {
                expected: self.metadata.active_layer(),
                actual: scheduler.layer(),
            });
        }
        Ok(scheduler.schedule(&mut self.atlas, commands)?)
    }

    pub(crate) fn rollback_round_stroke_allocations(
        &mut self,
        id: GpuResidentRoundStrokeId,
        allocations: &[AtlasAllocation],
    ) -> Result<(), GpuResidentDocumentError> {
        self.check_active_round_stroke(Some(id))?;
        for allocation in allocations
            .iter()
            .filter(|allocation| allocation.newly_allocated)
        {
            if self.atlas.slot(allocation.key) != Some(allocation.slot)
                || self.atlas.pin_count(allocation.key) != 0
            {
                return Err(
                    GpuResidentDocumentError::ActiveRoundStrokeAllocationChanged {
                        key: allocation.key,
                        expected: allocation.slot,
                        actual: self.atlas.slot(allocation.key),
                    },
                );
            }
        }
        for allocation in allocations
            .iter()
            .rev()
            .filter(|allocation| allocation.newly_allocated)
        {
            let released = self.atlas.release(allocation.key)?;
            debug_assert_eq!(released, Some(allocation.slot));
        }
        Ok(())
    }

    pub(crate) fn finish_round_stroke(
        &mut self,
        id: GpuResidentRoundStrokeId,
    ) -> Result<(), GpuResidentDocumentError> {
        self.check_active_round_stroke(Some(id))?;
        self.active_round_stroke = None;
        Ok(())
    }

    pub(crate) fn check_active_round_stroke(
        &self,
        expected: Option<GpuResidentRoundStrokeId>,
    ) -> Result<(), GpuResidentDocumentError> {
        if self.active_round_stroke != expected {
            return Err(GpuResidentDocumentError::ActiveRoundStrokeMismatch {
                expected,
                actual: self.active_round_stroke,
            });
        }
        Ok(())
    }

    pub const fn metadata(&self) -> &DocumentMetadata {
        &self.metadata
    }

    pub const fn target_id(&self) -> GpuDocumentTargetId {
        self.target_id
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

    pub fn recovery_snapshot(&self) -> GpuDocumentRecoverySnapshot {
        GpuDocumentRecoverySnapshot::new(self.metadata.clone(), self.recovery.timeline().snapshot())
            .expect("resident metadata and recovery timeline retain one exact revision")
    }

    pub fn set_active_layer(
        &mut self,
        layer: crate::document::LayerId,
    ) -> Result<(), GpuResidentDocumentError> {
        self.check_active_round_stroke(None)?;
        self.metadata.set_active_layer(layer)?;
        Ok(())
    }

    pub fn set_layer_visibility(
        &mut self,
        layer: crate::document::LayerId,
        visible: bool,
    ) -> Result<Option<GpuResidentMetadataCommit>, Box<GpuResidentMetadataEditFailure>> {
        let edit = match self.metadata.prepare_layer_visibility(layer, visible) {
            Ok(Some(edit)) => edit,
            Ok(None) => return Ok(None),
            Err(error) => return Err(metadata_edit_failure(error.into(), None)),
        };
        self.apply_metadata_edit(edit).map(Some)
    }

    pub fn create_layer(
        &mut self,
        name: impl Into<String>,
    ) -> Result<
        (crate::document::LayerId, GpuResidentMetadataCommit),
        Box<GpuResidentMetadataEditFailure>,
    > {
        let edit = match self.metadata.prepare_create_layer(name) {
            Ok(edit) => edit,
            Err(error) => return Err(metadata_edit_failure(error.into(), None)),
        };
        let layer = edit.layer();
        self.apply_metadata_edit(edit).map(|commit| (layer, commit))
    }

    pub fn duplicate_layer(
        &mut self,
        target: &mut GpuDocumentTarget,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: LayerId,
    ) -> Result<(LayerId, GpuResidentLayerCloneCommit), Box<GpuResidentMetadataEditFailure>> {
        if let Err(error) = self.check_active_round_stroke(None) {
            return Err(metadata_edit_failure(error, None));
        }
        if let Err(error) = self.check_target(target) {
            return Err(metadata_edit_failure(error, None));
        }
        let edit = match self.metadata.prepare_duplicate_layer(source) {
            Ok(edit) => edit,
            Err(error) => return Err(metadata_edit_failure(error.into(), None)),
        };
        let destination = edit.layer();
        let Some(revision) = self.revision().checked_next() else {
            return Err(metadata_edit_failure(
                GpuResidentDocumentError::RevisionExhausted,
                Some(edit),
            ));
        };
        let mut metadata = self.metadata.clone();
        if let Err(error) =
            metadata.apply_edit(&edit, DocumentMetadataEditDirection::Forward, revision)
        {
            return Err(metadata_edit_failure(error.into(), Some(edit)));
        }
        let history_preview = match self.history.check_metadata_record(&edit) {
            Ok(preview) => preview,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };
        let recovered =
            match replay_gpu_raster_recovery(&self.recovery.timeline().snapshot(), source) {
                Ok(recovered) => recovered,
                Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
            };
        let recovery_command =
            match GpuExactLayerRecoveryCommand::from_raster(destination, recovered.raster()) {
                Ok(command) => GpuRasterRecoveryCommand::from(command),
                Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
            };
        if let Err(error) =
            self.mirror
                .check_layer_clone_revision(self.revision(), revision, source, destination)
        {
            return Err(metadata_edit_failure(error.into(), Some(edit)));
        }
        let recovery = match self.recovery.prepare_structural_history_record(
            history_preview.evicted_raster_ids(),
            revision,
            recovery_command,
        ) {
            Ok(recovery) => recovery,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };

        let mut source_residents: Vec<_> = self
            .atlas
            .allocations()
            .filter(|(key, _)| key.layer == source)
            .collect();
        source_residents.sort_unstable_by_key(|(key, _)| (key.tile.y, key.tile.x));
        if let Some((key, slot, actual)) = source_residents.iter().find_map(|(key, slot)| {
            target
                .initialized_resident(*slot)
                .filter(|actual| actual != key)
                .map(|actual| (*key, *slot, actual))
        }) {
            return Err(metadata_edit_failure(
                GpuDocumentTargetError::ResidentCloneSourceMismatch {
                    slot,
                    expected: key,
                    actual: Some(actual),
                }
                .into(),
                Some(edit),
            ));
        }
        source_residents.retain(|(key, slot)| match target.initialized_resident(*slot) {
            Some(actual) => actual == *key,
            None => false,
        });
        let destination_keys: Vec<_> = source_residents
            .iter()
            .map(|(key, _)| LayerTileKey::new(destination, key.tile))
            .collect();
        let allocations = match self.atlas.allocate_batch(destination_keys) {
            Ok(allocations) => allocations,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };
        debug_assert!(allocations
            .iter()
            .all(|allocation| allocation.newly_allocated));
        let clones: Vec<_> = source_residents
            .iter()
            .zip(&allocations)
            .map(
                |((source_key, source_slot), destination)| GpuDocumentResidentClone {
                    source_key: *source_key,
                    source_slot: *source_slot,
                    destination_key: destination.key,
                    destination_slot: destination.slot,
                },
            )
            .collect();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Resident Layer Clone"),
        });
        let encoded = match target.encode_resident_clone(device, &mut encoder, &clones) {
            Ok(encoded) => encoded,
            Err(error) => {
                for allocation in allocations.iter().rev() {
                    self.atlas
                        .release(allocation.key)
                        .expect("failed resident clone owns every destination allocation");
                }
                return Err(metadata_edit_failure(error.into(), Some(edit)));
            }
        };

        queue.submit(iter::once(encoder.finish()));
        let stats = encoded.stats();
        target
            .resident_clone_submitted(encoded)
            .expect("the newly encoded resident clone still owns its target token");
        let history = self
            .history
            .record_metadata(&mut self.atlas, edit)
            .expect("structural history was checked immediately before submission");
        assert!(history_preview.matches_record(&history));
        let recovery = self
            .recovery
            .commit_structural_history_record(recovery)
            .expect("structural recovery was checked immediately before submission");
        self.mirror
            .register_layer_clone_revision(self.revision(), revision, source, destination)
            .expect("mirror layer clone was checked immediately before submission");
        self.metadata = metadata;
        self.recovery
            .advance_structural_mirror(self.mirror.snapshot());
        assert!(history
            .evicted
            .iter()
            .filter(|entry| entry.kind() == GpuHistoryEntryKind::Raster)
            .map(GpuHistoryEntry::id)
            .eq(recovery
                .evicted_spills
                .iter()
                .map(GpuHistoryRecoveryEntry::id)));
        let reclamation = self.reclaim_unreachable_layers(&history.evicted);
        Ok((
            destination,
            GpuResidentLayerCloneCommit {
                revision,
                history_id: history.id,
                stats,
                evicted_history: history.evicted,
                evicted_spills: recovery.evicted_spills,
                freed_spill_bytes: recovery.freed_spill_bytes,
                reclamation,
            },
        ))
    }

    pub fn insert_raster_layer(
        &mut self,
        target: &mut GpuDocumentTarget,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        name: impl Into<String>,
        raster: RasterLayer,
    ) -> Result<(LayerId, GpuResidentLayerImportCommit), Box<GpuResidentMetadataEditFailure>> {
        if let Err(error) = self.check_active_round_stroke(None) {
            return Err(metadata_edit_failure(error, None));
        }
        if let Err(error) = self.check_target(target) {
            return Err(metadata_edit_failure(error, None));
        }
        if raster.width() != self.metadata.width()
            || raster.height() != self.metadata.height()
            || raster.tile_size() != self.metadata.tile_size()
        {
            return Err(metadata_edit_failure(
                GpuResidentDocumentError::ImportedLayerGeometryMismatch,
                None,
            ));
        }
        let edit = match self.metadata.prepare_create_layer(name) {
            Ok(edit) => edit,
            Err(error) => return Err(metadata_edit_failure(error.into(), None)),
        };
        let layer = edit.layer();
        let Some(revision) = self.revision().checked_next() else {
            return Err(metadata_edit_failure(
                GpuResidentDocumentError::RevisionExhausted,
                Some(edit),
            ));
        };
        let mut metadata = self.metadata.clone();
        if let Err(error) =
            metadata.apply_edit(&edit, DocumentMetadataEditDirection::Forward, revision)
        {
            return Err(metadata_edit_failure(error.into(), Some(edit)));
        }
        let history_preview = match self.history.check_metadata_record(&edit) {
            Ok(preview) => preview,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };
        let layer_snapshot = match GpuExactLayerRecoveryCommand::from_raster(layer, &raster) {
            Ok(command) => command,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };
        if let Err(error) =
            self.mirror
                .check_layer_snapshot_revision(self.revision(), revision, &layer_snapshot)
        {
            return Err(metadata_edit_failure(error.into(), Some(edit)));
        }
        let recovery = match self.recovery.prepare_structural_history_record(
            history_preview.evicted_raster_ids(),
            revision,
            GpuRasterRecoveryCommand::from(layer_snapshot.clone()),
        ) {
            Ok(recovery) => recovery,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };

        let mut tiles = raster.checkpoint_tiles();
        tiles.sort_unstable_by_key(|tile| (tile.coord.y, tile.coord.x));
        let keys = tiles
            .iter()
            .map(|tile| LayerTileKey::new(layer, tile.coord));
        let allocations = match self.atlas.allocate_batch(keys) {
            Ok(allocations) => allocations,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };
        debug_assert!(allocations
            .iter()
            .all(|allocation| allocation.newly_allocated));
        let uploads: Vec<_> = tiles
            .into_iter()
            .zip(&allocations)
            .map(|(tile, allocation)| GpuDocumentResidentUpload {
                key: allocation.key,
                slot: allocation.slot,
                pixels: tile.pixels,
            })
            .collect();
        let stats = match target.upload_residents(device, queue, &uploads) {
            Ok(stats) => stats,
            Err(error) => {
                for allocation in allocations.iter().rev() {
                    self.atlas
                        .release(allocation.key)
                        .expect("failed resident import owns every destination allocation");
                }
                return Err(metadata_edit_failure(error.into(), Some(edit)));
            }
        };

        let history = self
            .history
            .record_metadata(&mut self.atlas, edit)
            .expect("import history was checked immediately before upload");
        assert!(history_preview.matches_record(&history));
        let recovery = self
            .recovery
            .commit_structural_history_record(recovery)
            .expect("import recovery was checked immediately before upload");
        self.mirror
            .register_layer_snapshot_revision(self.revision(), revision, layer_snapshot)
            .expect("mirror import snapshot was checked immediately before upload");
        self.metadata = metadata;
        self.recovery
            .advance_structural_mirror(self.mirror.snapshot());
        assert!(history
            .evicted
            .iter()
            .filter(|entry| entry.kind() == GpuHistoryEntryKind::Raster)
            .map(GpuHistoryEntry::id)
            .eq(recovery
                .evicted_spills
                .iter()
                .map(GpuHistoryRecoveryEntry::id)));
        let reclamation = self.reclaim_unreachable_layers(&history.evicted);
        Ok((
            layer,
            GpuResidentLayerImportCommit {
                revision,
                history_id: history.id,
                stats,
                evicted_history: history.evicted,
                evicted_spills: recovery.evicted_spills,
                freed_spill_bytes: recovery.freed_spill_bytes,
                reclamation,
            },
        ))
    }

    pub fn delete_layer(
        &mut self,
        layer: crate::document::LayerId,
    ) -> Result<GpuResidentMetadataCommit, Box<GpuResidentMetadataEditFailure>> {
        let edit = match self.metadata.prepare_delete_layer(layer) {
            Ok(edit) => edit,
            Err(error) => return Err(metadata_edit_failure(error.into(), None)),
        };
        self.apply_metadata_edit(edit)
    }

    pub fn set_layer_opacity(
        &mut self,
        layer: crate::document::LayerId,
        opacity: f32,
    ) -> Result<Option<GpuResidentMetadataCommit>, Box<GpuResidentMetadataEditFailure>> {
        let edit = match self.metadata.prepare_layer_opacity(layer, opacity) {
            Ok(Some(edit)) => edit,
            Ok(None) => return Ok(None),
            Err(error) => return Err(metadata_edit_failure(error.into(), None)),
        };
        self.apply_metadata_edit(edit).map(Some)
    }

    pub fn rename_layer(
        &mut self,
        layer: LayerId,
        name: impl Into<String>,
    ) -> Result<Option<GpuResidentMetadataCommit>, Box<GpuResidentMetadataEditFailure>> {
        let edit = match self.metadata.prepare_layer_name(layer, name) {
            Ok(Some(edit)) => edit,
            Ok(None) => return Ok(None),
            Err(error) => return Err(metadata_edit_failure(error.into(), None)),
        };
        self.apply_metadata_edit(edit).map(Some)
    }

    pub fn move_layer(
        &mut self,
        layer: crate::document::LayerId,
        destination: usize,
    ) -> Result<Option<GpuResidentMetadataCommit>, Box<GpuResidentMetadataEditFailure>> {
        let edit = match self.metadata.prepare_layer_move(layer, destination) {
            Ok(Some(edit)) => edit,
            Ok(None) => return Ok(None),
            Err(error) => return Err(metadata_edit_failure(error.into(), None)),
        };
        self.apply_metadata_edit(edit).map(Some)
    }

    pub fn apply_metadata_edit(
        &mut self,
        edit: DocumentMetadataEdit,
    ) -> Result<GpuResidentMetadataCommit, Box<GpuResidentMetadataEditFailure>> {
        if let Err(error) = self.check_active_round_stroke(None) {
            return Err(metadata_edit_failure(error, Some(edit)));
        }
        let Some(revision) = self.revision().checked_next() else {
            return Err(metadata_edit_failure(
                GpuResidentDocumentError::RevisionExhausted,
                Some(edit),
            ));
        };
        let mut metadata = self.metadata.clone();
        if let Err(error) =
            metadata.apply_edit(&edit, DocumentMetadataEditDirection::Forward, revision)
        {
            return Err(metadata_edit_failure(error.into(), Some(edit)));
        }
        let history_preview = match self.history.check_metadata_record(&edit) {
            Ok(preview) => preview,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };
        let recovery = match self
            .recovery
            .prepare_metadata_history_record(history_preview.evicted_raster_ids(), revision)
        {
            Ok(recovery) => recovery,
            Err(error) => return Err(metadata_edit_failure(error.into(), Some(edit))),
        };
        if let Err(error) = self
            .mirror
            .check_metadata_revision(self.revision(), revision)
        {
            return Err(metadata_edit_failure(error.into(), Some(edit)));
        }

        let history = self
            .history
            .record_metadata(&mut self.atlas, edit)
            .expect("metadata history was checked immediately before recording");
        assert!(history_preview.matches_record(&history));
        let recovery = self
            .recovery
            .commit_metadata_history_record(recovery)
            .expect("metadata recovery was checked immediately before recording");
        self.mirror
            .register_metadata_revision(self.revision(), revision)
            .expect("metadata mirror revision was checked immediately before recording");
        self.metadata = metadata;
        self.recovery
            .advance_structural_mirror(self.mirror.snapshot());
        assert!(history
            .evicted
            .iter()
            .filter(|entry| entry.kind() == GpuHistoryEntryKind::Raster)
            .map(GpuHistoryEntry::id)
            .eq(recovery
                .evicted_spills
                .iter()
                .map(GpuHistoryRecoveryEntry::id)));
        let reclamation = self.reclaim_unreachable_layers(&history.evicted);
        Ok(GpuResidentMetadataCommit {
            revision,
            history_id: history.id,
            evicted_history: history.evicted,
            evicted_spills: recovery.evicted_spills,
            freed_spill_bytes: recovery.freed_spill_bytes,
            reclamation,
        })
    }

    fn reclaim_unreachable_layers(
        &mut self,
        evicted: &[GpuHistoryEntry],
    ) -> GpuResidentLayerReclamation {
        let candidates: HashSet<_> = evicted
            .iter()
            .flat_map(GpuHistoryEntry::referenced_layers)
            .collect();
        let mut candidates: Vec<_> = candidates.into_iter().collect();
        candidates.sort_by_key(|layer| layer.get());
        let mut reclamation = GpuResidentLayerReclamation::default();
        for layer in candidates {
            let present = self
                .metadata
                .layers()
                .iter()
                .any(|candidate| candidate.id() == layer);
            if present || self.history.references_layer(layer) {
                continue;
            }
            let released = self
                .atlas
                .release_layer(layer)
                .expect("an unreachable layer has no remaining history pins");
            reclamation.layers += 1;
            reclamation.atlas_tiles += released.len();
            reclamation.mirror_tiles_retired_immediately +=
                self.mirror.retire_layer_when_idle(layer);
        }
        reclamation
    }

    pub fn next_history_kind(
        &self,
        direction: GpuHistoryDirection,
    ) -> Result<Option<GpuHistoryEntryKind>, GpuResidentDocumentError> {
        Ok(self.history.check_begin_kind(direction)?)
    }

    pub fn swap_metadata_history(
        &mut self,
        direction: GpuHistoryDirection,
    ) -> Result<Option<GpuResidentMetadataHistorySwapCommit>, GpuResidentDocumentError> {
        self.check_active_round_stroke(None)?;
        let Some(history_id) = self.history.check_begin(direction)? else {
            return Ok(None);
        };
        let edit = self
            .history
            .next_metadata_edit(direction)?
            .expect("the same nonempty history boundary retains its metadata edit")
            .clone();
        let revision = self
            .revision()
            .checked_next()
            .ok_or(GpuResidentDocumentError::RevisionExhausted)?;
        let edit_direction = match direction {
            GpuHistoryDirection::Undo => DocumentMetadataEditDirection::Reverse,
            GpuHistoryDirection::Redo => DocumentMetadataEditDirection::Forward,
        };
        let mut metadata = self.metadata.clone();
        metadata.apply_edit(&edit, edit_direction, revision)?;
        let recovery = self
            .recovery
            .prepare_metadata_history_record(&[], revision)?;
        self.mirror
            .check_metadata_revision(self.revision(), revision)?;

        let began = match direction {
            GpuHistoryDirection::Undo => self.history.begin_undo(),
            GpuHistoryDirection::Redo => self.history.begin_redo(),
        }
        .expect("metadata history was checked immediately before beginning a swap");
        assert!(began, "a checked metadata history swap has an entry");
        let recovery = self
            .recovery
            .commit_metadata_history_record(recovery)
            .expect("metadata recovery was checked immediately before history swap");
        self.mirror
            .register_metadata_revision(self.revision(), revision)
            .expect("metadata mirror revision was checked immediately before history swap");
        let finished_id = self
            .history
            .finish_pending()
            .expect("the begun metadata history swap remains pending");
        assert_eq!(finished_id, history_id);
        self.metadata = metadata;
        self.recovery
            .advance_structural_mirror(self.mirror.snapshot());
        Ok(Some(GpuResidentMetadataHistorySwapCommit {
            revision,
            history_id,
            direction,
            edit,
            evicted_spills: recovery.evicted_spills,
            freed_spill_bytes: recovery.freed_spill_bytes,
        }))
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.metadata.revision()
    }

    pub fn pending_mirror_purpose_count(&self) -> usize {
        self.mirror_purposes.len()
    }

    pub fn pending_mirror_handoff_error(&self) -> Option<GpuLiveRecoveryError> {
        self.pending_mirror_handoff
            .as_ref()
            .and_then(|pending| pending.last_error)
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
    pub reclamation: GpuResidentLayerReclamation,
}

pub struct GpuResidentMetadataCommit {
    pub revision: DocumentRevision,
    pub history_id: GpuHistoryId,
    pub evicted_history: Vec<GpuHistoryEntry>,
    pub evicted_spills: Vec<GpuHistoryRecoveryEntry>,
    pub freed_spill_bytes: u64,
    pub reclamation: GpuResidentLayerReclamation,
}

pub struct GpuResidentLayerCloneCommit {
    pub revision: DocumentRevision,
    pub history_id: GpuHistoryId,
    pub stats: GpuDocumentResidentCloneStats,
    pub evicted_history: Vec<GpuHistoryEntry>,
    pub evicted_spills: Vec<GpuHistoryRecoveryEntry>,
    pub freed_spill_bytes: u64,
    pub reclamation: GpuResidentLayerReclamation,
}

pub struct GpuResidentLayerImportCommit {
    pub revision: DocumentRevision,
    pub history_id: GpuHistoryId,
    pub stats: GpuDocumentResidentUploadStats,
    pub evicted_history: Vec<GpuHistoryEntry>,
    pub evicted_spills: Vec<GpuHistoryRecoveryEntry>,
    pub freed_spill_bytes: u64,
    pub reclamation: GpuResidentLayerReclamation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuResidentLayerReclamation {
    pub layers: usize,
    pub atlas_tiles: usize,
    pub mirror_tiles_retired_immediately: usize,
}

pub struct GpuResidentMetadataHistorySwapCommit {
    pub revision: DocumentRevision,
    pub history_id: GpuHistoryId,
    pub direction: GpuHistoryDirection,
    pub edit: DocumentMetadataEdit,
    pub evicted_spills: Vec<GpuHistoryRecoveryEntry>,
    pub freed_spill_bytes: u64,
}

pub struct GpuResidentDocumentBootstrap {
    document: GpuResidentDocument,
    stats: GpuDocumentBootstrapStats,
}

impl GpuResidentDocumentBootstrap {
    pub const fn document(&self) -> &GpuResidentDocument {
        &self.document
    }

    pub const fn stats(&self) -> GpuDocumentBootstrapStats {
        self.stats
    }

    pub fn into_document(self) -> GpuResidentDocument {
        self.document
    }
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

pub struct PreparedGpuResidentMirrorReadback {
    encoder: wgpu::CommandEncoder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuResidentMirrorReadbackSubmission {
    pub staging_bytes: u64,
}

pub struct GpuResidentMirrorCompletion {
    pub revision: DocumentRevision,
    pub batch_index: u32,
    pub byte_len: u64,
    pub applied_revisions: Vec<DocumentRevision>,
    pub handoff: Option<GpuMirrorRecoveryCommit>,
}

struct PendingGpuResidentMirrorHandoff {
    revision: DocumentRevision,
    batch_index: u32,
    byte_len: u64,
    applied_revisions: Vec<DocumentRevision>,
    requested_purpose: GpuMirrorRecoveryPurpose,
    snapshot: GpuCpuMirrorSnapshot,
    transition: GpuExactRasterRecoveryTransition,
    last_error: Option<GpuLiveRecoveryError>,
}

pub struct GpuResidentMetadataEditFailure {
    pub error: GpuResidentDocumentError,
    pub edit: Option<DocumentMetadataEdit>,
}

impl fmt::Debug for GpuResidentMetadataEditFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuResidentMetadataEditFailure")
            .field("error", &self.error)
            .field("edit", &self.edit)
            .finish()
    }
}

impl fmt::Display for GpuResidentMetadataEditFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuResidentMetadataEditFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

fn metadata_edit_failure(
    error: GpuResidentDocumentError,
    edit: Option<DocumentMetadataEdit>,
) -> Box<GpuResidentMetadataEditFailure> {
    Box::new(GpuResidentMetadataEditFailure { error, edit })
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
    Archive(ArchiveError),
    RevisionExhausted,
    ImportedLayerGeometryMismatch,
    MirrorRevisionMismatch {
        metadata: DocumentRevision,
        mirror: DocumentRevision,
    },
    MetadataTileSizeMismatch {
        metadata: u32,
        atlas: u32,
    },
    TargetMismatch {
        expected: GpuDocumentTargetId,
        actual: GpuDocumentTargetId,
    },
    TargetLayoutMismatch {
        expected: AtlasLayout,
        actual: AtlasLayout,
    },
    HistoryPreviewChanged,
    HistorySwapChanged,
    MirrorHandoffPending(DocumentRevision),
    ActiveRoundStrokeIdExhausted,
    ActiveRoundStrokeMismatch {
        expected: Option<GpuResidentRoundStrokeId>,
        actual: Option<GpuResidentRoundStrokeId>,
    },
    ActiveRoundStrokeLayerMismatch {
        expected: crate::document::LayerId,
        actual: crate::document::LayerId,
    },
    ActiveRoundStrokeAllocationChanged {
        key: LayerTileKey,
        expected: crate::gpu_atlas::AtlasSlot,
        actual: Option<crate::gpu_atlas::AtlasSlot>,
    },
    RoundMask(RoundMaskError),
    Atlas(AtlasError),
    History(GpuDocumentHistoryError),
    HistoryRecord(GpuHistoryRecordError),
    Target(GpuDocumentTargetError),
    MirrorPlan(GpuMirrorPlanError),
    MirrorReadback(GpuMirrorReadbackError),
    MirrorDispatch(GpuMirrorDispatchError),
    Recovery(GpuLiveRecoveryError),
    RecoveryReplay(GpuRasterRecoveryReplayError),
    LayerRecoveryBuild(GpuExactLayerRecoveryBuildError),
    Metadata(DocumentMetadataError),
}

impl fmt::Display for GpuResidentDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Archive(error) => error.fmt(formatter),
            Self::RevisionExhausted => {
                write!(formatter, "GPU document revision space is exhausted")
            }
            Self::ImportedLayerGeometryMismatch => {
                write!(
                    formatter,
                    "imported raster geometry does not match the resident document"
                )
            }
            Self::MirrorRevisionMismatch { metadata, mirror } => write!(
                formatter,
                "document metadata revision {} does not match CPU mirror revision {}",
                metadata.get(),
                mirror.get()
            ),
            Self::MetadataTileSizeMismatch { metadata, atlas } => write!(
                formatter,
                "document metadata tile size {metadata} does not match atlas tile size {atlas}"
            ),
            Self::TargetMismatch { expected, actual } => write!(
                formatter,
                "GPU resident document is bound to target {}, not target {}",
                expected.get(),
                actual.get()
            ),
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
            Self::MirrorHandoffPending(revision) => write!(
                formatter,
                "GPU mirror revision {} is waiting for recovery handoff",
                revision.get()
            ),
            Self::ActiveRoundStrokeIdExhausted => {
                write!(
                    formatter,
                    "GPU active round stroke identity space is exhausted"
                )
            }
            Self::ActiveRoundStrokeMismatch { expected, actual } => write!(
                formatter,
                "GPU active round stroke mismatch: expected {expected:?}, found {actual:?}"
            ),
            Self::ActiveRoundStrokeLayerMismatch { expected, actual } => write!(
                formatter,
                "GPU active round stroke layer {actual:?} does not match {expected:?}"
            ),
            Self::ActiveRoundStrokeAllocationChanged {
                key,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU active round stroke allocation {key:?} changed from {expected:?} to {actual:?}"
            ),
            Self::RoundMask(error) => error.fmt(formatter),
            Self::Atlas(error) => error.fmt(formatter),
            Self::History(error) => error.fmt(formatter),
            Self::HistoryRecord(error) => error.fmt(formatter),
            Self::Target(error) => error.fmt(formatter),
            Self::MirrorPlan(error) => error.fmt(formatter),
            Self::MirrorReadback(error) => error.fmt(formatter),
            Self::MirrorDispatch(error) => error.fmt(formatter),
            Self::Recovery(error) => error.fmt(formatter),
            Self::RecoveryReplay(error) => error.fmt(formatter),
            Self::LayerRecoveryBuild(error) => error.fmt(formatter),
            Self::Metadata(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuResidentDocumentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Atlas(error) => Some(error),
            Self::History(error) => Some(error),
            Self::HistoryRecord(error) => Some(error),
            Self::Target(error) => Some(error),
            Self::MirrorPlan(error) => Some(error),
            Self::MirrorReadback(error) => Some(error),
            Self::MirrorDispatch(error) => Some(error),
            Self::Recovery(error) => Some(error),
            Self::RecoveryReplay(error) => Some(error),
            Self::LayerRecoveryBuild(error) => Some(error),
            Self::RoundMask(error) => Some(error),
            Self::Metadata(error) => Some(error),
            _ => None,
        }
    }
}

impl From<GpuDocumentHistoryError> for GpuResidentDocumentError {
    fn from(error: GpuDocumentHistoryError) -> Self {
        Self::History(error)
    }
}

impl From<DocumentMetadataError> for GpuResidentDocumentError {
    fn from(error: DocumentMetadataError) -> Self {
        Self::Metadata(error)
    }
}

impl From<AtlasError> for GpuResidentDocumentError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
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

impl From<GpuRasterRecoveryReplayError> for GpuResidentDocumentError {
    fn from(error: GpuRasterRecoveryReplayError) -> Self {
        Self::RecoveryReplay(error)
    }
}

impl From<GpuExactLayerRecoveryBuildError> for GpuResidentDocumentError {
    fn from(error: GpuExactLayerRecoveryBuildError) -> Self {
        Self::LayerRecoveryBuild(error)
    }
}

impl From<RoundMaskError> for GpuResidentDocumentError {
    fn from(error: RoundMaskError) -> Self {
        Self::RoundMask(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gpu_document_undo::GPU_UNDO_BLOCK_BYTES,
        stroke::{RoundContact, RoundPathCommand},
    };

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

    fn test_document(
        layout: AtlasLayout,
        initial_revision: DocumentRevision,
        limits: GpuResidentDocumentLimits,
    ) -> Result<GpuResidentDocument, GpuResidentDocumentError> {
        let metadata =
            DocumentMetadata::new_blank(48, 40, layout.tile_size(), initial_revision).unwrap();
        GpuResidentDocument::new_with_target_identity(
            metadata,
            layout,
            GpuDocumentTargetId::from_raw(73),
            limits,
        )
    }

    #[test]
    fn owner_starts_all_subsystems_at_one_revision_and_budget() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let initial_revision = DocumentRevision::from_raw(19);
        let limits = limits();
        let document = test_document(layout, initial_revision, limits).unwrap();

        assert_eq!(document.target_id(), GpuDocumentTargetId::from_raw(73));
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
        assert_eq!(document.pending_mirror_handoff_error(), None);
        let wrong_target = GpuDocumentTargetId::from_raw(74);
        assert!(matches!(
            document.check_target_identity(wrong_target),
            Err(GpuResidentDocumentError::TargetMismatch {
                expected,
                actual
            }) if expected == document.target_id() && actual == wrong_target
        ));
    }

    #[test]
    fn active_layer_selection_is_metadata_only_and_respects_the_stroke_guard() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut cpu = Document::new(48, 40, 8).unwrap();
        let first = cpu.active_layer_id();
        let second = cpu.create_layer("Second").unwrap();
        let revision = cpu.revision();
        let mut document = GpuResidentDocument::new_with_target_identity(
            DocumentMetadata::from_document(&cpu),
            layout,
            GpuDocumentTargetId::from_raw(73),
            limits(),
        )
        .unwrap();

        document.set_active_layer(first).unwrap();
        assert_eq!(document.metadata().active_layer(), first);
        assert_eq!(document.revision(), revision);
        assert_eq!(document.history().undo_depth(), 0);
        assert_eq!(document.recovery().revision(), revision);

        document.active_round_stroke = Some(GpuResidentRoundStrokeId(9));
        assert!(matches!(
            document.set_active_layer(second),
            Err(GpuResidentDocumentError::ActiveRoundStrokeMismatch { .. })
        ));
        assert_eq!(document.metadata().active_layer(), first);
    }

    #[test]
    fn metadata_edits_commit_one_shared_history_and_recovery_revision() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut cpu = Document::new(48, 40, 8).unwrap();
        let first = cpu.active_layer_id();
        let second = cpu.create_layer("Second").unwrap();
        let initial_revision = cpu.revision();
        let mut document = GpuResidentDocument::new_with_target_identity(
            DocumentMetadata::from_document(&cpu),
            layout,
            GpuDocumentTargetId::from_raw(73),
            limits(),
        )
        .unwrap();

        let visibility = document
            .set_layer_visibility(second, false)
            .unwrap()
            .expect("visibility changed");
        assert_eq!(
            visibility.revision,
            initial_revision.checked_next().unwrap()
        );
        assert_eq!(document.history().undo_depth(), 1);
        assert!(!document.metadata().layers()[1].visible());
        assert_eq!(document.recovery().revision(), visibility.revision);
        assert_eq!(document.mirror().snapshot().revision(), visibility.revision);
        assert!(document.recovery().spills().is_empty());
        assert!(visibility.evicted_history.is_empty());
        assert!(visibility.evicted_spills.is_empty());

        let opacity = document
            .set_layer_opacity(second, 0.25)
            .unwrap()
            .expect("opacity changed");
        assert_eq!(
            opacity.revision,
            visibility.revision.checked_next().unwrap()
        );
        assert_eq!(document.history().undo_depth(), 2);
        assert_eq!(document.metadata().layers()[1].opacity(), 0.25);

        let moved = document
            .move_layer(second, 0)
            .unwrap()
            .expect("layer moved");
        assert_eq!(moved.revision, opacity.revision.checked_next().unwrap());
        assert_eq!(document.metadata().layers()[0].id(), second);
        assert_eq!(document.metadata().layers()[1].id(), first);
        assert_eq!(document.recovery_snapshot().revision(), moved.revision);

        assert!(document.move_layer(second, 0).unwrap().is_none());
        assert_eq!(document.revision(), moved.revision);
        let failure = document
            .set_layer_opacity(second, f32::NAN)
            .err()
            .expect("non-finite opacity is rejected");
        assert!(failure.edit.is_none());
        assert!(matches!(
            failure.error,
            GpuResidentDocumentError::Metadata(DocumentMetadataError::InvalidOpacity)
        ));
        assert_eq!(document.revision(), moved.revision);

        let undo_move = document
            .swap_metadata_history(GpuHistoryDirection::Undo)
            .unwrap()
            .expect("move is undoable");
        assert_eq!(undo_move.history_id, moved.history_id);
        assert_eq!(document.metadata().layers()[0].id(), first);
        assert_eq!(document.metadata().layers()[1].id(), second);
        assert_eq!(document.history().undo_depth(), 2);
        assert_eq!(document.history().redo_depth(), 1);

        document
            .swap_metadata_history(GpuHistoryDirection::Undo)
            .unwrap()
            .expect("opacity is undoable");
        assert_eq!(document.metadata().layers()[1].opacity(), 1.0);
        document
            .swap_metadata_history(GpuHistoryDirection::Undo)
            .unwrap()
            .expect("visibility is undoable");
        assert!(document.metadata().layers()[1].visible());
        assert_eq!(document.history().undo_depth(), 0);
        assert_eq!(document.history().redo_depth(), 3);

        let redo_visibility = document
            .swap_metadata_history(GpuHistoryDirection::Redo)
            .unwrap()
            .expect("visibility is redoable");
        assert!(!document.metadata().layers()[1].visible());
        assert_eq!(document.recovery().revision(), redo_visibility.revision);
        assert_eq!(
            document.mirror().snapshot().revision(),
            redo_visibility.revision
        );
        assert_eq!(
            document.recovery_snapshot().revision(),
            redo_visibility.revision
        );
    }

    #[test]
    fn resident_layer_name_is_one_reversible_metadata_revision() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut document = test_document(layout, DocumentRevision::INITIAL, limits()).unwrap();
        let layer = document.metadata().active_layer();

        let commit = document
            .rename_layer(layer, "Ink")
            .unwrap()
            .expect("name changed");
        assert_eq!(document.metadata().layers()[0].name(), "Ink");
        assert_eq!(document.recovery().revision(), commit.revision);
        assert_eq!(document.mirror().snapshot().revision(), commit.revision);
        assert!(document.rename_layer(layer, "Ink").unwrap().is_none());

        document
            .swap_metadata_history(GpuHistoryDirection::Undo)
            .unwrap()
            .expect("rename is undoable");
        assert_eq!(document.metadata().layers()[0].name(), "Layer 1");
        document
            .swap_metadata_history(GpuHistoryDirection::Redo)
            .unwrap()
            .expect("rename is redoable");
        assert_eq!(document.metadata().layers()[0].name(), "Ink");
    }

    #[test]
    fn metadata_edit_is_rejected_unchanged_during_an_active_stroke() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut document = test_document(layout, DocumentRevision::INITIAL, limits()).unwrap();
        let layer = document.metadata().active_layer();
        let edit = document
            .metadata()
            .prepare_layer_visibility(layer, false)
            .unwrap()
            .unwrap();
        document.active_round_stroke = Some(GpuResidentRoundStrokeId(9));

        let failure = document
            .apply_metadata_edit(edit.clone())
            .err()
            .expect("active stroke blocks metadata mutation");
        assert_eq!(failure.edit, Some(edit));
        assert!(matches!(
            failure.error,
            GpuResidentDocumentError::ActiveRoundStrokeMismatch { .. }
        ));
        assert!(document.metadata().layers()[0].visible());
        assert_eq!(document.history().undo_depth(), 0);
        assert_eq!(document.recovery().revision(), DocumentRevision::INITIAL);
    }

    #[test]
    fn empty_layer_creation_is_reversible_without_raster_or_mirror_work() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut document = test_document(layout, DocumentRevision::INITIAL, limits()).unwrap();
        let original = document.metadata().active_layer();

        let (created, commit) = document.create_layer("Ink").unwrap();
        assert_ne!(created, original);
        assert_eq!(document.metadata().active_layer(), created);
        assert_eq!(document.metadata().layers().len(), 2);
        assert_eq!(document.metadata().layers()[1].name(), "Ink");
        assert_eq!(document.atlas().resident_tile_count(), 0);
        assert_eq!(document.mirror().pending_revision_count(), 0);
        assert_eq!(document.recovery().revision(), commit.revision);

        document.set_active_layer(original).unwrap();
        let undone = document
            .swap_metadata_history(GpuHistoryDirection::Undo)
            .unwrap()
            .expect("creation is undoable after selection changes");
        assert_eq!(undone.history_id, commit.history_id);
        assert_eq!(document.metadata().layers().len(), 1);
        assert_eq!(document.metadata().active_layer(), original);

        document
            .swap_metadata_history(GpuHistoryDirection::Redo)
            .unwrap()
            .expect("creation is redoable");
        assert_eq!(document.metadata().layers().len(), 2);
        assert_eq!(document.metadata().active_layer(), created);

        document
            .swap_metadata_history(GpuHistoryDirection::Undo)
            .unwrap();
        let (newer, _) = document.create_layer("Paint").unwrap();
        assert!(
            newer.get() > created.get(),
            "undone layer IDs are not reused"
        );
        assert_eq!(document.history().redo_depth(), 0);
    }

    #[test]
    fn deleted_layer_payload_stays_reachable_then_reclaims_on_branch() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut document = test_document(layout, DocumentRevision::INITIAL, limits()).unwrap();
        let original = document.metadata().active_layer();
        let (created, _) = document.create_layer("Ink").unwrap();
        let key = LayerTileKey::new(created, crate::raster::TileCoord::new(0, 0));
        document.atlas.allocate(key).unwrap();

        let deletion = document.delete_layer(created).unwrap();
        assert_eq!(document.metadata().layers().len(), 1);
        assert_eq!(document.metadata().active_layer(), original);
        assert_eq!(document.atlas().resident_tile_count(), 1);
        assert_eq!(deletion.reclamation, GpuResidentLayerReclamation::default());

        document
            .swap_metadata_history(GpuHistoryDirection::Undo)
            .unwrap()
            .expect("deletion is undoable");
        assert_eq!(document.metadata().layers().len(), 2);
        assert_eq!(document.metadata().active_layer(), created);
        assert!(document.atlas().slot(key).is_some());

        document
            .swap_metadata_history(GpuHistoryDirection::Undo)
            .unwrap()
            .expect("creation is undoable");
        assert_eq!(document.metadata().layers().len(), 1);
        assert_eq!(document.atlas().resident_tile_count(), 1);

        let (_, branch) = document.create_layer("Paint").unwrap();
        assert_eq!(branch.reclamation.layers, 1);
        assert_eq!(branch.reclamation.atlas_tiles, 1);
        assert!(document.atlas().slot(key).is_none());
        assert_eq!(document.history().redo_depth(), 0);
    }

    #[test]
    fn deleting_the_last_resident_layer_is_transactional() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut document = test_document(layout, DocumentRevision::INITIAL, limits()).unwrap();
        let layer = document.metadata().active_layer();
        let failure = document
            .delete_layer(layer)
            .err()
            .expect("the last layer cannot be deleted");
        assert!(failure.edit.is_none());
        assert!(matches!(
            failure.error,
            GpuResidentDocumentError::Metadata(DocumentMetadataError::CannotDeleteLastLayer)
        ));
        assert_eq!(document.metadata().layers().len(), 1);
        assert_eq!(document.history().undo_depth(), 0);
    }

    #[test]
    fn invalid_history_limit_fails_before_constructing_an_owner() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let result = GpuResidentDocument::new_with_target_identity(
            DocumentMetadata::new_blank(48, 40, 8, DocumentRevision::INITIAL).unwrap(),
            layout,
            GpuDocumentTargetId::from_raw(73),
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
        let result = GpuResidentDocument::new_with_target_identity(
            DocumentMetadata::new_blank(48, 40, 8, DocumentRevision::INITIAL).unwrap(),
            layout,
            GpuDocumentTargetId::from_raw(73),
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

    #[test]
    fn owner_rejects_metadata_for_a_different_tile_geometry() {
        let result = GpuResidentDocument::new_with_target_identity(
            DocumentMetadata::new_blank(48, 40, 16, DocumentRevision::INITIAL).unwrap(),
            AtlasLayout::new(32, 8, 2).unwrap(),
            GpuDocumentTargetId::from_raw(73),
            limits(),
        );

        assert!(matches!(
            result,
            Err(GpuResidentDocumentError::MetadataTileSizeMismatch {
                metadata: 16,
                atlas: 8
            })
        ));
    }

    #[test]
    fn active_round_guard_tracks_and_reclaims_only_its_provisional_allocations() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut document = test_document(layout, DocumentRevision::INITIAL, limits()).unwrap();
        let id = GpuResidentRoundStrokeId(41);
        document.active_round_stroke = Some(id);
        let layer = document.metadata().active_layer();
        let mut scheduler = RoundMaskScheduler::new([48, 40], layer, layout).unwrap();
        let contact = RoundContact {
            center: [4.0, 4.0],
            radius: 2.0,
            elapsed_micros: 0,
        };
        let commands = [
            RoundPathCommand::Begin(contact),
            RoundPathCommand::End {
                at: contact,
                elapsed_micros: 1,
            },
        ];

        let wrong = GpuResidentRoundStrokeId(42);
        assert!(matches!(
            document.schedule_round_stroke(wrong, &mut scheduler, &commands),
            Err(GpuResidentDocumentError::ActiveRoundStrokeMismatch {
                expected: Some(expected),
                actual: Some(actual),
            }) if expected == wrong && actual == id
        ));
        assert_eq!(document.atlas().resident_tile_count(), 0);

        let batch = document
            .schedule_round_stroke(id, &mut scheduler, &commands)
            .unwrap();
        assert_eq!(batch.allocations().len(), 1);
        assert!(batch.allocations()[0].newly_allocated);
        assert_eq!(document.atlas().resident_tile_count(), 1);

        document
            .rollback_round_stroke_allocations(id, batch.allocations())
            .unwrap();
        document.finish_round_stroke(id).unwrap();
        assert_eq!(document.atlas().resident_tile_count(), 0);
        assert_eq!(document.active_round_stroke(), None);
    }

    #[test]
    fn resident_snapshot_materializes_the_owned_revision_without_aliasing_metadata() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut document = test_document(layout, DocumentRevision::from_raw(7), limits()).unwrap();
        let snapshot = document.recovery_snapshot();
        document
            .metadata
            .set_revision(DocumentRevision::from_raw(8));

        assert_eq!(snapshot.revision(), DocumentRevision::from_raw(7));
        let recovered = snapshot.recover_document().unwrap();
        assert_eq!(
            recovered.document().revision(),
            DocumentRevision::from_raw(7)
        );
        assert_eq!(recovered.document().layers().len(), 1);
        assert_eq!(
            recovered.document().active_layer().allocated_tile_count(),
            0
        );
    }

    #[test]
    fn delayed_history_mapping_becomes_reconcile_only_after_eviction() {
        let layout = AtlasLayout::new(32, 8, 2).unwrap();
        let mut document = test_document(layout, DocumentRevision::INITIAL, limits()).unwrap();
        let first_id = GpuHistoryId::from_raw(1);
        let first_revision = DocumentRevision::from_raw(1);
        let first = document
            .recovery
            .prepare_history_record(
                first_id,
                &[],
                first_revision,
                GpuRasterRecoveryCommand::MetadataOnly,
            )
            .unwrap();
        document.recovery.commit_history_record(first).unwrap();
        assert_eq!(
            document.resolve_mirror_purpose(
                GpuMirrorRecoveryPurpose::History(first_id),
                first_revision,
            ),
            GpuMirrorRecoveryPurpose::History(first_id)
        );

        let second_id = GpuHistoryId::from_raw(2);
        let second = document
            .recovery
            .prepare_history_record(
                second_id,
                &[first_id],
                DocumentRevision::from_raw(2),
                GpuRasterRecoveryCommand::MetadataOnly,
            )
            .unwrap();
        document.recovery.commit_history_record(second).unwrap();
        assert_eq!(
            document.resolve_mirror_purpose(
                GpuMirrorRecoveryPurpose::History(first_id),
                first_revision,
            ),
            GpuMirrorRecoveryPurpose::ReconcileOnly
        );
    }
}
