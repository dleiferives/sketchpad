use crate::{
    document::DocumentRevision,
    gpu_document_history::{GpuHistoryDirection, GpuHistoryId},
    gpu_raster_recovery::{GpuExactRasterRecoveryCommand, GpuExactRasterRecoveryTransition},
};
use std::{collections::BTreeMap, error::Error, fmt, mem};

// A 64 MiB GPU history can contribute 128 MiB of before/after pixels. The
// remaining allowance bounds canonical tile/region metadata at the 256-entry
// default without pretending shared pixel allocations are free.
pub const DEFAULT_GPU_HISTORY_RECOVERY_BYTES: u64 = 160 * 1024 * 1024;
pub const DEFAULT_GPU_HISTORY_RECOVERY_ENTRIES: usize = 256;

pub struct GpuHistoryRecoverySpills {
    entries: BTreeMap<GpuHistoryId, GpuHistoryRecoveryEntry>,
    revisions: BTreeMap<DocumentRevision, GpuHistoryId>,
    ready_entries: usize,
    resident_bytes: u64,
    max_entries: usize,
    max_bytes: u64,
}

impl GpuHistoryRecoverySpills {
    pub fn new(max_entries: usize, max_bytes: u64) -> Result<Self, GpuHistoryRecoveryError> {
        if max_entries == 0 {
            return Err(GpuHistoryRecoveryError::InvalidEntryLimit);
        }
        if max_bytes == 0 {
            return Err(GpuHistoryRecoveryError::InvalidByteLimit);
        }
        Ok(Self {
            entries: BTreeMap::new(),
            revisions: BTreeMap::new(),
            ready_entries: 0,
            resident_bytes: 0,
            max_entries,
            max_bytes,
        })
    }

    pub fn register(
        &mut self,
        id: GpuHistoryId,
        source_revision: DocumentRevision,
        revision: DocumentRevision,
    ) -> Result<(), GpuHistoryRecoveryError> {
        if source_revision.get().checked_add(1) != Some(revision.get()) {
            return Err(GpuHistoryRecoveryError::NonConsecutiveRevision {
                source: source_revision,
                target: revision,
            });
        }
        if let Some(entry) = self.entries.get(&id) {
            return Err(GpuHistoryRecoveryError::DuplicateHistoryId {
                id,
                revision: entry.revision,
            });
        }
        if let Some(&registered_id) = self.revisions.get(&revision) {
            return Err(GpuHistoryRecoveryError::DuplicateRevision {
                revision,
                registered_id,
            });
        }
        if self.entries.len() == self.max_entries {
            return Err(GpuHistoryRecoveryError::EntryLimitExhausted {
                resident: self.entries.len(),
                maximum: self.max_entries,
            });
        }

        self.entries.insert(
            id,
            GpuHistoryRecoveryEntry {
                id,
                source_revision,
                revision,
                transition: None,
            },
        );
        self.revisions.insert(revision, id);
        Ok(())
    }

    pub fn attach(
        &mut self,
        transition: GpuExactRasterRecoveryTransition,
    ) -> Result<GpuHistoryRecoveryAttachment, Box<GpuHistoryRecoveryAttachFailure>> {
        let revision = transition.revision();
        let Some(&id) = self.revisions.get(&revision) else {
            return Err(attach_failure(
                GpuHistoryRecoveryError::UntrackedRevision(revision),
                transition,
            ));
        };
        let entry = self
            .entries
            .get(&id)
            .expect("the revision index names one retained recovery spill entry");
        if entry.transition.is_some() {
            return Err(attach_failure(
                GpuHistoryRecoveryError::TransitionAlreadyAttached { id, revision },
                transition,
            ));
        }
        if transition.source_revision() != entry.source_revision {
            return Err(attach_failure(
                GpuHistoryRecoveryError::SourceRevisionMismatch {
                    id,
                    expected: entry.source_revision,
                    actual: transition.source_revision(),
                },
                transition,
            ));
        }

        let byte_len = transition.retained_byte_len();
        if byte_len > self.max_bytes {
            return Err(attach_failure(
                GpuHistoryRecoveryError::TransitionExceedsByteLimit {
                    requested: byte_len,
                    maximum: self.max_bytes,
                },
                transition,
            ));
        }
        let Some(next_resident_bytes) = self.resident_bytes.checked_add(byte_len) else {
            return Err(attach_failure(
                GpuHistoryRecoveryError::ByteCountOverflow,
                transition,
            ));
        };
        if next_resident_bytes > self.max_bytes {
            return Err(attach_failure(
                GpuHistoryRecoveryError::ByteLimitExhausted {
                    resident: self.resident_bytes,
                    requested: byte_len,
                    maximum: self.max_bytes,
                },
                transition,
            ));
        }

        self.entries
            .get_mut(&id)
            .expect("the validated recovery spill entry remains retained")
            .transition = Some(transition);
        self.ready_entries += 1;
        self.resident_bytes = next_resident_bytes;
        Ok(GpuHistoryRecoveryAttachment {
            id,
            revision,
            byte_len,
        })
    }

