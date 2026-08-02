use crate::{
    document::LayerId,
    raster::{RectU32, TileCoord},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fmt,
};

pub const DEFAULT_ATLAS_PAGE_SIZE: u32 = 1_024;
pub const DEFAULT_ATLAS_TILE_SIZE: u32 = 128;
pub const DEFAULT_ATLAS_MAX_PAGES: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasLayout {
    page_size: u32,
    tile_size: u32,
    slots_per_axis: u32,
    slots_per_page: u32,
    max_pages: u32,
    total_capacity: u32,
}

impl AtlasLayout {
    pub fn new(page_size: u32, tile_size: u32, max_pages: u32) -> Result<Self, AtlasError> {
        if page_size == 0 {
            return Err(AtlasError::InvalidPageSize(page_size));
        }
        if tile_size == 0 || !page_size.is_multiple_of(tile_size) {
            return Err(AtlasError::InvalidTileSize {
                page_size,
                tile_size,
            });
        }
        if max_pages == 0 {
            return Err(AtlasError::InvalidMaximumPages(max_pages));
        }
        let slots_per_axis = page_size / tile_size;
        let slots_per_page = slots_per_axis
            .checked_mul(slots_per_axis)
            .ok_or(AtlasError::CapacityOverflow)?;
        let total_capacity = slots_per_page
            .checked_mul(max_pages)
            .ok_or(AtlasError::CapacityOverflow)?;
        Ok(Self {
            page_size,
            tile_size,
            slots_per_axis,
            slots_per_page,
            max_pages,
            total_capacity,
        })
    }

    pub fn document_default() -> Self {
        Self::new(
            DEFAULT_ATLAS_PAGE_SIZE,
            DEFAULT_ATLAS_TILE_SIZE,
            DEFAULT_ATLAS_MAX_PAGES,
        )
        .expect("the document atlas constants are valid")
    }

    pub const fn page_size(self) -> u32 {
        self.page_size
    }

    pub const fn tile_size(self) -> u32 {
        self.tile_size
    }

    pub const fn slots_per_axis(self) -> u32 {
        self.slots_per_axis
    }

    pub const fn slots_per_page(self) -> u32 {
        self.slots_per_page
    }

    pub const fn max_pages(self) -> u32 {
        self.max_pages
    }

    pub const fn total_capacity(self) -> u32 {
        self.total_capacity
    }

    pub const fn page_texels(self) -> u64 {
        self.page_size as u64 * self.page_size as u64
    }

    pub const fn page_bytes(self, bytes_per_texel: u32) -> Option<u64> {
        self.page_texels().checked_mul(bytes_per_texel as u64)
    }

    fn slot_origin(self, slot_in_page: u32) -> [u32; 2] {
        debug_assert!(slot_in_page < self.slots_per_page);
        [
            (slot_in_page % self.slots_per_axis) * self.tile_size,
            (slot_in_page / self.slots_per_axis) * self.tile_size,
        ]
    }
}

