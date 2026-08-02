use crate::document::DocumentRevision;
use std::{collections::VecDeque, error::Error, fmt, sync::Arc};

pub const DEFAULT_GPU_RECOVERY_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_GPU_RECOVERY_JOURNAL_ENTRIES: usize = 4_096;

pub struct GpuRecoveryJournal<C> {
    mirrored_revision: DocumentRevision,
    latest_revision: DocumentRevision,
    entries: VecDeque<GpuRecoveryRecord<C>>,
    resident_bytes: u64,
    max_bytes: u64,
    max_entries: usize,
}

impl<C> GpuRecoveryJournal<C> {
    pub fn new(
        initial_revision: DocumentRevision,
        max_entries: usize,
        max_bytes: u64,
    ) -> Result<Self, GpuRecoveryJournalError> {
        if max_entries == 0 {
            return Err(GpuRecoveryJournalError::InvalidEntryLimit);
        }
        if max_bytes == 0 {
            return Err(GpuRecoveryJournalError::InvalidByteLimit);
        }
        Ok(Self {
            mirrored_revision: initial_revision,
            latest_revision: initial_revision,
            entries: VecDeque::new(),
            resident_bytes: 0,
            max_bytes,
            max_entries,
        })
    }

    pub fn record(
        &mut self,
        revision: DocumentRevision,
        byte_len: u64,
        command: C,
    ) -> Result<(), Box<GpuRecoveryRecordFailure<C>>> {
        if let Err(error) = self.check_record(revision, byte_len) {
            return Err(Box::new(GpuRecoveryRecordFailure { error, command }));
        }
        self.entries.push_back(GpuRecoveryRecord {
            revision,
            byte_len,
            command: Arc::new(command),
        });
        self.resident_bytes = self
            .resident_bytes
            .checked_add(byte_len)
            .expect("journal preflight proved that command bytes fit");
        self.latest_revision = revision;
        Ok(())
    }

    pub fn check_record(
        &self,
        revision: DocumentRevision,
        byte_len: u64,
    ) -> Result<(), GpuRecoveryJournalError> {
        let expected = match self.latest_revision.get().checked_add(1) {
            Some(expected) => expected,
            None => return Err(GpuRecoveryJournalError::RevisionExhausted),
        };
        if revision.get() != expected {
            return Err(GpuRecoveryJournalError::NonConsecutiveRevision {
                latest: self.latest_revision,
                requested: revision,
            });
        }
        if byte_len == 0 {
            return Err(GpuRecoveryJournalError::EmptyCommand);
        }
        if self.entries.len() == self.max_entries {
            return Err(GpuRecoveryJournalError::EntryLimitExhausted {
                resident: self.entries.len(),
                maximum: self.max_entries,
            });
        }
        if byte_len > self.max_bytes {
            return Err(GpuRecoveryJournalError::CommandExceedsByteLimit {
                requested: byte_len,
                maximum: self.max_bytes,
            });
        }
        let combined = self
            .resident_bytes
            .checked_add(byte_len)
            .ok_or(GpuRecoveryJournalError::ByteCountOverflow)?;
        if combined > self.max_bytes {
            return Err(GpuRecoveryJournalError::ByteLimitExhausted {
                resident: self.resident_bytes,
                requested: byte_len,
                maximum: self.max_bytes,
            });
        }
        Ok(())
    }

    pub fn acknowledge_mirrored(
        &mut self,
        revision: DocumentRevision,
    ) -> Result<GpuRecoveryRetirement<C>, GpuRecoveryJournalError> {
        let preview = self.check_acknowledge_mirrored(revision)?;
        let records: Vec<_> = self.entries.drain(..preview.record_count).collect();
        self.resident_bytes = self
            .resident_bytes
            .checked_sub(preview.retired_bytes)
            .expect("the retirement preview counted only retained journal bytes");
        self.mirrored_revision = revision;
        Ok(GpuRecoveryRetirement {
            previous_revision: preview.previous_revision,
            mirrored_revision: preview.mirrored_revision,
            retired_bytes: preview.retired_bytes,
            records,
        })
    }