    pub fn exact_command(
        &self,
        id: GpuHistoryId,
        direction: GpuHistoryDirection,
    ) -> Result<&GpuExactRasterRecoveryCommand, GpuHistoryRecoveryError> {
        let entry = self
            .entries
            .get(&id)
            .ok_or(GpuHistoryRecoveryError::UntrackedHistoryId(id))?;
        let transition =
            entry
                .transition
                .as_ref()
                .ok_or(GpuHistoryRecoveryError::TransitionPending {
                    id,
                    revision: entry.revision,
                })?;
        Ok(match direction {
            GpuHistoryDirection::Undo => transition.before(),
            GpuHistoryDirection::Redo => transition.after(),
        })
    }

    pub fn entry(&self, id: GpuHistoryId) -> Option<&GpuHistoryRecoveryEntry> {
        self.entries.get(&id)
    }

    pub fn id_for_revision(&self, revision: DocumentRevision) -> Option<GpuHistoryId> {
        self.revisions.get(&revision).copied()
    }

    pub fn remove(&mut self, id: GpuHistoryId) -> Option<GpuHistoryRecoveryEntry> {
        let entry = self.entries.remove(&id)?;
        let removed_id = self.revisions.remove(&entry.revision);
        debug_assert_eq!(removed_id, Some(id));
        if let Some(transition) = &entry.transition {
            self.ready_entries -= 1;
            self.resident_bytes = self
                .resident_bytes
                .checked_sub(transition.retained_byte_len())
                .expect("a ready recovery entry owns all of its accounted bytes");
        }
        Some(entry)
    }

