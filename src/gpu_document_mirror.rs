use crate::{
    document::DocumentRevision,
    gpu_atlas::{AtlasLayout, AtlasSlot, LayerTileKey},
    gpu_document_undo::{
        GpuDocumentMemento, GpuMementoResidentState, GpuUndoCapturePlan, GPU_UNDO_BLOCK_BYTES,
        GPU_UNDO_BLOCK_SIZE, GPU_UNDO_PIXEL_BYTES,
    },
    raster::RectU32,
};
use std::{error::Error, fmt};

pub const DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuMirrorCopyRegion {
    pub key: LayerTileKey,
    pub slot: AtlasSlot,
    pub local_bounds: RectU32,
    pub physical_origin: [u32; 2],
    pub extent: [u32; 2],
    pub initialized: bool,
    pub buffer_offset: Option<u64>,
    pub bytes_per_row: u32,
    pub block_count: u32,
}

impl GpuMirrorCopyRegion {
    pub const fn byte_len(self) -> u64 {
        if self.initialized {
            self.bytes_per_row as u64 * self.extent[1] as u64
        } else {
            0
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuMirrorBatchPlan {
    revision: DocumentRevision,
    index: u32,
    regions: Vec<GpuMirrorCopyRegion>,
    byte_len: u64,
    block_count: u32,
}

impl GpuMirrorBatchPlan {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn index(&self) -> u32 {
        self.index
    }

    pub fn regions(&self) -> &[GpuMirrorCopyRegion] {
        &self.regions
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub const fn block_count(&self) -> u32 {
        self.block_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuMirrorReadbackPlan {
    layout: AtlasLayout,
    revision: DocumentRevision,
    batches: Vec<GpuMirrorBatchPlan>,
    byte_len: u64,
    block_count: u32,
}

impl GpuMirrorReadbackPlan {
    pub fn from_memento(
        revision: DocumentRevision,
        memento: &GpuDocumentMemento,
        max_batch_bytes: u64,
    ) -> Result<Self, GpuMirrorPlanError> {
        Self::from_capture(
            revision,
            memento.plan(),
            memento.resident_states(),
            max_batch_bytes,
        )
    }

    pub fn from_capture(
        revision: DocumentRevision,
        capture: &GpuUndoCapturePlan,
        resident_states: &[GpuMementoResidentState],
        max_batch_bytes: u64,
    ) -> Result<Self, GpuMirrorPlanError> {
        if max_batch_bytes < GPU_UNDO_BLOCK_BYTES {
            return Err(GpuMirrorPlanError::BatchBudgetTooSmall {
                requested: max_batch_bytes,
                minimum: GPU_UNDO_BLOCK_BYTES,
            });
        }
        if capture.regions().len() != resident_states.len() {
            return Err(GpuMirrorPlanError::ResidentStateCountMismatch {
                regions: capture.regions().len(),
                states: resident_states.len(),
            });
        }

        let mut pieces = Vec::new();
        for (region, state) in capture.regions().iter().zip(resident_states) {
            if (region.key, region.slot) != (state.key, state.slot) {
                return Err(GpuMirrorPlanError::ResidentIdentityMismatch {
                    region_key: region.key,
                    region_slot: region.slot,
                    state_key: state.key,
                    state_slot: state.slot,
                });
            }
            append_region_pieces(
                region.key,
                region.slot,
                region.local_bounds,
                region.physical_origin,
                state.document_initialized,
                max_batch_bytes,
                &mut pieces,
            )?;
        }

        let mut batches = Vec::<GpuMirrorBatchPlan>::new();
        for mut piece in pieces {
            let piece_bytes = piece.byte_len();
            let needs_new_batch = batches.last().is_some_and(|batch| {
                batch.byte_len != 0
                    && batch
                        .byte_len
                        .checked_add(piece_bytes)
                        .is_none_or(|combined| combined > max_batch_bytes)
            });
            if batches.is_empty() || needs_new_batch {
                batches.push(GpuMirrorBatchPlan {
                    revision,
                    index: checked_u32(batches.len())?,
                    regions: Vec::new(),
                    byte_len: 0,
                    block_count: 0,
                });
            }
            let batch = batches
                .last_mut()
                .expect("a mirror batch is created before appending each piece");
            if piece.initialized {
                piece.buffer_offset = Some(batch.byte_len);
                batch.byte_len = batch
                    .byte_len
                    .checked_add(piece_bytes)
                    .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
            }
            batch.block_count = batch
                .block_count
                .checked_add(piece.block_count)
                .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
            batch.regions.push(piece);
        }

        let byte_len = batches
            .iter()
            .try_fold(0_u64, |total, batch| total.checked_add(batch.byte_len));
        let block_count = batches
            .iter()
            .try_fold(0_u32, |total, batch| total.checked_add(batch.block_count));
        Ok(Self {
            layout: capture.layout(),
            revision,
            batches,
            byte_len: byte_len.ok_or(GpuMirrorPlanError::ArithmeticOverflow)?,
            block_count: block_count.ok_or(GpuMirrorPlanError::ArithmeticOverflow)?,
        })
    }

    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub fn batches(&self) -> &[GpuMirrorBatchPlan] {
        &self.batches
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub const fn block_count(&self) -> u32 {
        self.block_count
    }
}

fn append_region_pieces(
    key: LayerTileKey,
    slot: AtlasSlot,
    local_bounds: RectU32,
    physical_origin: [u32; 2],
    initialized: bool,
    max_batch_bytes: u64,
    pieces: &mut Vec<GpuMirrorCopyRegion>,
) -> Result<(), GpuMirrorPlanError> {
    let width = local_bounds.width();
    let bytes_per_row = width
        .checked_mul(GPU_UNDO_PIXEL_BYTES)
        .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
    if !initialized {
        pieces.push(GpuMirrorCopyRegion {
            key,
            slot,
            local_bounds,
            physical_origin,
            extent: [width, local_bounds.height()],
            initialized: false,
            buffer_offset: None,
            bytes_per_row,
            block_count: 0,
        });
        return Ok(());
    }

    let maximum_rows = u32::try_from(max_batch_bytes / u64::from(bytes_per_row))
        .unwrap_or(u32::MAX)
        / GPU_UNDO_BLOCK_SIZE
        * GPU_UNDO_BLOCK_SIZE;
    if maximum_rows < GPU_UNDO_BLOCK_SIZE {
        return Err(GpuMirrorPlanError::RegionRowExceedsBatch {
            bytes_per_block_row: u64::from(bytes_per_row) * u64::from(GPU_UNDO_BLOCK_SIZE),
            maximum: max_batch_bytes,
        });
    }
    let mut local_y = local_bounds.min_y();
    let mut physical_y = physical_origin[1];
    while local_y < local_bounds.max_y() {
        let height = maximum_rows.min(local_bounds.max_y() - local_y);
        let piece_bounds = RectU32::from_min_max(
            local_bounds.min_x(),
            local_y,
            local_bounds.max_x(),
            local_y + height,
        )
        .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
        let block_count = (width / GPU_UNDO_BLOCK_SIZE)
            .checked_mul(height / GPU_UNDO_BLOCK_SIZE)
            .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
        pieces.push(GpuMirrorCopyRegion {
            key,
            slot,
            local_bounds: piece_bounds,
            physical_origin: [physical_origin[0], physical_y],
            extent: [width, height],
            initialized: true,
            buffer_offset: None,
            bytes_per_row,
            block_count,
        });
        local_y = local_y
            .checked_add(height)
            .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
        physical_y = physical_y
            .checked_add(height)
            .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
    }
    Ok(())
}

fn checked_u32(value: usize) -> Result<u32, GpuMirrorPlanError> {
    u32::try_from(value).map_err(|_| GpuMirrorPlanError::BatchCountOverflow)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMirrorPlanError {
    BatchBudgetTooSmall {
        requested: u64,
        minimum: u64,
    },
    ResidentStateCountMismatch {
        regions: usize,
        states: usize,
    },
    ResidentIdentityMismatch {
        region_key: LayerTileKey,
        region_slot: AtlasSlot,
        state_key: LayerTileKey,
        state_slot: AtlasSlot,
    },
    RegionRowExceedsBatch {
        bytes_per_block_row: u64,
        maximum: u64,
    },
    BatchCountOverflow,
    ArithmeticOverflow,
}

impl fmt::Display for GpuMirrorPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BatchBudgetTooSmall { requested, minimum } => write!(
                formatter,
                "GPU mirror batch budget {requested} is smaller than one {minimum}-byte block"
            ),
            Self::ResidentStateCountMismatch { regions, states } => write!(
                formatter,
                "GPU mirror has {regions} copy regions but {states} resident states"
            ),
            Self::ResidentIdentityMismatch {
                region_key,
                region_slot,
                state_key,
                state_slot,
            } => write!(
                formatter,
                "GPU mirror region ({region_key:?}, {region_slot:?}) does not match resident state ({state_key:?}, {state_slot:?})"
            ),
            Self::RegionRowExceedsBatch {
                bytes_per_block_row,
                maximum,
            } => write!(
                formatter,
                "one GPU mirror block row needs {bytes_per_block_row} bytes but the batch limit is {maximum}"
            ),
            Self::BatchCountOverflow => write!(formatter, "GPU mirror batch count overflows"),
            Self::ArithmeticOverflow => write!(formatter, "GPU mirror plan size overflows"),
        }
    }
}

impl Error for GpuMirrorPlanError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::LayerId,
        gpu_atlas::{LayerTileKey, SparseAtlasPlanner},
        gpu_round_target::ActiveRoundMaskTile,
        raster::TileCoord,
    };

    fn capture_and_states(
        layout: AtlasLayout,
        damages: &[(TileCoord, RectU32, bool)],
    ) -> (GpuUndoCapturePlan, Vec<GpuMementoResidentState>) {
        let layer = LayerId::from_raw(5);
        let mut atlas = SparseAtlasPlanner::new(layout);
        let mut active = Vec::new();
        let mut initialized = Vec::new();
        for &(tile, damage, present) in damages {
            let key = LayerTileKey::new(layer, tile);
            let slot = atlas.allocate(key).unwrap().slot;
            active.push(ActiveRoundMaskTile {
                key,
                slot,
                local_damage: damage,
            });
            initialized.push((key, slot, present));
        }
        let capture = GpuUndoCapturePlan::from_active_tiles(layout, &active).unwrap();
        initialized.sort_by_key(|(key, slot, _)| {
            (
                slot.page().get(),
                slot.slot_in_page(),
                key.tile.y,
                key.tile.x,
            )
        });
        let states = initialized
            .into_iter()
            .map(
                |(key, slot, document_initialized)| GpuMementoResidentState {
                    key,
                    slot,
                    document_initialized,
                    memento_initialized: !document_initialized,
                },
            )
            .collect();
        (capture, states)
    }

    #[test]
    fn default_tile_regions_pack_to_the_declared_byte_cap() {
        let layout = AtlasLayout::document_default();
        let damage = RectU32::from_xywh(0, 0, 128, 128).unwrap();
        let damages: Vec<_> = (0..3)
            .map(|x| (TileCoord::new(x, 0), damage, true))
            .collect();
        let (capture, states) = capture_and_states(layout, &damages);
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::INITIAL,
            &capture,
            &states,
            2 * 128 * 128 * 16,
        )
        .unwrap();
        assert_eq!(plan.batches().len(), 2);
        assert_eq!(plan.batches()[0].regions().len(), 2);
        assert_eq!(plan.batches()[0].byte_len(), 524_288);
        assert_eq!(plan.batches()[1].regions().len(), 1);
        assert_eq!(plan.byte_len(), 786_432);
        assert_eq!(plan.block_count(), 192);
    }

    #[test]
    fn oversized_region_splits_only_on_block_rows() {
        let layout = AtlasLayout::new(2_048, 2_048, 1).unwrap();
        let damage = RectU32::from_xywh(0, 0, 2_048, 2_048).unwrap();
        let (capture, states) = capture_and_states(layout, &[(TileCoord::new(0, 0), damage, true)]);
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::INITIAL,
            &capture,
            &states,
            DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
        )
        .unwrap();
        assert_eq!(plan.batches().len(), 4);
        assert!(plan
            .batches()
            .iter()
            .all(|batch| batch.byte_len() == DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT));
        assert!(plan.batches().iter().all(|batch| {
            let region = batch.regions()[0];
            region.extent == [2_048, 512]
                && region
                    .local_bounds
                    .min_y()
                    .is_multiple_of(GPU_UNDO_BLOCK_SIZE)
        }));
        assert_eq!(plan.byte_len(), 64 * 1024 * 1024);
    }

    #[test]
    fn absent_resident_is_metadata_only() {
        let layout = AtlasLayout::document_default();
        let damage = RectU32::from_xywh(16, 16, 16, 16).unwrap();
        let (capture, states) =
            capture_and_states(layout, &[(TileCoord::new(0, 0), damage, false)]);
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::INITIAL,
            &capture,
            &states,
            DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
        )
        .unwrap();
        assert_eq!(plan.batches().len(), 1);
        assert_eq!(plan.byte_len(), 0);
        assert_eq!(plan.block_count(), 0);
        assert_eq!(plan.batches()[0].regions()[0].buffer_offset, None);
    }

    #[test]
    fn budget_smaller_than_one_block_fails_before_planning() {
        let layout = AtlasLayout::document_default();
        let (capture, states) = capture_and_states(
            layout,
            &[(
                TileCoord::new(0, 0),
                RectU32::from_xywh(0, 0, 1, 1).unwrap(),
                true,
            )],
        );
        assert_eq!(
            GpuMirrorReadbackPlan::from_capture(
                DocumentRevision::INITIAL,
                &capture,
                &states,
                GPU_UNDO_BLOCK_BYTES - 1,
            )
            .unwrap_err(),
            GpuMirrorPlanError::BatchBudgetTooSmall {
                requested: GPU_UNDO_BLOCK_BYTES - 1,
                minimum: GPU_UNDO_BLOCK_BYTES,
            }
        );
    }
}
