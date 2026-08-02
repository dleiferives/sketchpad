use crate::{
    gpu_atlas::{AtlasLayout, AtlasSlot, LayerTileKey},
    gpu_round_target::{ActiveRoundMaskTile, RoundMaskTarget},
    raster::{LinearRgba, RectU32},
};
use std::{collections::HashMap, error::Error, fmt, mem::size_of};

pub const GPU_UNDO_BLOCK_SIZE: u32 = 16;
pub const GPU_UNDO_PIXEL_BYTES: u32 = size_of::<LinearRgba>() as u32;
pub const GPU_UNDO_BLOCK_BYTES: u64 =
    GPU_UNDO_BLOCK_SIZE as u64 * GPU_UNDO_BLOCK_SIZE as u64 * GPU_UNDO_PIXEL_BYTES as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuUndoCopyRegion {
    pub key: LayerTileKey,
    pub slot: AtlasSlot,
    pub local_bounds: RectU32,
    pub physical_origin: [u32; 2],
    pub extent: [u32; 2],
    pub buffer_offset: u64,
    pub bytes_per_row: u32,
    pub block_count: u32,
}

impl GpuUndoCopyRegion {
    pub const fn byte_len(self) -> u64 {
        self.bytes_per_row as u64 * self.extent[1] as u64
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuUndoCapturePlan {
    layout: AtlasLayout,
    regions: Vec<GpuUndoCopyRegion>,
    block_count: u32,
    byte_len: u64,
}

pub struct GpuDocumentMemento {
    plan: GpuUndoCapturePlan,
    buffer: wgpu::Buffer,
}

impl GpuDocumentMemento {
    pub(crate) fn new(
        device: &wgpu::Device,
        plan: GpuUndoCapturePlan,
    ) -> Result<Self, GpuUndoResourceError> {
        if plan.is_empty() {
            return Err(GpuUndoResourceError::EmptyCapture);
        }
        if plan.byte_len() > device.limits().max_buffer_size {
            return Err(GpuUndoResourceError::BufferTooLarge {
                requested: plan.byte_len(),
                maximum: device.limits().max_buffer_size,
            });
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU Document Exact Undo Memento"),
            size: plan.byte_len(),
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self { plan, buffer })
    }

    pub fn plan(&self) -> &GpuUndoCapturePlan {
        &self.plan
    }

    pub const fn block_count(&self) -> u32 {
        self.plan.block_count()
    }

    pub const fn byte_len(&self) -> u64 {
        self.plan.byte_len()
    }

    pub(crate) const fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
}

impl GpuUndoCapturePlan {
    pub fn from_mask(mask: &RoundMaskTarget) -> Result<Self, GpuUndoPlanError> {
        Self::from_active_tiles(mask.layout(), &mask.active_tiles())
    }

    pub fn from_active_tiles(
        layout: AtlasLayout,
        active_tiles: &[ActiveRoundMaskTile],
    ) -> Result<Self, GpuUndoPlanError> {
        validate_layout(layout)?;

        let mut merged = HashMap::<(LayerTileKey, AtlasSlot), RectU32>::new();
        let mut occupants = HashMap::<AtlasSlot, LayerTileKey>::new();
        for active in active_tiles {
            if active.local_damage.max_x() > layout.tile_size()
                || active.local_damage.max_y() > layout.tile_size()
            {
                return Err(GpuUndoPlanError::DamageOutsideTile {
                    key: active.key,
                    damage: active.local_damage,
                    tile_size: layout.tile_size(),
                });
            }
            if let Some(previous) = occupants.insert(active.slot, active.key) {
                if previous != active.key {
                    return Err(GpuUndoPlanError::ConflictingSlotOccupants {
                        slot: active.slot,
                        first: previous,
                        second: active.key,
                    });
                }
            }
            merged
                .entry((active.key, active.slot))
                .and_modify(|damage| *damage = damage.union(active.local_damage))
                .or_insert(active.local_damage);
        }

        let mut tiles: Vec<_> = merged.into_iter().collect();
        tiles.sort_by_key(|((key, slot), _)| {
            (
                slot.page().get(),
                slot.slot_in_page(),
                key.layer.get(),
                key.tile.y,
                key.tile.x,
            )
        });

        let mut regions = Vec::with_capacity(tiles.len());
        let mut block_count = 0_u32;
        let mut byte_len = 0_u64;
        for ((key, slot), damage) in tiles {
            let local_bounds = round_damage_to_blocks(damage)?;
            let extent = [local_bounds.width(), local_bounds.height()];
            let region_blocks = (extent[0] / GPU_UNDO_BLOCK_SIZE)
                .checked_mul(extent[1] / GPU_UNDO_BLOCK_SIZE)
                .ok_or(GpuUndoPlanError::ArithmeticOverflow)?;
            let bytes_per_row = extent[0]
                .checked_mul(GPU_UNDO_PIXEL_BYTES)
                .ok_or(GpuUndoPlanError::ArithmeticOverflow)?;
            let region_bytes = u64::from(bytes_per_row)
                .checked_mul(u64::from(extent[1]))
                .ok_or(GpuUndoPlanError::ArithmeticOverflow)?;
            let slot_origin = slot.origin();
            let physical_origin = [
                slot_origin[0]
                    .checked_add(local_bounds.min_x())
                    .ok_or(GpuUndoPlanError::ArithmeticOverflow)?,
                slot_origin[1]
                    .checked_add(local_bounds.min_y())
                    .ok_or(GpuUndoPlanError::ArithmeticOverflow)?,
            ];
            regions.push(GpuUndoCopyRegion {
                key,
                slot,
                local_bounds,
                physical_origin,
                extent,
                buffer_offset: byte_len,
                bytes_per_row,
                block_count: region_blocks,
            });
            block_count = block_count
                .checked_add(region_blocks)
                .ok_or(GpuUndoPlanError::ArithmeticOverflow)?;
            byte_len = byte_len
                .checked_add(region_bytes)
                .ok_or(GpuUndoPlanError::ArithmeticOverflow)?;
        }

        Ok(Self {
            layout,
            regions,
            block_count,
            byte_len,
        })
    }

    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub fn regions(&self) -> &[GpuUndoCopyRegion] {
        &self.regions
    }

    pub const fn block_count(&self) -> u32 {
        self.block_count
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

fn validate_layout(layout: AtlasLayout) -> Result<(), GpuUndoPlanError> {
    if !layout.tile_size().is_multiple_of(GPU_UNDO_BLOCK_SIZE) {
        return Err(GpuUndoPlanError::TileSizeNotBlockAligned {
            tile_size: layout.tile_size(),
            block_size: GPU_UNDO_BLOCK_SIZE,
        });
    }
    if GPU_UNDO_BLOCK_SIZE * GPU_UNDO_PIXEL_BYTES != wgpu::COPY_BYTES_PER_ROW_ALIGNMENT {
        return Err(GpuUndoPlanError::BlockRowNotCopyAligned);
    }
    Ok(())
}

fn round_damage_to_blocks(damage: RectU32) -> Result<RectU32, GpuUndoPlanError> {
    let min_x = damage.min_x() / GPU_UNDO_BLOCK_SIZE * GPU_UNDO_BLOCK_SIZE;
    let min_y = damage.min_y() / GPU_UNDO_BLOCK_SIZE * GPU_UNDO_BLOCK_SIZE;
    let max_x = damage
        .max_x()
        .div_ceil(GPU_UNDO_BLOCK_SIZE)
        .checked_mul(GPU_UNDO_BLOCK_SIZE)
        .ok_or(GpuUndoPlanError::ArithmeticOverflow)?;
    let max_y = damage
        .max_y()
        .div_ceil(GPU_UNDO_BLOCK_SIZE)
        .checked_mul(GPU_UNDO_BLOCK_SIZE)
        .ok_or(GpuUndoPlanError::ArithmeticOverflow)?;
    RectU32::from_min_max(min_x, min_y, max_x, max_y).ok_or(GpuUndoPlanError::ArithmeticOverflow)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuUndoPlanError {
    TileSizeNotBlockAligned {
        tile_size: u32,
        block_size: u32,
    },
    BlockRowNotCopyAligned,
    DamageOutsideTile {
        key: LayerTileKey,
        damage: RectU32,
        tile_size: u32,
    },
    ConflictingSlotOccupants {
        slot: AtlasSlot,
        first: LayerTileKey,
        second: LayerTileKey,
    },
    ArithmeticOverflow,
}

impl fmt::Display for GpuUndoPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TileSizeNotBlockAligned {
                tile_size,
                block_size,
            } => write!(
                formatter,
                "GPU undo tile size {tile_size} is not aligned to {block_size}-pixel blocks"
            ),
            Self::BlockRowNotCopyAligned => {
                write!(formatter, "one GPU undo block row is not copy aligned")
            }
            Self::DamageOutsideTile {
                key,
                damage,
                tile_size,
            } => write!(
                formatter,
                "GPU undo damage {damage:?} for {key:?} exceeds its {tile_size}-pixel tile"
            ),
            Self::ConflictingSlotOccupants {
                slot,
                first,
                second,
            } => write!(
                formatter,
                "GPU undo slot {slot:?} names conflicting occupants {first:?} and {second:?}"
            ),
            Self::ArithmeticOverflow => write!(formatter, "GPU undo plan size overflows"),
        }
    }
}

impl Error for GpuUndoPlanError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuUndoResourceError {
    EmptyCapture,
    BufferTooLarge { requested: u64, maximum: u64 },
}

impl fmt::Display for GpuUndoResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCapture => write!(formatter, "GPU undo capture is empty"),
            Self::BufferTooLarge { requested, maximum } => write!(
                formatter,
                "GPU undo buffer {requested} exceeds device limit {maximum}"
            ),
        }
    }
}

