use crate::{
    document::DocumentRevision,
    gpu_document_mirror::{
        GpuCpuMirror, GpuCpuMirrorSnapshot, GpuMirrorReadbackError, GpuMirrorReadbackPlan,
        GpuMirrorReconcileError, GpuMirrorReconciler, GpuMirrorRevisionCapture,
        DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
    },
    gpu_document_undo::GPU_UNDO_BLOCK_BYTES,
};
use std::{collections::VecDeque, error::Error, fmt};

pub const DEFAULT_GPU_MIRROR_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_GPU_MIRROR_STAGING_BYTES: u64 = DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT;

pub struct GpuMirrorDispatcher {
    reconciler: GpuMirrorReconciler,
    captures: VecDeque<GpuMirrorRevisionCapture>,
    resident_snapshot_bytes: u64,
    max_snapshot_bytes: u64,
    max_staging_bytes: u64,
}

impl GpuMirrorDispatcher {
    pub fn new(
        width: u32,
        height: u32,
        tile_size: u32,
        initial_revision: DocumentRevision,
        max_snapshot_bytes: u64,
        max_staging_bytes: u64,
    ) -> Result<Self, GpuMirrorDispatchError> {
        if max_snapshot_bytes < GPU_UNDO_BLOCK_BYTES {
            return Err(GpuMirrorDispatchError::InvalidSnapshotBudget {
                requested: max_snapshot_bytes,
                minimum: GPU_UNDO_BLOCK_BYTES,
            });
        }
        if max_staging_bytes < GPU_UNDO_BLOCK_BYTES {
            return Err(GpuMirrorDispatchError::InvalidStagingBudget {
                requested: max_staging_bytes,
                minimum: GPU_UNDO_BLOCK_BYTES,
            });
        }
        Ok(Self {
            reconciler: GpuMirrorReconciler::new(width, height, tile_size, initial_revision)?,
            captures: VecDeque::new(),
            resident_snapshot_bytes: 0,
            max_snapshot_bytes,
            max_staging_bytes,
        })
    }

    pub fn check_capacity(
        &self,
        plan: &GpuMirrorReadbackPlan,
    ) -> Result<(), GpuMirrorDispatchError> {
        if plan.batches().is_empty() {
            return Err(GpuMirrorDispatchError::EmptyRevisionPlan);
        }
        if let Some(batch) = plan
            .batches()
            .iter()
            .find(|batch| batch.byte_len() > self.max_staging_bytes)
        {
            return Err(GpuMirrorDispatchError::BatchExceedsStagingBudget {
                revision: plan.revision(),
                index: batch.index(),
                requested: batch.byte_len(),
                maximum: self.max_staging_bytes,
            });
        }
        if plan.byte_len() > self.max_snapshot_bytes {
            return Err(GpuMirrorDispatchError::CaptureExceedsSnapshotBudget {
                requested: plan.byte_len(),
                maximum: self.max_snapshot_bytes,
            });
        }
        let combined = self
            .resident_snapshot_bytes
            .checked_add(plan.byte_len())
            .ok_or(GpuMirrorDispatchError::SnapshotByteOverflow)?;
        if combined > self.max_snapshot_bytes {
            return Err(GpuMirrorDispatchError::SnapshotBudgetExhausted {
                resident: self.resident_snapshot_bytes,
                requested: plan.byte_len(),
                maximum: self.max_snapshot_bytes,
            });
        }
        Ok(())
    }