impl Default for AtlasLayout {
    fn default() -> Self {
        Self::document_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AtlasPageId(u32);

impl AtlasPageId {
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AtlasSlot {
    page: AtlasPageId,
    slot_in_page: u32,
    origin: [u32; 2],
}

impl AtlasSlot {
    pub const fn page(self) -> AtlasPageId {
        self.page
    }

    pub const fn slot_in_page(self) -> u32 {
        self.slot_in_page
    }

    pub const fn origin(self) -> [u32; 2] {
        self.origin
    }

    pub fn rect(self, layout: AtlasLayout) -> RectU32 {
        RectU32::from_xywh(
            self.origin[0],
            self.origin[1],
            layout.tile_size,
            layout.tile_size,
        )
        .expect("a validated atlas slot has a nonempty in-page rectangle")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LayerTileKey {
    pub layer: LayerId,
    pub tile: TileCoord,
}

impl LayerTileKey {
    pub const fn new(layer: LayerId, tile: TileCoord) -> Self {
        Self { layer, tile }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasAllocation {
    pub key: LayerTileKey,
    pub slot: AtlasSlot,
    pub newly_allocated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtlasWork<T> {
    pub key: LayerTileKey,
    pub slot: AtlasSlot,
    pub payload: T,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtlasPageBatch<T> {
    pub page: AtlasPageId,
    pub work: Vec<AtlasWork<T>>,
}

pub struct SparseAtlasPlanner {
    layout: AtlasLayout,
    allocations: HashMap<LayerTileKey, AtlasSlot>,
    pin_counts: HashMap<LayerTileKey, u32>,
    occupants: Vec<Option<LayerTileKey>>,
    free_slots: Vec<u32>,
}

impl SparseAtlasPlanner {
    pub fn new(layout: AtlasLayout) -> Self {
        Self {
            layout,
            allocations: HashMap::new(),
            pin_counts: HashMap::new(),
            occupants: Vec::new(),
            free_slots: Vec::new(),
        }
    }

    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub fn resident_tile_count(&self) -> usize {
        self.allocations.len()
    }

    pub fn retained_page_count(&self) -> usize {
        self.occupants.len() / self.layout.slots_per_page as usize
    }

    pub fn free_slot_count(&self) -> usize {
        self.free_slots.len()
    }

    pub fn pinned_tile_count(&self) -> usize {
        self.pin_counts.len()
    }

    pub fn pin_count(&self, key: LayerTileKey) -> u32 {
        self.pin_counts.get(&key).copied().unwrap_or(0)
    }

    pub fn slot(&self, key: LayerTileKey) -> Option<AtlasSlot> {
        self.allocations.get(&key).copied()
    }

    pub fn allocate(&mut self, key: LayerTileKey) -> Result<AtlasAllocation, AtlasError> {
        if let Some(slot) = self.slot(key) {
            return Ok(AtlasAllocation {
                key,
                slot,
                newly_allocated: false,
            });
        }
        if self.free_slots.is_empty() {
            self.grow_one_page()?;
        }
        let global_slot = self
            .free_slots
            .pop()
            .expect("growing an atlas page must provide a free slot");
        let slot = self.slot_from_global(global_slot);
        assert!(self.occupants[global_slot as usize].replace(key).is_none());
        assert!(self.allocations.insert(key, slot).is_none());
        Ok(AtlasAllocation {
            key,
            slot,
            newly_allocated: true,
        })
    }

    pub fn allocate_batch(
        &mut self,
        keys: impl IntoIterator<Item = LayerTileKey>,
    ) -> Result<Vec<AtlasAllocation>, AtlasError> {
        let keys: Vec<_> = keys.into_iter().collect();
        let mut seen = HashSet::with_capacity(keys.len());
        for key in &keys {
            if !seen.insert(*key) {
                return Err(AtlasError::DuplicateKey(*key));
            }
        }
        let missing = keys
            .iter()
            .filter(|key| !self.allocations.contains_key(key))
            .count();
        let available_without_growth = self.free_slots.len();
        let remaining_pages = self.layout.max_pages as usize - self.retained_page_count();
        let available_with_growth = available_without_growth
            .checked_add(remaining_pages * self.layout.slots_per_page as usize)
            .ok_or(AtlasError::CapacityOverflow)?;
        if missing > available_with_growth {
            return Err(AtlasError::CapacityExhausted {
                requested_new_tiles: missing,
                available_slots: available_with_growth,
            });
        }
        keys.into_iter().map(|key| self.allocate(key)).collect()
    }

    pub fn pin(&mut self, key: LayerTileKey, expected_slot: AtlasSlot) -> Result<(), AtlasError> {
        self.check_pin(key, expected_slot)?;
        let next = self
            .pin_count(key)
            .checked_add(1)
            .expect("the pin count was checked before mutation");
        self.pin_counts.insert(key, next);
        Ok(())
    }

    pub fn check_pin(&self, key: LayerTileKey, expected_slot: AtlasSlot) -> Result<(), AtlasError> {
        let actual = self.slot(key);
        if actual != Some(expected_slot) {
            return Err(AtlasError::ResidentSlotMismatch {
                key,
                expected: expected_slot,
                actual,
            });
        }
        self.pin_count(key)
            .checked_add(1)
            .ok_or(AtlasError::PinCountOverflow(key))?;
        Ok(())
    }

    pub fn unpin(&mut self, key: LayerTileKey) -> Result<(), AtlasError> {
        let count = self
            .pin_counts
            .get_mut(&key)
            .ok_or(AtlasError::KeyNotPinned(key))?;
        *count -= 1;
        if *count == 0 {
            self.pin_counts.remove(&key);
        }
        Ok(())
    }

    pub fn release(&mut self, key: LayerTileKey) -> Result<Option<AtlasSlot>, AtlasError> {
        let pin_count = self.pin_count(key);
        if pin_count != 0 {
            return Err(AtlasError::KeyPinned { key, pin_count });
        }
        let Some(slot) = self.allocations.remove(&key) else {
            return Ok(None);
        };
        let global_slot = self.global_from_slot(slot);
        let occupant = self.occupants[global_slot as usize].take();
        assert_eq!(occupant, Some(key));
        self.free_slots.push(global_slot);
        Ok(Some(slot))
    }

    pub fn release_layer(
        &mut self,
        layer: LayerId,
    ) -> Result<Vec<(LayerTileKey, AtlasSlot)>, AtlasError> {
        let mut keys: Vec<_> = self
            .allocations
            .keys()
            .copied()
            .filter(|key| key.layer == layer)
            .collect();
        keys.sort_by_key(|key| (key.tile.y, key.tile.x));
        if let Some(key) = keys.iter().find(|key| self.pin_count(**key) != 0) {
            return Err(AtlasError::KeyPinned {
                key: *key,
                pin_count: self.pin_count(*key),
            });
        }
        Ok(keys
            .into_iter()
            .map(|key| {
                let slot = self
                    .release(key)
                    .expect("all pinned layer keys were rejected before release")
                    .expect("a key collected from the planner must remain allocated");
                (key, slot)
            })
            .collect())
    }

    pub fn batch_resident_work<T>(
        &self,
        work: impl IntoIterator<Item = (LayerTileKey, T)>,
    ) -> Result<Vec<AtlasPageBatch<T>>, AtlasError> {
        let mut pages: BTreeMap<AtlasPageId, Vec<AtlasWork<T>>> = BTreeMap::new();
        for (key, payload) in work {
            let slot = self.slot(key).ok_or(AtlasError::KeyNotResident(key))?;
            pages
                .entry(slot.page)
                .or_default()
                .push(AtlasWork { key, slot, payload });
        }
        Ok(pages
            .into_iter()
            .map(|(page, work)| AtlasPageBatch { page, work })
            .collect())
    }

    fn grow_one_page(&mut self) -> Result<(), AtlasError> {
        let retained_pages = self.retained_page_count() as u32;
        if retained_pages >= self.layout.max_pages {
            return Err(AtlasError::CapacityExhausted {
                requested_new_tiles: 1,
                available_slots: 0,
            });
        }
        let first_global = self.occupants.len() as u32;
        let end_global = first_global
            .checked_add(self.layout.slots_per_page)
            .ok_or(AtlasError::CapacityOverflow)?;
        self.occupants
            .resize(end_global as usize, Option::<LayerTileKey>::None);
        self.free_slots.extend((first_global..end_global).rev());
        Ok(())
    }

    fn slot_from_global(&self, global_slot: u32) -> AtlasSlot {
        let slot_in_page = global_slot % self.layout.slots_per_page;
        AtlasSlot {
            page: AtlasPageId(global_slot / self.layout.slots_per_page),
            slot_in_page,
            origin: self.layout.slot_origin(slot_in_page),
        }
    }

    fn global_from_slot(&self, slot: AtlasSlot) -> u32 {
        slot.page.0 * self.layout.slots_per_page + slot.slot_in_page
    }
}

impl Default for SparseAtlasPlanner {
    fn default() -> Self {
        Self::new(AtlasLayout::default())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtlasError {
    InvalidPageSize(u32),
    InvalidTileSize {
        page_size: u32,
        tile_size: u32,
    },
    InvalidMaximumPages(u32),
    CapacityOverflow,
    CapacityExhausted {
        requested_new_tiles: usize,
        available_slots: usize,
    },
    DuplicateKey(LayerTileKey),
    KeyNotResident(LayerTileKey),
    ResidentSlotMismatch {
        key: LayerTileKey,
        expected: AtlasSlot,
        actual: Option<AtlasSlot>,
    },
    PinCountOverflow(LayerTileKey),
    KeyNotPinned(LayerTileKey),
    KeyPinned {
        key: LayerTileKey,
        pin_count: u32,
    },
}

impl fmt::Display for AtlasError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageSize(size) => write!(formatter, "invalid atlas page size {size}"),
            Self::InvalidTileSize {
                page_size,
                tile_size,
            } => write!(
                formatter,
                "atlas tile size {tile_size} does not evenly divide page size {page_size}"
            ),
            Self::InvalidMaximumPages(pages) => {
                write!(formatter, "invalid atlas maximum page count {pages}")
            }
            Self::CapacityOverflow => write!(formatter, "atlas capacity overflows its counters"),
            Self::CapacityExhausted {
                requested_new_tiles,
                available_slots,
            } => write!(
                formatter,
                "atlas needs {requested_new_tiles} new tiles but has {available_slots} slots"
            ),
            Self::DuplicateKey(key) => write!(
                formatter,
                "layer {} tile ({}, {}) occurs twice in one allocation batch",
                key.layer.get(),
                key.tile.x,
                key.tile.y
            ),
            Self::KeyNotResident(key) => write!(
                formatter,
                "layer {} tile ({}, {}) is not resident",
                key.layer.get(),
                key.tile.x,
                key.tile.y
            ),
            Self::ResidentSlotMismatch {
                key,
                expected,
                actual,
            } => write!(
                formatter,
                "layer {} tile ({}, {}) expected atlas slot {expected:?}, found {actual:?}",
                key.layer.get(),
                key.tile.x,
                key.tile.y
            ),
            Self::PinCountOverflow(key) => write!(
                formatter,
                "atlas pin count overflows for layer {} tile ({}, {})",
                key.layer.get(),
                key.tile.x,
                key.tile.y
            ),
            Self::KeyNotPinned(key) => write!(
                formatter,
                "layer {} tile ({}, {}) is not pinned",
                key.layer.get(),
                key.tile.x,
                key.tile.y
            ),
            Self::KeyPinned { key, pin_count } => write!(
                formatter,
                "layer {} tile ({}, {}) has {pin_count} history pins",
                key.layer.get(),
                key.tile.x,
                key.tile.y
            ),
        }
    }
}

impl Error for AtlasError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(layer: u64, x: u32, y: u32) -> LayerTileKey {
        LayerTileKey::new(LayerId::from_raw(layer), TileCoord::new(x, y))
    }

    #[test]
    fn document_layout_has_declared_full_precision_page_costs() {
        let layout = AtlasLayout::document_default();
        assert_eq!(layout.slots_per_axis(), 8);
        assert_eq!(layout.slots_per_page(), 64);
        assert_eq!(layout.total_capacity(), 1_024);
        assert_eq!(layout.page_bytes(16), Some(16 * 1024 * 1024));
        assert_eq!(layout.page_bytes(4), Some(4 * 1024 * 1024));
    }

    #[test]
    fn allocation_is_stable_and_maps_row_major_into_a_page() {
        let mut atlas = SparseAtlasPlanner::default();
        let first = atlas.allocate(key(2, 5, 7)).unwrap();
        assert!(first.newly_allocated);
        assert_eq!(first.slot.page().get(), 0);
        assert_eq!(first.slot.slot_in_page(), 0);
        assert_eq!(first.slot.origin(), [0, 0]);
        assert_eq!(first.slot.rect(atlas.layout()).max_x(), 128);

        let same = atlas.allocate(first.key).unwrap();
        assert!(!same.newly_allocated);
        assert_eq!(same.slot, first.slot);

        for index in 1..9 {
            atlas.allocate(key(2, index, 0)).unwrap();
        }
        let ninth = atlas.slot(key(2, 8, 0)).unwrap();
        assert_eq!(ninth.slot_in_page(), 8);
        assert_eq!(ninth.origin(), [0, 128]);
    }

    #[test]
    fn many_logical_tiles_collapse_to_one_page_batch() {
        let mut atlas = SparseAtlasPlanner::default();
        let keys: Vec<_> = (0..48).map(|x| key(1, x, 0)).collect();
        atlas.allocate_batch(keys.iter().copied()).unwrap();
        let batches = atlas
            .batch_resident_work(keys.iter().copied().map(|key| (key, ())))
            .unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].work.len(), 48);
    }

    #[test]
    fn pass_count_tracks_touched_pages_not_touched_tiles() {
        let layout = AtlasLayout::new(32, 16, 3).unwrap();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let keys: Vec<_> = (0..10).map(|x| key(1, x, 0)).collect();
        atlas.allocate_batch(keys.iter().copied()).unwrap();

        let touched = [keys[0], keys[3], keys[4], keys[7], keys[8], keys[9]];
        let batches = atlas
            .batch_resident_work(touched.into_iter().map(|key| (key, key.tile.x)))
            .unwrap();
        assert_eq!(batches.len(), 3);
        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.work.len())
                .collect::<Vec<_>>(),
            vec![2, 2, 2]
        );
    }

    #[test]
    fn release_reuses_a_slot_but_requires_the_caller_to_clear_it() {
        let layout = AtlasLayout::new(32, 16, 1).unwrap();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let first = atlas.allocate(key(1, 0, 0)).unwrap();
        assert_eq!(atlas.release(first.key).unwrap(), Some(first.slot));
        let replacement = atlas.allocate(key(2, 0, 0)).unwrap();
        assert_eq!(replacement.slot, first.slot);
        assert!(replacement.newly_allocated);
    }

    #[test]
    fn batch_capacity_failure_is_transactional() {
        let layout = AtlasLayout::new(16, 16, 1).unwrap();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let result = atlas.allocate_batch([key(1, 0, 0), key(1, 1, 0)]);
        assert_eq!(
            result,
            Err(AtlasError::CapacityExhausted {
                requested_new_tiles: 2,
                available_slots: 1,
            })
        );
        assert_eq!(atlas.resident_tile_count(), 0);
        assert_eq!(atlas.retained_page_count(), 0);
    }

    #[test]
    fn duplicate_batch_keys_and_missing_work_are_explicit_errors() {
        let mut atlas = SparseAtlasPlanner::default();
        let repeated = key(4, 2, 3);
        assert_eq!(
            atlas.allocate_batch([repeated, repeated]),
            Err(AtlasError::DuplicateKey(repeated))
        );
        assert_eq!(
            atlas.batch_resident_work([(repeated, ())]),
            Err(AtlasError::KeyNotResident(repeated))
        );
    }

    #[test]
    fn releasing_a_layer_is_deterministic_and_keeps_pages_retained() {
        let layout = AtlasLayout::new(32, 16, 2).unwrap();
        let mut atlas = SparseAtlasPlanner::new(layout);
        atlas
            .allocate_batch([key(7, 2, 1), key(8, 0, 0), key(7, 1, 1), key(7, 0, 2)])
            .unwrap();
        let released = atlas.release_layer(LayerId::from_raw(7)).unwrap();
        assert_eq!(
            released.iter().map(|(key, _)| key.tile).collect::<Vec<_>>(),
            vec![
                TileCoord::new(1, 1),
                TileCoord::new(2, 1),
                TileCoord::new(0, 2),
            ]
        );
        assert_eq!(atlas.resident_tile_count(), 1);
        assert_eq!(atlas.retained_page_count(), 1);
    }

    #[test]
    fn history_pins_prevent_slot_reuse_until_the_last_unpin() {
        let layout = AtlasLayout::new(32, 16, 1).unwrap();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let allocation = atlas.allocate(key(9, 0, 0)).unwrap();
        atlas.check_pin(allocation.key, allocation.slot).unwrap();
        assert_eq!(atlas.pin_count(allocation.key), 0);
        let missing = key(9, 1, 0);
        assert_eq!(
            atlas.check_pin(missing, allocation.slot),
            Err(AtlasError::ResidentSlotMismatch {
                key: missing,
                expected: allocation.slot,
                actual: None,
            })
        );
        assert_eq!(atlas.pinned_tile_count(), 0);
        atlas.pin(allocation.key, allocation.slot).unwrap();
        atlas.pin(allocation.key, allocation.slot).unwrap();
        assert_eq!(atlas.pin_count(allocation.key), 2);
        assert_eq!(
            atlas.release(allocation.key),
            Err(AtlasError::KeyPinned {
                key: allocation.key,
                pin_count: 2,
            })
        );
        atlas.unpin(allocation.key).unwrap();
        assert_eq!(atlas.pin_count(allocation.key), 1);
        atlas.unpin(allocation.key).unwrap();
        assert_eq!(atlas.pinned_tile_count(), 0);
        assert_eq!(
            atlas.release(allocation.key).unwrap(),
            Some(allocation.slot)
        );
    }

    #[test]
    fn a_pinned_layer_release_fails_without_releasing_siblings() {
        let layout = AtlasLayout::new(32, 16, 1).unwrap();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let first = atlas.allocate(key(10, 0, 0)).unwrap();
        let second = atlas.allocate(key(10, 1, 0)).unwrap();
        atlas.pin(second.key, second.slot).unwrap();
        assert_eq!(
            atlas.release_layer(LayerId::from_raw(10)),
            Err(AtlasError::KeyPinned {
                key: second.key,
                pin_count: 1,
            })
        );
        assert_eq!(atlas.slot(first.key), Some(first.slot));
        assert_eq!(atlas.slot(second.key), Some(second.slot));
        assert_eq!(atlas.resident_tile_count(), 2);
    }
}
