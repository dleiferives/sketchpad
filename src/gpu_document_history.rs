use crate::{
    gpu_atlas::{AtlasError, AtlasSlot, LayerTileKey, SparseAtlasPlanner},
    gpu_document_undo::GpuDocumentMemento,
};
use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt,
};

pub const DEFAULT_GPU_HISTORY_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_GPU_HISTORY_ENTRIES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpuHistoryId(u64);

impl GpuHistoryId {
    #[cfg(test)]
    pub(crate) const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuHistoryDirection {
    Undo,
    Redo,
}

pub struct GpuHistoryEntry {
    inner: WeightedEntry<GpuDocumentMemento>,
}

impl GpuHistoryEntry {
    pub const fn id(&self) -> GpuHistoryId {
        self.inner.id
    }

    pub const fn byte_len(&self) -> u64 {
        self.inner.byte_len
    }

    pub const fn memento(&self) -> &GpuDocumentMemento {
        &self.inner.value
    }

    pub fn into_memento(self) -> GpuDocumentMemento {
        self.inner.value
    }
}

pub struct GpuHistoryRecord {
    pub id: GpuHistoryId,
    pub evicted: Vec<GpuHistoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuHistoryRecordPreview {
    id: GpuHistoryId,
    evicted_ids: Box<[GpuHistoryId]>,
}

impl GpuHistoryRecordPreview {
    pub const fn id(&self) -> GpuHistoryId {
        self.id
    }

    pub fn evicted_ids(&self) -> &[GpuHistoryId] {
        &self.evicted_ids
    }

    pub fn matches_record(&self, record: &GpuHistoryRecord) -> bool {
        self.id == record.id
            && self
                .evicted_ids
                .iter()
                .copied()
                .eq(record.evicted.iter().map(GpuHistoryEntry::id))
    }
}

pub struct GpuHistoryRecordFailure {
    pub error: GpuHistoryRecordError,
    pub memento: GpuDocumentMemento,
}

pub struct GpuDocumentHistory {
    core: BoundedHistory<GpuDocumentMemento>,
}

impl GpuDocumentHistory {
    pub fn new(max_entries: usize, max_bytes: u64) -> Result<Self, GpuDocumentHistoryError> {
        Ok(Self {
            core: BoundedHistory::new(max_entries, max_bytes)?,
        })
    }

    pub fn record(
        &mut self,
        atlas: &mut SparseAtlasPlanner,
        memento: GpuDocumentMemento,
    ) -> Result<GpuHistoryRecord, Box<GpuHistoryRecordFailure>> {
        let preview = match self.check_record(atlas, &memento) {
            Ok(preview) => preview,
            Err(error) => {
                return Err(Box::new(GpuHistoryRecordFailure { error, memento }));
            }
        };
        let byte_len = memento.byte_len();
        let residents = memento_residents(&memento);
        let mut pinned = Vec::with_capacity(residents.len());
        for &(key, slot) in &residents {
            if let Err(error) = atlas.pin(key, slot) {
                for &(pinned_key, _) in pinned.iter().rev() {
                    atlas
                        .unpin(pinned_key)
                        .expect("record rollback owns every pin it removes");
                }
                return Err(Box::new(GpuHistoryRecordFailure {
                    error: GpuHistoryRecordError::Atlas(error),
                    memento,
                }));
            }
            pinned.push((key, slot));
        }
        match self.core.record(memento, byte_len) {
            Ok(record) => {
                debug_assert_eq!(record.id, preview.id);
                debug_assert_eq!(
                    record
                        .evicted
                        .iter()
                        .map(|entry| entry.id)
                        .collect::<Vec<_>>(),
                    preview.evicted_ids.as_ref()
                );
                for entry in &record.evicted {
                    unpin_memento(atlas, &entry.value);
                }
                Ok(GpuHistoryRecord {
                    id: record.id,
                    evicted: record
                        .evicted
                        .into_iter()
                        .map(|inner| GpuHistoryEntry { inner })
                        .collect(),
                })
            }
            Err(failure) => {
                for &(key, _) in pinned.iter().rev() {
                    atlas
                        .unpin(key)
                        .expect("failed record rollback owns every pin it removes");
                }
                Err(Box::new(GpuHistoryRecordFailure {
                    error: GpuHistoryRecordError::History(failure.error),
                    memento: failure.value,
                }))
            }
        }
    }