    pub fn check_acknowledge_mirrored(
        &self,
        revision: DocumentRevision,
    ) -> Result<GpuRecoveryRetirementPreview, GpuRecoveryJournalError> {
        if revision < self.mirrored_revision {
            return Err(GpuRecoveryJournalError::MirrorRevisionRegressed {
                mirrored: self.mirrored_revision,
                requested: revision,
            });
        }
        if revision > self.latest_revision {
            return Err(GpuRecoveryJournalError::MirrorRevisionAhead {
                latest: self.latest_revision,
                requested: revision,
            });
        }
        if revision == self.mirrored_revision {
            return Ok(GpuRecoveryRetirementPreview {
                previous_revision: revision,
                mirrored_revision: revision,
                retired_bytes: 0,
                record_count: 0,
            });
        }
        if !self.entries.iter().any(|entry| entry.revision == revision) {
            return Err(GpuRecoveryJournalError::UnknownMirrorRevision(revision));
        }

        let previous_revision = self.mirrored_revision;
        let retire_count = self
            .entries
            .iter()
            .take_while(|entry| entry.revision <= revision)
            .count();
        let retired_bytes = self
            .entries
            .iter()
            .take(retire_count)
            .try_fold(0_u64, |total, record| total.checked_add(record.byte_len()))
            .ok_or(GpuRecoveryJournalError::ByteCountOverflow)?;
        Ok(GpuRecoveryRetirementPreview {
            previous_revision,
            mirrored_revision: revision,
            retired_bytes,
            record_count: retire_count,
        })
    }

    pub fn snapshot(&self) -> GpuRecoveryJournalSnapshot<C> {
        GpuRecoveryJournalSnapshot {
            base_revision: self.mirrored_revision,
            target_revision: self.latest_revision,
            records: self.entries.iter().cloned().collect(),
            byte_len: self.resident_bytes,
        }
    }

    pub fn records(&self) -> impl Iterator<Item = &GpuRecoveryRecord<C>> {
        self.entries.iter()
    }

    pub const fn mirrored_revision(&self) -> DocumentRevision {
        self.mirrored_revision
    }

    pub const fn latest_revision(&self) -> DocumentRevision {
        self.latest_revision
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn is_current(&self) -> bool {
        self.mirrored_revision == self.latest_revision
    }

    pub const fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub const fn max_entries(&self) -> usize {
        self.max_entries
    }
}

impl<C> Default for GpuRecoveryJournal<C> {
    fn default() -> Self {
        Self::new(
            DocumentRevision::INITIAL,
            DEFAULT_GPU_RECOVERY_JOURNAL_ENTRIES,
            DEFAULT_GPU_RECOVERY_JOURNAL_BYTES,
        )
        .expect("the default GPU recovery journal limits are nonzero")
    }
}

pub struct GpuRecoveryRecord<C> {
    revision: DocumentRevision,
    byte_len: u64,
    command: Arc<C>,
}

impl<C> Clone for GpuRecoveryRecord<C> {
    fn clone(&self) -> Self {
        Self {
            revision: self.revision,
            byte_len: self.byte_len,
            command: Arc::clone(&self.command),
        }
    }
}

impl<C> GpuRecoveryRecord<C> {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub fn command(&self) -> &C {
        &self.command
    }

    pub fn shared_command(&self) -> Arc<C> {
        Arc::clone(&self.command)
    }
}

pub struct GpuRecoveryJournalSnapshot<C> {
    base_revision: DocumentRevision,
    target_revision: DocumentRevision,
    records: Vec<GpuRecoveryRecord<C>>,
    byte_len: u64,
}

impl<C> Clone for GpuRecoveryJournalSnapshot<C> {
    fn clone(&self) -> Self {
        Self {
            base_revision: self.base_revision,
            target_revision: self.target_revision,
            records: self.records.clone(),
            byte_len: self.byte_len,
        }
    }
}

impl<C> GpuRecoveryJournalSnapshot<C> {
    pub const fn base_revision(&self) -> DocumentRevision {
        self.base_revision
    }

    pub const fn target_revision(&self) -> DocumentRevision {
        self.target_revision
    }

