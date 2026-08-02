use crate::{
    document::DocumentRevision,
    gpu_document_mirror::GpuCpuMirrorSnapshot,
    gpu_recovery_journal::{
        GpuRecoveryJournal, GpuRecoveryJournalError, GpuRecoveryJournalSnapshot,
        GpuRecoveryRecordFailure, GpuRecoveryRetirement,
    },
};
use std::{error::Error, fmt, mem};

pub struct GpuRecoveryTimeline<C> {
    base: GpuCpuMirrorSnapshot,
    journal: GpuRecoveryJournal<C>,
}

impl<C> GpuRecoveryTimeline<C> {
    pub fn new(
        base: GpuCpuMirrorSnapshot,
        max_entries: usize,
        max_bytes: u64,
    ) -> Result<Self, GpuRecoveryJournalError> {
        let journal = GpuRecoveryJournal::new(base.revision(), max_entries, max_bytes)?;
        Ok(Self { base, journal })
    }

    pub fn check_record(
        &self,
        revision: DocumentRevision,
        byte_len: u64,
    ) -> Result<(), GpuRecoveryJournalError> {
        self.journal.check_record(revision, byte_len)
    }

    pub fn record(
        &mut self,
        revision: DocumentRevision,
        byte_len: u64,
        command: C,
    ) -> Result<(), Box<GpuRecoveryRecordFailure<C>>> {
        self.journal.record(revision, byte_len, command)
    }

    pub fn advance_base(
        &mut self,
        next: GpuCpuMirrorSnapshot,
    ) -> Result<GpuRecoveryBaseAdvance<C>, Box<GpuRecoveryBaseAdvanceFailure>> {
        if next.dimensions() != self.base.dimensions() || next.tile_size() != self.base.tile_size()
        {
            return Err(Box::new(GpuRecoveryBaseAdvanceFailure {
                error: GpuRecoveryTimelineError::GeometryMismatch {
                    expected_dimensions: self.base.dimensions(),
                    actual_dimensions: next.dimensions(),
                    expected_tile_size: self.base.tile_size(),
                    actual_tile_size: next.tile_size(),
                },
                base: next,
            }));
        }
        if next.revision() <= self.base.revision() {
            return Err(Box::new(GpuRecoveryBaseAdvanceFailure {
                error: GpuRecoveryTimelineError::BaseNotNewer {
                    current: self.base.revision(),
                    requested: next.revision(),
                },
                base: next,
            }));
        }
        let retirement = match self.journal.acknowledge_mirrored(next.revision()) {
            Ok(retirement) => retirement,
            Err(error) => {
                return Err(Box::new(GpuRecoveryBaseAdvanceFailure {
                    error: error.into(),
                    base: next,
                }));
            }
        };
        let previous_base = mem::replace(&mut self.base, next);
        debug_assert_eq!(
            self.base.revision(),
            self.journal.mirrored_revision(),
            "recovery base and journal retirement must advance together"
        );
        Ok(GpuRecoveryBaseAdvance {
            previous_base,
            retirement,
        })
    }

    pub fn snapshot(&self) -> GpuRecoveryTimelineSnapshot<C> {
        debug_assert_eq!(self.base.revision(), self.journal.mirrored_revision());
        GpuRecoveryTimelineSnapshot {
            base: self.base.clone(),
            journal: self.journal.snapshot(),
        }
    }

    pub const fn base(&self) -> &GpuCpuMirrorSnapshot {
        &self.base
    }

    pub const fn journal(&self) -> &GpuRecoveryJournal<C> {
        &self.journal
    }

    pub const fn base_revision(&self) -> DocumentRevision {
        self.base.revision()
    }

    pub const fn target_revision(&self) -> DocumentRevision {
        self.journal.latest_revision()
    }

    pub fn is_current(&self) -> bool {
        self.journal.is_current()
    }
}

pub struct GpuRecoveryTimelineSnapshot<C> {
    base: GpuCpuMirrorSnapshot,
    journal: GpuRecoveryJournalSnapshot<C>,
}

impl<C> Clone for GpuRecoveryTimelineSnapshot<C> {
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            journal: self.journal.clone(),
        }
    }
}

impl<C> GpuRecoveryTimelineSnapshot<C> {
    pub const fn base(&self) -> &GpuCpuMirrorSnapshot {
        &self.base
    }

    pub const fn journal(&self) -> &GpuRecoveryJournalSnapshot<C> {
        &self.journal
    }

    pub const fn base_revision(&self) -> DocumentRevision {
        self.base.revision()
    }

    pub const fn target_revision(&self) -> DocumentRevision {
        self.journal.target_revision()
    }

    pub fn is_current(&self) -> bool {
        self.journal.is_current()
    }
}

pub struct GpuRecoveryBaseAdvance<C> {
    pub previous_base: GpuCpuMirrorSnapshot,
    pub retirement: GpuRecoveryRetirement<C>,
}

pub struct GpuRecoveryBaseAdvanceFailure {
    pub error: GpuRecoveryTimelineError,
    pub base: GpuCpuMirrorSnapshot,
}

impl fmt::Debug for GpuRecoveryBaseAdvanceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuRecoveryBaseAdvanceFailure")
            .field("error", &self.error)
            .field("base_revision", &self.base.revision())
            .finish()
    }
}