impl Error for GpuUndoResourceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::LayerId, gpu_atlas::SparseAtlasPlanner, raster::TileCoord};

    fn active_tile(
        atlas: &mut SparseAtlasPlanner,
        layer: LayerId,
        tile: TileCoord,
        damage: RectU32,
    ) -> ActiveRoundMaskTile {
        let key = LayerTileKey::new(layer, tile);
        let slot = atlas.allocate(key).unwrap().slot;
        ActiveRoundMaskTile {
            key,
            slot,
            local_damage: damage,
        }
    }

    #[test]
    fn one_pixel_rounds_to_one_copy_aligned_full_float_block() {
        let layout = AtlasLayout::document_default();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let tile = active_tile(
            &mut atlas,
            LayerId::from_raw(1),
            TileCoord::new(0, 0),
            RectU32::from_xywh(17, 31, 1, 1).unwrap(),
        );
        let plan = GpuUndoCapturePlan::from_active_tiles(layout, &[tile]).unwrap();
        assert_eq!(plan.block_count(), 1);
        assert_eq!(plan.byte_len(), GPU_UNDO_BLOCK_BYTES);
        assert_eq!(plan.regions().len(), 1);
        assert_eq!(
            plan.regions()[0].local_bounds,
            RectU32::from_min_max(16, 16, 32, 32).unwrap()
        );
        assert_eq!(plan.regions()[0].bytes_per_row, 256);
    }

    #[test]
    fn rectangular_block_work_coalesces_into_one_copy_region_per_tile() {
        let layout = AtlasLayout::document_default();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let layer = LayerId::from_raw(2);
        let left = active_tile(
            &mut atlas,
            layer,
            TileCoord::new(0, 0),
            RectU32::from_min_max(15, 15, 33, 49).unwrap(),
        );
        let right = active_tile(
            &mut atlas,
            layer,
            TileCoord::new(1, 0),
            RectU32::from_min_max(0, 55, 73, 73).unwrap(),
        );
        let plan = GpuUndoCapturePlan::from_active_tiles(layout, &[right, left]).unwrap();
        assert_eq!(plan.regions().len(), 2);
        assert_eq!(plan.regions()[0].block_count, 12);
        assert_eq!(plan.regions()[1].block_count, 10);
        assert_eq!(plan.block_count(), 22);
        assert_eq!(plan.regions()[1].buffer_offset, 12 * GPU_UNDO_BLOCK_BYTES);
        assert_eq!(plan.byte_len(), 22 * GPU_UNDO_BLOCK_BYTES);
    }

    #[test]
    fn a_full_tile_is_sixty_four_blocks_but_one_copy_region() {
        let layout = AtlasLayout::document_default();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let tile = active_tile(
            &mut atlas,
            LayerId::from_raw(3),
            TileCoord::new(0, 0),
            RectU32::from_xywh(0, 0, 128, 128).unwrap(),
        );
        let plan = GpuUndoCapturePlan::from_active_tiles(layout, &[tile]).unwrap();
        assert_eq!(plan.block_count(), 64);
        assert_eq!(plan.regions().len(), 1);
        assert_eq!(plan.byte_len(), 128 * 128 * 16);
        assert_eq!(plan.regions()[0].bytes_per_row, 2_048);
    }

    #[test]
    fn incompatible_tile_size_fails_before_planning() {
        let layout = AtlasLayout::new(240, 120, 1).unwrap();
        assert_eq!(
            GpuUndoCapturePlan::from_active_tiles(layout, &[]).unwrap_err(),
            GpuUndoPlanError::TileSizeNotBlockAligned {
                tile_size: 120,
                block_size: 16,
            }
        );
    }
}