    pub fn records(&self) -> &[GpuRecoveryRecord<C>] {
        &self.records
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub fn is_current(&self) -> bool {
        self.base_revision == self.target_revision
    }
}

pub struct GpuRecoveryRetirement<C> {
    pub previous_revision: DocumentRevision,
    pub mirrored_revision: DocumentRevision,
    pub retired_bytes: u64,
    pub records: Vec<GpuRecoveryRecord<C>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuRecoveryRetirementPreview {
    pub previous_revision: DocumentRevision,
    pub mirrored_revision: DocumentRevision,
    pub retired_bytes: u64,
    pub record_count: usize,
}

pub struct GpuRecoveryRecordFailure<C> {
    pub error: GpuRecoveryJournalError,
    pub command: C,
}

impl<C> fmt::Debug for GpuRecoveryRecordFailure<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuRecoveryRecordFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<C> fmt::Display for GpuRecoveryRecordFailure<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl<C: 'static> Error for GpuRecoveryRecordFailure<C> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuRecoveryJournalError {
    InvalidEntryLimit,
    InvalidByteLimit,
    RevisionExhausted,
    NonConsecutiveRevision {
        latest: DocumentRevision,
        requested: DocumentRevision,
    },
    EmptyCommand,
    EntryLimitExhausted {
        resident: usize,
        maximum: usize,
    },
    CommandExceedsByteLimit {
        requested: u64,
        maximum: u64,
    },
    ByteLimitExhausted {
        resident: u64,
        requested: u64,
        maximum: u64,
    },
    ByteCountOverflow,
    MirrorRevisionRegressed {
        mirrored: DocumentRevision,
        requested: DocumentRevision,
    },
    MirrorRevisionAhead {
        latest: DocumentRevision,
        requested: DocumentRevision,
    },
    UnknownMirrorRevision(DocumentRevision),
}

impl fmt::Display for GpuRecoveryJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEntryLimit => write!(formatter, "GPU recovery journal entry limit is zero"),
            Self::InvalidByteLimit => write!(formatter, "GPU recovery journal byte limit is zero"),
            Self::RevisionExhausted => write!(formatter, "GPU recovery journal revision exhausted"),
            Self::NonConsecutiveRevision { latest, requested } => write!(
                formatter,
                "GPU recovery revision {} does not immediately follow {}",
                requested.get(),
                latest.get()
            ),
            Self::EmptyCommand => write!(formatter, "GPU recovery command has zero declared bytes"),
            Self::EntryLimitExhausted { resident, maximum } => write!(
                formatter,
                "GPU recovery journal has {resident} entries and reached its {maximum}-entry limit"
            ),
            Self::CommandExceedsByteLimit { requested, maximum } => write!(
                formatter,
                "GPU recovery command needs {requested} bytes but the journal limit is {maximum}"
            ),
            Self::ByteLimitExhausted {
                resident,
                requested,
                maximum,
            } => write!(
                formatter,
                "GPU recovery journal has {resident} bytes and cannot reserve {requested} within {maximum}"
            ),
            Self::ByteCountOverflow => write!(formatter, "GPU recovery journal bytes overflow"),
            Self::MirrorRevisionRegressed {
                mirrored,
                requested,
            } => write!(
                formatter,
                "GPU recovery mirror revision {} cannot regress from {}",
                requested.get(),
                mirrored.get()
            ),
            Self::MirrorRevisionAhead { latest, requested } => write!(
                formatter,
                "GPU recovery mirror revision {} is newer than journal revision {}",
                requested.get(),
                latest.get()
            ),
            Self::UnknownMirrorRevision(revision) => write!(
                formatter,
                "GPU recovery mirror revision {} is not a journal boundary",
                revision.get()
            ),
        }
    }
}