    pub fn clear(&mut self) -> Vec<GpuHistoryRecoveryEntry> {
        self.revisions.clear();
        self.ready_entries = 0;
        self.resident_bytes = 0;
        mem::take(&mut self.entries).into_values().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub const fn ready_len(&self) -> usize {
        self.ready_entries
    }

    pub fn pending_len(&self) -> usize {
        self.entries.len() - self.ready_entries
    }

    pub const fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    pub const fn max_entries(&self) -> usize {
        self.max_entries
    }

    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

impl Default for GpuHistoryRecoverySpills {
    fn default() -> Self {
        Self::new(
            DEFAULT_GPU_HISTORY_RECOVERY_ENTRIES,
            DEFAULT_GPU_HISTORY_RECOVERY_BYTES,
        )
        .expect("the default GPU history recovery limits are nonzero")
    }
}

pub struct GpuHistoryRecoveryEntry {
    id: GpuHistoryId,
    source_revision: DocumentRevision,
    revision: DocumentRevision,
    transition: Option<GpuExactRasterRecoveryTransition>,
}

impl GpuHistoryRecoveryEntry {
    pub const fn id(&self) -> GpuHistoryId {
        self.id
    }

    pub const fn source_revision(&self) -> DocumentRevision {
        self.source_revision
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub fn transition(&self) -> Option<&GpuExactRasterRecoveryTransition> {
        self.transition.as_ref()
    }

    pub const fn retained_byte_len(&self) -> u64 {
        match &self.transition {
            Some(transition) => transition.retained_byte_len(),
            None => 0,
        }
    }

    pub fn into_transition(self) -> Option<GpuExactRasterRecoveryTransition> {
        self.transition
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuHistoryRecoveryAttachment {
    pub id: GpuHistoryId,
    pub revision: DocumentRevision,
    pub byte_len: u64,
}

pub struct GpuHistoryRecoveryAttachFailure {
    pub error: GpuHistoryRecoveryError,
    pub transition: GpuExactRasterRecoveryTransition,
}

impl fmt::Debug for GpuHistoryRecoveryAttachFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuHistoryRecoveryAttachFailure")
            .field("error", &self.error)
            .field("transition_revision", &self.transition.revision())
            .finish()
    }
}

impl fmt::Display for GpuHistoryRecoveryAttachFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuHistoryRecoveryAttachFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

fn attach_failure(
    error: GpuHistoryRecoveryError,
    transition: GpuExactRasterRecoveryTransition,
) -> Box<GpuHistoryRecoveryAttachFailure> {
    Box::new(GpuHistoryRecoveryAttachFailure { error, transition })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuHistoryRecoveryError {
    InvalidEntryLimit,
    InvalidByteLimit,
    NonConsecutiveRevision {
        source: DocumentRevision,
        target: DocumentRevision,
    },
    DuplicateHistoryId {
        id: GpuHistoryId,
        revision: DocumentRevision,
    },
    DuplicateRevision {
        revision: DocumentRevision,
        registered_id: GpuHistoryId,
    },
    EntryLimitExhausted {
        resident: usize,
        maximum: usize,
    },
    UntrackedHistoryId(GpuHistoryId),
    UntrackedRevision(DocumentRevision),
    SourceRevisionMismatch {
        id: GpuHistoryId,
        expected: DocumentRevision,
        actual: DocumentRevision,
    },
    TransitionAlreadyAttached {
        id: GpuHistoryId,
        revision: DocumentRevision,
    },
    TransitionPending {
        id: GpuHistoryId,
        revision: DocumentRevision,
    },
    TransitionExceedsByteLimit {
        requested: u64,
        maximum: u64,
    },
    ByteLimitExhausted {
        resident: u64,
        requested: u64,
        maximum: u64,
    },
    ByteCountOverflow,
}

impl fmt::Display for GpuHistoryRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEntryLimit => {
                write!(formatter, "GPU history recovery entry limit must be nonzero")
            }
            Self::InvalidByteLimit => {
                write!(formatter, "GPU history recovery byte limit must be nonzero")
            }
            Self::NonConsecutiveRevision { source, target } => write!(
                formatter,
                "GPU history recovery revision {} does not immediately follow {}",
                target.get(),
                source.get()
            ),
            Self::DuplicateHistoryId { id, revision } => write!(
                formatter,
                "GPU history ID {} is already registered for revision {}",
                id.get(),
                revision.get()
            ),
            Self::DuplicateRevision {
                revision,
                registered_id,
            } => write!(
                formatter,
                "GPU history recovery revision {} is already registered to history ID {}",
                revision.get(),
                registered_id.get()
            ),
            Self::EntryLimitExhausted { resident, maximum } => write!(
                formatter,
                "GPU history recovery has {resident} entries and cannot exceed {maximum}"
            ),
            Self::UntrackedHistoryId(id) => {
                write!(formatter, "GPU history ID {} has no recovery spill", id.get())
            }
            Self::UntrackedRevision(revision) => write!(
                formatter,
                "GPU history recovery revision {} is not registered",
                revision.get()
            ),
            Self::SourceRevisionMismatch {
                id,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU history ID {} expected recovery source revision {}, got {}",
                id.get(),
                expected.get(),
                actual.get()
            ),
            Self::TransitionAlreadyAttached { id, revision } => write!(
                formatter,
                "GPU history ID {} already has recovery revision {}",
                id.get(),
                revision.get()
            ),
            Self::TransitionPending { id, revision } => write!(
                formatter,
                "GPU history ID {} is still waiting for recovery revision {}",
                id.get(),
                revision.get()
            ),
            Self::TransitionExceedsByteLimit { requested, maximum } => write!(
                formatter,
                "GPU history recovery transition needs {requested} bytes but the limit is {maximum}"
            ),
            Self::ByteLimitExhausted {
                resident,
                requested,
                maximum,
            } => write!(
                formatter,
                "GPU history recovery has {resident} bytes and cannot add {requested} within {maximum}"
            ),
            Self::ByteCountOverflow => write!(formatter, "GPU history recovery bytes overflow"),
        }
    }
}

impl Error for GpuHistoryRecoveryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::LayerId,
        gpu_atlas::{AtlasLayout, LayerTileKey, SparseAtlasPlanner},
        gpu_document_mirror::{
            GpuCpuMirror, GpuMirrorPatchBatch, GpuMirrorPatchRegion, GpuMirrorReadbackPlan,
        },
        gpu_document_undo::{GpuMementoResidentState, GpuUndoCapturePlan, GPU_UNDO_BLOCK_BYTES},
        gpu_round_target::ActiveRoundMaskTile,
        raster::{LinearRgba, RectU32, TileCoord},
    };

    const RED: LinearRgba = LinearRgba::premultiplied(0.75, 0.0, 0.0, 0.75);

    fn transition(source_revision: u64, revision: u64) -> GpuExactRasterRecoveryTransition {
        let layout = AtlasLayout::new(32, 32, 1).unwrap();
        let layer = LayerId::from_raw(7);
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
        let source_revision = DocumentRevision::from_raw(source_revision);
        let revision = DocumentRevision::from_raw(revision);
        let plan = GpuMirrorReadbackPlan::from_capture(
            source_revision,
            revision,
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
            revision,
            batch.index(),
            vec![GpuMirrorPatchRegion {
                key,
                local_bounds: batch.regions()[0].local_bounds,
                initialized: true,
                pixels: vec![RED; 16 * 16].into(),
            }],
            batch.byte_len(),
        );
        let base = GpuCpuMirror::new(32, 32, 32, source_revision)
            .unwrap()
            .snapshot();
        GpuExactRasterRecoveryTransition::from_mirror_revision(&base, &plan, vec![mapped]).unwrap()
    }

    #[test]
    fn out_of_order_mapping_binds_to_the_registered_history_id() {
        let first_id = GpuHistoryId::from_raw(10);
        let second_id = GpuHistoryId::from_raw(11);
        let mut spills = GpuHistoryRecoverySpills::new(4, 1_000_000).unwrap();
        spills
            .register(
                first_id,
                DocumentRevision::INITIAL,
                DocumentRevision::from_raw(1),
            )
            .unwrap();
        spills
            .register(
                second_id,
                DocumentRevision::from_raw(1),
                DocumentRevision::from_raw(2),
            )
            .unwrap();

        let second = spills.attach(transition(1, 2)).unwrap();
        assert_eq!(second.id, second_id);
        assert_eq!(spills.ready_len(), 1);
        assert_eq!(spills.pending_len(), 1);
        let first = spills.attach(transition(0, 1)).unwrap();
        assert_eq!(first.id, first_id);
        assert_eq!(spills.ready_len(), 2);
        assert_eq!(
            spills.id_for_revision(DocumentRevision::from_raw(2)),
            Some(second_id)
        );
    }

    #[test]
    fn undo_and_redo_select_the_exact_opposite_sides() {
        let id = GpuHistoryId::from_raw(5);
        let mut spills = GpuHistoryRecoverySpills::new(1, 1_000_000).unwrap();
        spills
            .register(id, DocumentRevision::INITIAL, DocumentRevision::from_raw(1))
            .unwrap();
        assert_eq!(
            spills.exact_command(id, GpuHistoryDirection::Undo),
            Err(GpuHistoryRecoveryError::TransitionPending {
                id,
                revision: DocumentRevision::from_raw(1),
            })
        );
        spills.attach(transition(0, 1)).unwrap();

        let stored = spills.entry(id).unwrap().transition().unwrap();
        assert!(std::ptr::eq(
            spills.exact_command(id, GpuHistoryDirection::Undo).unwrap(),
            stored.before()
        ));
        assert!(std::ptr::eq(
            spills.exact_command(id, GpuHistoryDirection::Redo).unwrap(),
            stored.after()
        ));
    }

    #[test]
    fn rejected_attachments_return_ownership_without_mutating_the_entry() {
        let id = GpuHistoryId::from_raw(8);
        let mut spills = GpuHistoryRecoverySpills::new(2, 1_000_000).unwrap();
        spills
            .register(
                id,
                DocumentRevision::from_raw(1),
                DocumentRevision::from_raw(2),
            )
            .unwrap();

        let mismatch = spills.attach(transition(0, 2)).unwrap_err();
        assert_eq!(
            mismatch.transition.revision(),
            DocumentRevision::from_raw(2)
        );
        assert_eq!(
            mismatch.error,
            GpuHistoryRecoveryError::SourceRevisionMismatch {
                id,
                expected: DocumentRevision::from_raw(1),
                actual: DocumentRevision::INITIAL,
            }
        );
        assert_eq!(spills.pending_len(), 1);
        assert_eq!(spills.resident_bytes(), 0);

        spills.attach(transition(1, 2)).unwrap();
        let duplicate = spills.attach(transition(1, 2)).unwrap_err();
        assert_eq!(
            duplicate.error,
            GpuHistoryRecoveryError::TransitionAlreadyAttached {
                id,
                revision: DocumentRevision::from_raw(2),
            }
        );
        assert_eq!(spills.ready_len(), 1);
    }

    #[test]
    fn byte_pressure_does_not_evict_or_partially_attach_history() {
        let first = transition(0, 1);
        let byte_limit = first.retained_byte_len();
        let first_id = GpuHistoryId::from_raw(1);
        let second_id = GpuHistoryId::from_raw(2);
        let mut spills = GpuHistoryRecoverySpills::new(2, byte_limit).unwrap();
        spills
            .register(
                first_id,
                DocumentRevision::INITIAL,
                DocumentRevision::from_raw(1),
            )
            .unwrap();
        spills
            .register(
                second_id,
                DocumentRevision::from_raw(1),
                DocumentRevision::from_raw(2),
            )
            .unwrap();
        spills.attach(first).unwrap();

        let second = transition(1, 2);
        let second_bytes = second.retained_byte_len();
        let failure = spills.attach(second).unwrap_err();
        assert_eq!(
            failure.error,
            GpuHistoryRecoveryError::ByteLimitExhausted {
                resident: byte_limit,
                requested: second_bytes,
                maximum: byte_limit,
            }
        );
        assert_eq!(failure.transition.revision(), DocumentRevision::from_raw(2));
        assert_eq!(spills.ready_len(), 1);
        assert_eq!(spills.pending_len(), 1);
        assert_eq!(spills.resident_bytes(), byte_limit);

        let removed = spills.remove(first_id).unwrap();
        assert_eq!(removed.retained_byte_len(), byte_limit);
        assert_eq!(spills.resident_bytes(), 0);
        spills.attach(failure.transition).unwrap();
        assert_eq!(spills.ready_len(), 1);
        assert_eq!(spills.pending_len(), 0);
    }

    #[test]
    fn removing_gpu_history_ownership_makes_late_mapping_untracked() {
        let id = GpuHistoryId::from_raw(3);
        let mut spills = GpuHistoryRecoverySpills::new(2, 1_000_000).unwrap();
        spills
            .register(id, DocumentRevision::INITIAL, DocumentRevision::from_raw(1))
            .unwrap();
        let removed = spills.remove(id).unwrap();
        assert_eq!(removed.id(), id);
        assert!(removed.transition().is_none());

        let failure = spills.attach(transition(0, 1)).unwrap_err();
        assert_eq!(
            failure.error,
            GpuHistoryRecoveryError::UntrackedRevision(DocumentRevision::from_raw(1))
        );
        assert!(spills.is_empty());
    }

    #[test]
    fn registration_failures_preserve_existing_associations() {
        let id = GpuHistoryId::from_raw(1);
        let mut spills = GpuHistoryRecoverySpills::new(1, 1_000_000).unwrap();
        spills
            .register(id, DocumentRevision::INITIAL, DocumentRevision::from_raw(1))
            .unwrap();
        assert!(matches!(
            spills.register(
                id,
                DocumentRevision::from_raw(1),
                DocumentRevision::from_raw(2)
            ),
            Err(GpuHistoryRecoveryError::DuplicateHistoryId { .. })
        ));
        assert!(matches!(
            spills.register(
                GpuHistoryId::from_raw(2),
                DocumentRevision::INITIAL,
                DocumentRevision::from_raw(1)
            ),
            Err(GpuHistoryRecoveryError::DuplicateRevision { .. })
        ));
        assert!(matches!(
            spills.register(
                GpuHistoryId::from_raw(2),
                DocumentRevision::from_raw(1),
                DocumentRevision::from_raw(3)
            ),
            Err(GpuHistoryRecoveryError::NonConsecutiveRevision { .. })
        ));
        assert_eq!(spills.len(), 1);
        assert_eq!(
            spills.id_for_revision(DocumentRevision::from_raw(1)),
            Some(id)
        );
    }
}