    pub fn enqueue(
        &mut self,
        plan: &GpuMirrorReadbackPlan,
        capture: GpuMirrorRevisionCapture,
    ) -> Result<(), Box<GpuMirrorEnqueueFailure>> {
        if !capture.capture_submission_acknowledged() {
            return Err(Box::new(GpuMirrorEnqueueFailure {
                error: GpuMirrorDispatchError::CaptureNotSubmitted,
                capture,
            }));
        }
        if !capture.matches_plan(plan) {
            return Err(Box::new(GpuMirrorEnqueueFailure {
                error: GpuMirrorDispatchError::CapturePlanMismatch,
                capture,
            }));
        }
        if let Err(error) = self.check_capacity(plan) {
            return Err(Box::new(GpuMirrorEnqueueFailure { error, capture }));
        }
        if let Err(error) = self.reconciler.register_plan(plan) {
            return Err(Box::new(GpuMirrorEnqueueFailure {
                error: error.into(),
                capture,
            }));
        }
        self.resident_snapshot_bytes = self
            .resident_snapshot_bytes
            .checked_add(capture.byte_len())
            .expect("capacity checking proved that mirror snapshot bytes fit");
        self.captures.push_back(capture);
        Ok(())
    }

    pub fn encode_next_readback(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<bool, GpuMirrorDispatchError> {
        let Some(capture) = self.captures.front_mut() else {
            return Ok(false);
        };
        let prepared = capture.encode_next_readback(device, encoder)?;
        if prepared && capture.staging_byte_len() > self.max_staging_bytes {
            return Err(GpuMirrorDispatchError::StagingBudgetInvariant {
                requested: capture.staging_byte_len(),
                maximum: self.max_staging_bytes,
            });
        }
        Ok(prepared)
    }

    pub fn readback_submitted(&mut self) -> Result<(), GpuMirrorDispatchError> {
        self.front_mut()?.readback_submitted()?;
        Ok(())
    }

    pub fn readback_discarded(&mut self) -> Result<(), GpuMirrorDispatchError> {
        self.front_mut()?.readback_discarded()?;
        Ok(())
    }

    pub fn begin_map(&mut self) -> Result<(), GpuMirrorDispatchError> {
        self.front_mut()?.begin_map()?;
        Ok(())
    }

    pub fn try_finish(
        &mut self,
    ) -> Result<Option<GpuMirrorDispatchCompletion>, GpuMirrorDispatchError> {
        let (patch, revision_complete, completed_revision) = {
            let capture = self.front_mut()?;
            let Some(patch) = capture.try_finish()? else {
                return Ok(None);
            };
            (patch, capture.is_complete(), capture.revision())
        };
        let revision = patch.revision();
        let batch_index = patch.index();
        let byte_len = patch.byte_len();
        let applied_revisions = self.reconciler.complete_batch(patch)?;
        self.resident_snapshot_bytes = self
            .resident_snapshot_bytes
            .checked_sub(byte_len)
            .expect("a completed mirror batch owns its accounted snapshot bytes");
        if revision_complete {
            if applied_revisions.last().copied() != Some(completed_revision) {
                return Err(GpuMirrorDispatchError::CompletedRevisionNotApplied(
                    completed_revision,
                ));
            }
            self.captures.pop_front();
        }
        Ok(Some(GpuMirrorDispatchCompletion {
            revision,
            batch_index,
            byte_len,
            applied_revisions,
        }))
    }

    pub const fn mirror(&self) -> &GpuCpuMirror {
        self.reconciler.mirror()
    }

    pub fn snapshot(&self) -> GpuCpuMirrorSnapshot {
        self.reconciler.snapshot()
    }

    pub fn pending_revision_count(&self) -> usize {
        self.captures.len()
    }

    pub fn pending_batch_count(&self) -> usize {
        self.captures
            .iter()
            .map(GpuMirrorRevisionCapture::remaining_batch_count)
            .sum()
    }

    pub const fn resident_snapshot_bytes(&self) -> u64 {
        self.resident_snapshot_bytes
    }

    pub fn staging_byte_len(&self) -> u64 {
        self.captures
            .front()
            .map_or(0, GpuMirrorRevisionCapture::staging_byte_len)
    }

    pub const fn max_snapshot_bytes(&self) -> u64 {
        self.max_snapshot_bytes
    }

    pub const fn max_staging_bytes(&self) -> u64 {
        self.max_staging_bytes
    }

    fn front_mut(&mut self) -> Result<&mut GpuMirrorRevisionCapture, GpuMirrorDispatchError> {
        self.captures
            .front_mut()
            .ok_or(GpuMirrorDispatchError::NoPendingRevision)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuMirrorDispatchCompletion {
    pub revision: DocumentRevision,
    pub batch_index: u32,
    pub byte_len: u64,
    pub applied_revisions: Vec<DocumentRevision>,
}

pub struct GpuMirrorEnqueueFailure {
    pub error: GpuMirrorDispatchError,
    pub capture: GpuMirrorRevisionCapture,
}

impl fmt::Debug for GpuMirrorEnqueueFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuMirrorEnqueueFailure")
            .field("error", &self.error)
            .field("revision", &self.capture.revision())
            .field("capture_bytes", &self.capture.byte_len())
            .finish()
    }
}

impl fmt::Display for GpuMirrorEnqueueFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuMirrorEnqueueFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug)]
pub enum GpuMirrorDispatchError {
    InvalidSnapshotBudget {
        requested: u64,
        minimum: u64,
    },
    InvalidStagingBudget {
        requested: u64,
        minimum: u64,
    },
    EmptyRevisionPlan,
    CaptureNotSubmitted,
    CapturePlanMismatch,
    CaptureExceedsSnapshotBudget {
        requested: u64,
        maximum: u64,
    },
    SnapshotBudgetExhausted {
        resident: u64,
        requested: u64,
        maximum: u64,
    },
    BatchExceedsStagingBudget {
        revision: DocumentRevision,
        index: u32,
        requested: u64,
        maximum: u64,
    },
    SnapshotByteOverflow,
    StagingBudgetInvariant {
        requested: u64,
        maximum: u64,
    },
    NoPendingRevision,
    CompletedRevisionNotApplied(DocumentRevision),
    Readback(GpuMirrorReadbackError),
    Reconcile(GpuMirrorReconcileError),
}