impl Error for GpuRecoveryJournalError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(value: u64) -> DocumentRevision {
        DocumentRevision::from_raw(value)
    }

    #[test]
    fn snapshots_keep_a_complete_replay_while_live_entries_retire() {
        let mut journal = GpuRecoveryJournal::new(DocumentRevision::INITIAL, 4, 64).unwrap();
        journal.record(revision(1), 8, 3_i32).unwrap();
        journal.record(revision(2), 12, -1_i32).unwrap();
        let snapshot = journal.snapshot();

        let preview = journal.check_acknowledge_mirrored(revision(1)).unwrap();
        assert_eq!(preview.previous_revision, DocumentRevision::INITIAL);
        assert_eq!(preview.mirrored_revision, revision(1));
        assert_eq!(preview.retired_bytes, 8);
        assert_eq!(preview.record_count, 1);
        assert_eq!(journal.mirrored_revision(), DocumentRevision::INITIAL);
        assert_eq!(journal.len(), 2);
        assert_eq!(journal.resident_bytes(), 20);
        let retired = journal.acknowledge_mirrored(revision(1)).unwrap();
        assert_eq!(retired.previous_revision, DocumentRevision::INITIAL);
        assert_eq!(retired.mirrored_revision, revision(1));
        assert_eq!(retired.retired_bytes, 8);
        assert_eq!(*retired.records[0].command(), 3);
        assert_eq!(journal.len(), 1);
        assert_eq!(journal.resident_bytes(), 12);

        assert_eq!(snapshot.base_revision(), DocumentRevision::INITIAL);
        assert_eq!(snapshot.target_revision(), revision(2));
        assert_eq!(snapshot.byte_len(), 20);
        assert_eq!(
            snapshot
                .records()
                .iter()
                .map(|record| *record.command())
                .sum::<i32>(),
            2
        );
    }

    #[test]
    fn capacity_failure_returns_the_exact_command_without_mutation() {
        let mut journal = GpuRecoveryJournal::new(DocumentRevision::INITIAL, 2, 10).unwrap();
        journal
            .record(revision(1), 6, String::from("first"))
            .unwrap();
        assert!(matches!(
            journal.check_record(revision(2), 5),
            Err(GpuRecoveryJournalError::ByteLimitExhausted { .. })
        ));
        let failure = journal
            .record(revision(2), 5, String::from("second"))
            .unwrap_err();
        assert!(matches!(
            failure.error,
            GpuRecoveryJournalError::ByteLimitExhausted {
                resident: 6,
                requested: 5,
                maximum: 10,
            }
        ));
        assert_eq!(failure.command, "second");
        assert_eq!(journal.latest_revision(), revision(1));
        assert_eq!(journal.len(), 1);
        assert_eq!(journal.resident_bytes(), 6);
    }

    #[test]
    fn every_interactive_revision_requires_one_journal_boundary() {
        let mut journal = GpuRecoveryJournal::new(DocumentRevision::INITIAL, 4, 64).unwrap();
        let gap = journal.record(revision(2), 4, 2_u8).unwrap_err();
        assert!(matches!(
            gap.error,
            GpuRecoveryJournalError::NonConsecutiveRevision { .. }
        ));
        assert_eq!(gap.command, 2);
        journal.record(revision(1), 4, 1_u8).unwrap();
        journal.record(revision(2), 4, 2_u8).unwrap();

        assert!(matches!(
            journal.acknowledge_mirrored(revision(3)),
            Err(GpuRecoveryJournalError::MirrorRevisionAhead { .. })
        ));
        assert!(matches!(
            journal.acknowledge_mirrored(DocumentRevision::INITIAL),
            Ok(GpuRecoveryRetirement {
                retired_bytes: 0,
                ..
            })
        ));
        journal.acknowledge_mirrored(revision(1)).unwrap();
        assert!(matches!(
            journal.acknowledge_mirrored(DocumentRevision::INITIAL),
            Err(GpuRecoveryJournalError::MirrorRevisionRegressed { .. })
        ));
    }

    #[test]
    fn replay_snapshot_reconstructs_the_declared_target_revision() {
        #[derive(Clone, Copy)]
        enum Command {
            Add(i32),
            Multiply(i32),
        }

        let mut journal = GpuRecoveryJournal::new(DocumentRevision::INITIAL, 4, 64).unwrap();
        journal.record(revision(1), 4, Command::Add(5)).unwrap();
        journal
            .record(revision(2), 4, Command::Multiply(3))
            .unwrap();
        let snapshot = journal.snapshot();
        let reconstructed =
            snapshot
                .records()
                .iter()
                .fold(2, |state, record| match *record.command() {
                    Command::Add(value) => state + value,
                    Command::Multiply(value) => state * value,
                });
        assert_eq!(reconstructed, 21);
        assert_eq!(snapshot.target_revision(), revision(2));
    }
}