    pub fn check_record(
        &self,
        atlas: &SparseAtlasPlanner,
        memento: &GpuDocumentMemento,
    ) -> Result<GpuHistoryRecordPreview, GpuHistoryRecordError> {
        let preview = self
            .core
            .check_record(memento.byte_len())
            .map_err(GpuHistoryRecordError::History)?;
        for (key, slot) in memento_residents(memento) {
            atlas
                .check_pin(key, slot)
                .map_err(GpuHistoryRecordError::Atlas)?;
        }
        Ok(GpuHistoryRecordPreview {
            id: preview.id,
            evicted_ids: preview.evicted_ids.into_boxed_slice(),
        })
    }

    pub fn clear(
        &mut self,
        atlas: &mut SparseAtlasPlanner,
    ) -> Result<Vec<GpuHistoryEntry>, GpuDocumentHistoryError> {
        if self.core.pending.is_some() {
            return Err(GpuDocumentHistoryError::PendingOperation);
        }
        let entries = self.core.drain_all();
        for entry in &entries {
            unpin_memento(atlas, &entry.value);
        }
        Ok(entries
            .into_iter()
            .map(|inner| GpuHistoryEntry { inner })
            .collect())
    }

    pub fn begin_undo(&mut self) -> Result<bool, GpuDocumentHistoryError> {
        self.core.begin(GpuHistoryDirection::Undo)
    }

    pub fn begin_redo(&mut self) -> Result<bool, GpuDocumentHistoryError> {
        self.core.begin(GpuHistoryDirection::Redo)
    }

    pub fn pending_direction(&self) -> Option<GpuHistoryDirection> {
        self.core.pending.as_ref().map(|pending| pending.direction)
    }

    pub fn pending_id(&self) -> Option<GpuHistoryId> {
        self.core.pending_id()
    }

    pub fn pending_memento_mut(
        &mut self,
    ) -> Result<&mut GpuDocumentMemento, GpuDocumentHistoryError> {
        self.core
            .pending
            .as_mut()
            .map(|pending| &mut pending.entry.value)
            .ok_or(GpuDocumentHistoryError::NoPendingOperation)
    }

    pub fn finish_pending(&mut self) -> Result<GpuHistoryId, GpuDocumentHistoryError> {
        self.core.finish_pending()
    }

    pub fn cancel_pending(&mut self) -> Result<GpuHistoryId, GpuDocumentHistoryError> {
        self.core.cancel_pending()
    }

    pub fn undo_depth(&self) -> usize {
        self.core.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.core.redo.len()
    }

    pub const fn resident_bytes(&self) -> u64 {
        self.core.resident_bytes
    }

    pub const fn max_bytes(&self) -> u64 {
        self.core.max_bytes
    }