impl fmt::Display for GpuRecoveryBaseAdvanceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuRecoveryBaseAdvanceFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuRecoveryTimelineError {
    GeometryMismatch {
        expected_dimensions: [u32; 2],
        actual_dimensions: [u32; 2],
        expected_tile_size: u32,
        actual_tile_size: u32,
    },
    BaseNotNewer {
        current: DocumentRevision,
        requested: DocumentRevision,
    },
    Journal(GpuRecoveryJournalError),
}

impl fmt::Display for GpuRecoveryTimelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GeometryMismatch {
                expected_dimensions,
                actual_dimensions,
                expected_tile_size,
                actual_tile_size,
            } => write!(
                formatter,
                "GPU recovery base geometry {}x{} / tile {} does not match {}x{} / tile {}",
                actual_dimensions[0],
                actual_dimensions[1],
                actual_tile_size,
                expected_dimensions[0],
                expected_dimensions[1],
                expected_tile_size
            ),
            Self::BaseNotNewer { current, requested } => write!(
                formatter,
                "GPU recovery base revision {} is not newer than {}",
                requested.get(),
                current.get()
            ),
            Self::Journal(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuRecoveryTimelineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Journal(error) => Some(error),
            _ => None,
        }
    }
}

impl From<GpuRecoveryJournalError> for GpuRecoveryTimelineError {
    fn from(error: GpuRecoveryJournalError) -> Self {
        Self::Journal(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_document_mirror::GpuCpuMirror;

    fn mirror(revision: u64, width: u32, height: u32, tile_size: u32) -> GpuCpuMirrorSnapshot {
        GpuCpuMirror::new(
            width,
            height,
            tile_size,
            DocumentRevision::from_raw(revision),
        )
        .unwrap()
        .snapshot()
    }

    #[test]
    fn base_and_journal_advance_as_one_revision_boundary() {
        let mut timeline = GpuRecoveryTimeline::new(mirror(0, 64, 64, 16), 8, 1_024).unwrap();
        timeline
            .record(DocumentRevision::from_raw(1), 10, "paint")
            .unwrap();
        timeline
            .record(DocumentRevision::from_raw(2), 20, "erase")
            .unwrap();
        let before = timeline.snapshot();

        let advance = timeline.advance_base(mirror(1, 64, 64, 16)).unwrap();
        assert_eq!(advance.previous_base.revision().get(), 0);
        assert_eq!(advance.retirement.retired_bytes, 10);
        assert_eq!(advance.retirement.records.len(), 1);
        assert_eq!(timeline.base_revision().get(), 1);
        assert_eq!(timeline.target_revision().get(), 2);
        assert_eq!(timeline.journal().len(), 1);
        assert_eq!(
            timeline.journal().records().next().unwrap().command(),
            &"erase"
        );

        assert_eq!(before.base_revision().get(), 0);
        assert_eq!(before.target_revision().get(), 2);
        assert_eq!(before.journal().records().len(), 2);
        assert_eq!(before.journal().records()[0].command(), &"paint");
    }

    #[test]
    fn failed_handoff_returns_the_exact_base_without_retiring_commands() {
        let mut timeline = GpuRecoveryTimeline::new(mirror(0, 64, 64, 16), 8, 1_024).unwrap();
        timeline
            .record(DocumentRevision::from_raw(1), 10, "paint")
            .unwrap();
        let wrong_geometry = mirror(1, 65, 64, 16);
        let failure = match timeline.advance_base(wrong_geometry) {
            Ok(_) => panic!("mismatched recovery geometry unexpectedly advanced"),
            Err(failure) => failure,
        };
        assert!(matches!(
            failure.error,
            GpuRecoveryTimelineError::GeometryMismatch { .. }
        ));
        assert_eq!(failure.base.dimensions(), [65, 64]);
        assert_eq!(timeline.base_revision().get(), 0);
        assert_eq!(timeline.journal().len(), 1);

        let ahead = match timeline.advance_base(mirror(2, 64, 64, 16)) {
            Ok(_) => panic!("a recovery base ahead of the journal unexpectedly advanced"),
            Err(failure) => failure,
        };
        assert_eq!(
            ahead.error,
            GpuRecoveryTimelineError::Journal(GpuRecoveryJournalError::MirrorRevisionAhead {
                latest: DocumentRevision::from_raw(1),
                requested: DocumentRevision::from_raw(2),
            })
        );
        assert_eq!(ahead.base.revision().get(), 2);
        assert_eq!(timeline.base_revision().get(), 0);
        assert_eq!(timeline.journal().len(), 1);
    }

    #[test]
    fn immutable_snapshot_keeps_shared_commands_after_full_retirement() {
        let mut timeline = GpuRecoveryTimeline::new(mirror(3, 64, 64, 16), 8, 1_024).unwrap();
        timeline
            .record(DocumentRevision::from_raw(4), 10, String::from("stroke"))
            .unwrap();
        let snapshot = timeline.snapshot();
        timeline.advance_base(mirror(4, 64, 64, 16)).unwrap();

        assert!(timeline.is_current());
        assert_eq!(timeline.base_revision().get(), 4);
        assert_eq!(snapshot.base_revision().get(), 3);
        assert_eq!(snapshot.target_revision().get(), 4);
        assert_eq!(snapshot.journal().records()[0].command(), "stroke");
    }
}