impl fmt::Display for GpuMirrorDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSnapshotBudget { requested, minimum } => write!(
                formatter,
                "GPU mirror snapshot budget {requested} is smaller than one {minimum}-byte block"
            ),
            Self::InvalidStagingBudget { requested, minimum } => write!(
                formatter,
                "GPU mirror staging budget {requested} is smaller than one {minimum}-byte block"
            ),
            Self::EmptyRevisionPlan => write!(formatter, "GPU mirror revision plan is empty"),
            Self::CaptureNotSubmitted => {
                write!(formatter, "GPU mirror capture was not submitted")
            }
            Self::CapturePlanMismatch => {
                write!(formatter, "GPU mirror capture does not match its revision plan")
            }
            Self::CaptureExceedsSnapshotBudget { requested, maximum } => write!(
                formatter,
                "GPU mirror capture needs {requested} snapshot bytes but the limit is {maximum}"
            ),
            Self::SnapshotBudgetExhausted {
                resident,
                requested,
                maximum,
            } => write!(
                formatter,
                "GPU mirror has {resident} snapshot bytes and cannot reserve {requested} within {maximum}"
            ),
            Self::BatchExceedsStagingBudget {
                revision,
                index,
                requested,
                maximum,
            } => write!(
                formatter,
                "GPU mirror revision {} batch {index} needs {requested} staging bytes but the limit is {maximum}",
                revision.get()
            ),
            Self::SnapshotByteOverflow => write!(formatter, "GPU mirror snapshot bytes overflow"),
            Self::StagingBudgetInvariant { requested, maximum } => write!(
                formatter,
                "GPU mirror prepared {requested} staging bytes after accepting a {maximum}-byte limit"
            ),
            Self::NoPendingRevision => write!(formatter, "GPU mirror has no pending revision"),
            Self::CompletedRevisionNotApplied(revision) => write!(
                formatter,
                "GPU mirror completed revision {} without applying it",
                revision.get()
            ),
            Self::Readback(error) => error.fmt(formatter),
            Self::Reconcile(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuMirrorDispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Readback(error) => Some(error),
            Self::Reconcile(error) => Some(error),
            _ => None,
        }
    }
}