    pub const fn max_entries(&self) -> usize {
        self.core.max_entries
    }
}

fn memento_residents(memento: &GpuDocumentMemento) -> Vec<(LayerTileKey, AtlasSlot)> {
    let mut seen = HashSet::new();
    memento
        .plan()
        .regions()
        .iter()
        .filter_map(|region| {
            seen.insert((region.key, region.slot))
                .then_some((region.key, region.slot))
        })
        .collect()
}

fn unpin_memento(atlas: &mut SparseAtlasPlanner, memento: &GpuDocumentMemento) {
    for (key, _) in memento_residents(memento) {
        atlas
            .unpin(key)
            .expect("retained GPU history owns every resident pin it removes");
    }
}

impl Default for GpuDocumentHistory {
    fn default() -> Self {
        Self::new(DEFAULT_GPU_HISTORY_ENTRIES, DEFAULT_GPU_HISTORY_BYTES)
            .expect("the default GPU history limits are nonzero")
    }
}

#[derive(Debug)]
struct WeightedEntry<T> {
    id: GpuHistoryId,
    byte_len: u64,
    value: T,
}

#[derive(Debug)]
struct PendingEntry<T> {
    direction: GpuHistoryDirection,
    entry: WeightedEntry<T>,
}

struct BoundedHistory<T> {
    undo: VecDeque<WeightedEntry<T>>,
    redo: VecDeque<WeightedEntry<T>>,
    pending: Option<PendingEntry<T>>,
    resident_bytes: u64,
    max_bytes: u64,
    max_entries: usize,
    next_id: u64,
}

#[derive(Debug)]
struct CoreRecord<T> {
    id: GpuHistoryId,
    evicted: Vec<WeightedEntry<T>>,
}

struct CoreRecordPreview {
    id: GpuHistoryId,
    evicted_ids: Vec<GpuHistoryId>,
    redo_count: usize,
    undo_evict_count: usize,
    resident_bytes: u64,
}

#[derive(Debug)]
struct CoreRecordFailure<T> {
    error: GpuDocumentHistoryError,
    value: T,
}

impl<T> BoundedHistory<T> {
    fn new(max_entries: usize, max_bytes: u64) -> Result<Self, GpuDocumentHistoryError> {
        if max_entries == 0 {
            return Err(GpuDocumentHistoryError::InvalidMaximumEntries);
        }
        if max_bytes == 0 {
            return Err(GpuDocumentHistoryError::InvalidByteBudget);
        }
        Ok(Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            pending: None,
            resident_bytes: 0,
            max_bytes,
            max_entries,
            next_id: 1,
        })
    }

    fn record(&mut self, value: T, byte_len: u64) -> Result<CoreRecord<T>, CoreRecordFailure<T>> {
        let preview = match self.check_record(byte_len) {
            Ok(preview) => preview,
            Err(error) => return Err(CoreRecordFailure { error, value }),
        };
        let id = preview.id;
        self.next_id += 1;
        let mut evicted = Vec::with_capacity(preview.redo_count + preview.undo_evict_count);
        for _ in 0..preview.redo_count {
            evicted.push(
                self.redo
                    .pop_front()
                    .expect("the record preview counted every redo entry"),
            );
        }
        self.undo.push_back(WeightedEntry {
            id,
            byte_len,
            value,
        });
        for _ in 0..preview.undo_evict_count {
            evicted.push(
                self.undo
                    .pop_front()
                    .expect("the record preview counted every undo eviction"),
            );
        }
        self.resident_bytes = preview.resident_bytes;
        debug_assert_eq!(
            evicted.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            preview.evicted_ids
        );
        Ok(CoreRecord { id, evicted })
    }

    fn check_record(&self, byte_len: u64) -> Result<CoreRecordPreview, GpuDocumentHistoryError> {
        if self.pending.is_some() {
            return Err(GpuDocumentHistoryError::PendingOperation);
        }
        if byte_len == 0 {
            return Err(GpuDocumentHistoryError::EmptyMemento);
        }
        if byte_len > self.max_bytes {
            return Err(GpuDocumentHistoryError::MementoExceedsBudget {
                requested: byte_len,
                maximum: self.max_bytes,
            });
        }
        if self.next_id == u64::MAX {
            return Err(GpuDocumentHistoryError::HistoryIdOverflow);
        }

        let redo_bytes = self
            .redo
            .iter()
            .try_fold(0_u64, |total, entry| total.checked_add(entry.byte_len))
            .ok_or(GpuDocumentHistoryError::ByteCountOverflow)?;
        let mut resident_bytes = self
            .resident_bytes
            .checked_sub(redo_bytes)
            .and_then(|retained| retained.checked_add(byte_len))
            .ok_or(GpuDocumentHistoryError::ByteCountOverflow)?;
        let mut undo_len = self
            .undo
            .len()
            .checked_add(1)
            .ok_or(GpuDocumentHistoryError::ByteCountOverflow)?;
        let mut undo_evict_count = 0;
        while undo_len > self.max_entries || resident_bytes > self.max_bytes {
            let entry = self
                .undo
                .get(undo_evict_count)
                .expect("a valid new entry cannot evict itself");
            resident_bytes = resident_bytes
                .checked_sub(entry.byte_len)
                .expect("a retained undo entry owns its accounted bytes");
            undo_evict_count += 1;
            undo_len -= 1;
        }

        let mut evicted_ids = Vec::with_capacity(self.redo.len() + undo_evict_count);
        evicted_ids.extend(self.redo.iter().map(|entry| entry.id));
        evicted_ids.extend(
            self.undo
                .iter()
                .take(undo_evict_count)
                .map(|entry| entry.id),
        );
        Ok(CoreRecordPreview {
            id: GpuHistoryId(self.next_id),
            evicted_ids,
            redo_count: self.redo.len(),
            undo_evict_count,
            resident_bytes,
        })
    }

    fn begin(&mut self, direction: GpuHistoryDirection) -> Result<bool, GpuDocumentHistoryError> {
        if self.pending.is_some() {
            return Err(GpuDocumentHistoryError::PendingOperation);
        }
        let entry = match direction {
            GpuHistoryDirection::Undo => self.undo.pop_back(),
            GpuHistoryDirection::Redo => self.redo.pop_back(),
        };
        let Some(entry) = entry else {
            return Ok(false);
        };
        self.pending = Some(PendingEntry { direction, entry });
        Ok(true)
    }

    fn pending_id(&self) -> Option<GpuHistoryId> {
        self.pending.as_ref().map(|pending| pending.entry.id)
    }

    fn finish_pending(&mut self) -> Result<GpuHistoryId, GpuDocumentHistoryError> {
        let pending = self
            .pending
            .take()
            .ok_or(GpuDocumentHistoryError::NoPendingOperation)?;
        let id = pending.entry.id;
        match pending.direction {
            GpuHistoryDirection::Undo => self.redo.push_back(pending.entry),
            GpuHistoryDirection::Redo => self.undo.push_back(pending.entry),
        }
        Ok(id)
    }

    fn cancel_pending(&mut self) -> Result<GpuHistoryId, GpuDocumentHistoryError> {
        let pending = self
            .pending
            .take()
            .ok_or(GpuDocumentHistoryError::NoPendingOperation)?;
        let id = pending.entry.id;
        match pending.direction {
            GpuHistoryDirection::Undo => self.undo.push_back(pending.entry),
            GpuHistoryDirection::Redo => self.redo.push_back(pending.entry),
        }
        Ok(id)
    }

    fn drain_all(&mut self) -> Vec<WeightedEntry<T>> {
        debug_assert!(self.pending.is_none());
        let mut entries = Vec::with_capacity(self.undo.len() + self.redo.len());
        entries.extend(self.undo.drain(..));
        entries.extend(self.redo.drain(..));
        self.resident_bytes = 0;
        entries
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuHistoryRecordError {
    History(GpuDocumentHistoryError),
    Atlas(AtlasError),
}

impl fmt::Display for GpuHistoryRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::History(error) => error.fmt(formatter),
            Self::Atlas(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuHistoryRecordError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuDocumentHistoryError {
    InvalidMaximumEntries,
    InvalidByteBudget,
    EmptyMemento,
    MementoExceedsBudget { requested: u64, maximum: u64 },
    HistoryIdOverflow,
    ByteCountOverflow,
    PendingOperation,
    NoPendingOperation,
}

impl fmt::Display for GpuDocumentHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMaximumEntries => {
                write!(formatter, "GPU history maximum entry count must be nonzero")
            }
            Self::InvalidByteBudget => write!(formatter, "GPU history byte budget must be nonzero"),
            Self::EmptyMemento => write!(formatter, "GPU history memento is empty"),
            Self::MementoExceedsBudget { requested, maximum } => write!(
                formatter,
                "GPU history memento needs {requested} bytes but the budget is {maximum}"
            ),
            Self::HistoryIdOverflow => write!(formatter, "GPU history ID space is exhausted"),
            Self::ByteCountOverflow => write!(formatter, "GPU history byte count overflows"),
            Self::PendingOperation => write!(formatter, "a GPU history operation is pending"),
            Self::NoPendingOperation => write!(formatter, "no GPU history operation is pending"),
        }
    }
}

impl Error for GpuDocumentHistoryError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(entries: &[WeightedEntry<u32>]) -> Vec<u32> {
        entries.iter().map(|entry| entry.value).collect()
    }