impl From<GpuMirrorReadbackError> for GpuMirrorDispatchError {
    fn from(error: GpuMirrorReadbackError) -> Self {
        Self::Readback(error)
    }
}

impl From<GpuMirrorReconcileError> for GpuMirrorDispatchError {
    fn from(error: GpuMirrorReconcileError) -> Self {
        Self::Reconcile(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::LayerId,
        gpu_atlas::{AtlasLayout, LayerTileKey, SparseAtlasPlanner},
        gpu_document_mirror::GpuMirrorReadbackPlan,
        gpu_document_undo::{GpuMementoResidentState, GpuUndoCapturePlan},
        gpu_round_target::ActiveRoundMaskTile,
        raster::{RectU32, TileCoord},
    };

    fn two_batch_plan() -> GpuMirrorReadbackPlan {
        let layout = AtlasLayout::document_default();
        let key = LayerTileKey::new(LayerId::from_raw(1), TileCoord::new(0, 0));
        let mut atlas = SparseAtlasPlanner::new(layout);
        let slot = atlas.allocate(key).unwrap().slot;
        let capture = GpuUndoCapturePlan::from_active_tiles(
            layout,
            &[ActiveRoundMaskTile {
                key,
                slot,
                local_damage: RectU32::from_xywh(16, 16, 32, 32).unwrap(),
            }],
        )
        .unwrap();
        GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::from_raw(1),
            &capture,
            &[GpuMementoResidentState {
                key,
                slot,
                document_initialized: true,
                memento_initialized: false,
            }],
            2 * GPU_UNDO_BLOCK_BYTES,
        )
        .unwrap()
    }

    #[test]
    fn budgets_must_hold_at_least_one_exact_block() {
        assert!(matches!(
            GpuMirrorDispatcher::new(
                128,
                128,
                128,
                DocumentRevision::INITIAL,
                GPU_UNDO_BLOCK_BYTES - 1,
                GPU_UNDO_BLOCK_BYTES,
            ),
            Err(GpuMirrorDispatchError::InvalidSnapshotBudget { .. })
        ));
        assert!(matches!(
            GpuMirrorDispatcher::new(
                128,
                128,
                128,
                DocumentRevision::INITIAL,
                GPU_UNDO_BLOCK_BYTES,
                GPU_UNDO_BLOCK_BYTES - 1,
            ),
            Err(GpuMirrorDispatchError::InvalidStagingBudget { .. })
        ));
    }

    #[test]
    fn capacity_checks_the_whole_capture_and_each_staging_batch() {
        let plan = two_batch_plan();
        assert_eq!(plan.byte_len(), 16_384);
        assert_eq!(plan.batches().len(), 2);

        let exact =
            GpuMirrorDispatcher::new(128, 128, 128, DocumentRevision::INITIAL, 16_384, 8_192)
                .unwrap();
        assert!(exact.check_capacity(&plan).is_ok());

        let snapshot_too_small =
            GpuMirrorDispatcher::new(128, 128, 128, DocumentRevision::INITIAL, 8_192, 8_192)
                .unwrap();
        assert!(matches!(
            snapshot_too_small.check_capacity(&plan),
            Err(GpuMirrorDispatchError::CaptureExceedsSnapshotBudget {
                requested: 16_384,
                maximum: 8_192,
            })
        ));

        let staging_too_small = GpuMirrorDispatcher::new(
            128,
            128,
            128,
            DocumentRevision::INITIAL,
            16_384,
            GPU_UNDO_BLOCK_BYTES,
        )
        .unwrap();
        assert!(matches!(
            staging_too_small.check_capacity(&plan),
            Err(GpuMirrorDispatchError::BatchExceedsStagingBudget {
                requested: 8_192,
                maximum: GPU_UNDO_BLOCK_BYTES,
                ..
            })
        ));
    }
}