    #[test]
    fn byte_and_entry_budgets_evict_oldest_undo_entries() {
        let mut history = BoundedHistory::new(3, 10).unwrap();
        let first = history.record(1, 4).unwrap().id;
        assert!(history.record(2, 4).unwrap().evicted.is_empty());
        let preview = history.check_record(4).unwrap();
        assert_eq!(preview.id.get(), 3);
        assert_eq!(preview.evicted_ids, vec![first]);
        assert_eq!(history.undo.len(), 2);
        assert_eq!(history.resident_bytes, 8);
        let record = history.record(3, 4).unwrap();
        assert_eq!(values(&record.evicted), vec![1]);
        assert_eq!(values(history.undo.make_contiguous()), vec![2, 3]);
        assert_eq!(history.resident_bytes, 8);

        let record = history.record(4, 1).unwrap();
        assert!(record.evicted.is_empty());
        let record = history.record(5, 1).unwrap();
        assert_eq!(values(&record.evicted), vec![2]);
        assert_eq!(values(history.undo.make_contiguous()), vec![3, 4, 5]);
        assert_eq!(history.resident_bytes, 6);
    }

    #[test]
    fn new_branch_returns_cleared_redo_in_stable_storage_order() {
        let mut history = BoundedHistory::new(8, 100).unwrap();
        history.record(1, 10).unwrap();
        history.record(2, 10).unwrap();
        history.begin(GpuHistoryDirection::Undo).unwrap();
        history.finish_pending().unwrap();
        history.begin(GpuHistoryDirection::Undo).unwrap();
        history.finish_pending().unwrap();
        assert_eq!(values(history.redo.make_contiguous()), vec![2, 1]);

        let preview = history.check_record(5).unwrap();
        assert_eq!(preview.id.get(), 3);
        assert_eq!(
            preview
                .evicted_ids
                .iter()
                .map(|id| id.get())
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert!(history.undo.is_empty());
        assert_eq!(values(history.redo.make_contiguous()), vec![2, 1]);
        let record = history.record(3, 5).unwrap();
        assert_eq!(values(&record.evicted), vec![2, 1]);
        assert_eq!(values(history.undo.make_contiguous()), vec![3]);
        assert!(history.redo.is_empty());
        assert_eq!(history.resident_bytes, 5);
    }

    #[test]
    fn pending_swap_finishes_or_cancels_without_changing_budget() {
        let mut history = BoundedHistory::new(8, 100).unwrap();
        let first = history.record(7, 12).unwrap().id;
        assert!(history.begin(GpuHistoryDirection::Undo).unwrap());
        assert_eq!(history.pending_id(), Some(first));
        assert_eq!(history.pending.as_ref().unwrap().entry.id, first);
        assert_eq!(history.resident_bytes, 12);
        assert_eq!(history.cancel_pending().unwrap(), first);
        assert_eq!(values(history.undo.make_contiguous()), vec![7]);

        history.begin(GpuHistoryDirection::Undo).unwrap();
        assert_eq!(history.finish_pending().unwrap(), first);
        assert_eq!(values(history.redo.make_contiguous()), vec![7]);
        history.begin(GpuHistoryDirection::Redo).unwrap();
        assert_eq!(history.finish_pending().unwrap(), first);
        assert_eq!(values(history.undo.make_contiguous()), vec![7]);
        assert_eq!(history.resident_bytes, 12);
    }

    #[test]
    fn failed_record_preserves_history_and_returns_the_value() {
        let mut history = BoundedHistory::new(2, 10).unwrap();
        history.record(1, 4).unwrap();
        let failure = history.record(2, 11).unwrap_err();
        assert_eq!(failure.value, 2);
        assert_eq!(
            failure.error,
            GpuDocumentHistoryError::MementoExceedsBudget {
                requested: 11,
                maximum: 10,
            }
        );
        assert_eq!(values(history.undo.make_contiguous()), vec![1]);
        assert_eq!(history.resident_bytes, 4);

        history.begin(GpuHistoryDirection::Undo).unwrap();
        let failure = history.record(3, 1).unwrap_err();
        assert_eq!(failure.value, 3);
        assert_eq!(failure.error, GpuDocumentHistoryError::PendingOperation);
        assert_eq!(history.cancel_pending().unwrap().get(), 1);
    }
}
