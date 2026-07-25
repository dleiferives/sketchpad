use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fmt, mem,
};

pub const DEFAULT_TILE_SIZE: u32 = 128;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LinearRgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl LinearRgba {
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub const fn premultiplied(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_straight(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self {
            r: r * a,
            g: g * a,
            b: b * a,
            a,
        }
    }

    fn is_transparent(self) -> bool {
        self == Self::TRANSPARENT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectU32 {
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
}

impl RectU32 {
    pub fn from_min_max(min_x: u32, min_y: u32, max_x: u32, max_y: u32) -> Option<Self> {
        (min_x < max_x && min_y < max_y).then_some(Self {
            min_x,
            min_y,
            max_x,
            max_y,
        })
    }

    pub fn from_xywh(x: u32, y: u32, width: u32, height: u32) -> Option<Self> {
        Self::from_min_max(x, y, x.checked_add(width)?, y.checked_add(height)?)
    }

    pub const fn min_x(self) -> u32 {
        self.min_x
    }

    pub const fn min_y(self) -> u32 {
        self.min_y
    }

    pub const fn max_x(self) -> u32 {
        self.max_x
    }

    pub const fn max_y(self) -> u32 {
        self.max_y
    }

    pub const fn width(self) -> u32 {
        self.max_x - self.min_x
    }

    pub const fn height(self) -> u32 {
        self.max_y - self.min_y
    }

    pub const fn area(self) -> u64 {
        self.width() as u64 * self.height() as u64
    }

    pub fn contains(self, x: u32, y: u32) -> bool {
        x >= self.min_x && x < self.max_x && y >= self.min_y && y < self.max_y
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    fn translated(self, x: u32, y: u32) -> Self {
        Self {
            min_x: self.min_x + x,
            min_y: self.min_y + y,
            max_x: self.max_x + x,
            max_y: self.max_y + y,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    bounds: Option<RectU32>,
    tiles: Vec<TileCoord>,
    tile_bounds: Vec<RectU32>,
}

impl Damage {
    pub fn bounds(&self) -> Option<RectU32> {
        self.bounds
    }

    pub fn tiles(&self) -> &[TileCoord] {
        &self.tiles
    }

    pub fn tile_regions(&self) -> impl Iterator<Item = (TileCoord, RectU32)> + '_ {
        self.tiles
            .iter()
            .copied()
            .zip(self.tile_bounds.iter().copied())
    }

    pub fn is_empty(&self) -> bool {
        self.bounds.is_none()
    }

    fn add(&mut self, tile: TileCoord, bounds: RectU32) {
        self.bounds = Some(match self.bounds {
            Some(existing) => existing.union(bounds),
            None => bounds,
        });
        if let Some(index) = self.tiles.iter().position(|existing| *existing == tile) {
            self.tile_bounds[index] = self.tile_bounds[index].union(bounds);
        } else {
            self.tiles.push(tile);
            self.tile_bounds.push(bounds);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GestureId(u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RasterStats {
    pub write_tile_lookups: u64,
    pub bulk_tile_edits: u64,
    pub tiles_allocated: u64,
    pub before_images_recorded: u64,
    pub snapshot_blocks: u64,
    pub snapshot_bytes: u64,
    pub history_swap_blocks: u64,
    pub history_swap_bytes: u64,
    pub conservatively_touched_pixels: u64,
    pub content_bound_pixels_scanned: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UndoStorage {
    #[default]
    WholeTile,
    Blocks16,
    BrushAdaptive16,
}

impl UndoStorage {
    fn for_brush_diameter(self, diameter: f32, tile_size: u32) -> Self {
        match self {
            Self::WholeTile => Self::WholeTile,
            Self::Blocks16 => Self::Blocks16,
            Self::BrushAdaptive16 if diameter <= tile_size as f32 => Self::Blocks16,
            Self::BrushAdaptive16 => Self::WholeTile,
        }
    }

    fn without_brush_hint(self) -> Self {
        match self {
            Self::BrushAdaptive16 => Self::WholeTile,
            concrete => concrete,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RasterError {
    EmptyCanvas,
    InvalidTileSize,
    TileStorageTooLarge,
    InvalidTilePixelCount {
        expected: usize,
        actual: usize,
    },
    PixelOutOfBounds {
        x: u32,
        y: u32,
    },
    TileOutOfBounds(TileCoord),
    DamageOutsideTile {
        tile: TileCoord,
        damage: RectU32,
        valid_width: u32,
        valid_height: u32,
    },
    GestureAlreadyActive,
    NoActiveGesture,
    WrongGesture {
        expected: GestureId,
        actual: GestureId,
    },
}

impl fmt::Display for RasterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCanvas => write!(f, "canvas dimensions must both be nonzero"),
            Self::InvalidTileSize => write!(f, "tile size must be nonzero"),
            Self::TileStorageTooLarge => {
                write!(f, "one tile cannot be represented on this platform")
            }
            Self::InvalidTilePixelCount { expected, actual } => {
                write!(f, "tile pixel count is {actual}, expected {expected}")
            }
            Self::PixelOutOfBounds { x, y } => {
                write!(f, "pixel ({x}, {y}) lies outside the canvas")
            }
            Self::TileOutOfBounds(tile) => {
                write!(f, "tile ({}, {}) lies outside the canvas", tile.x, tile.y)
            }
            Self::DamageOutsideTile {
                tile,
                damage,
                valid_width,
                valid_height,
            } => write!(
                f,
                "damage {damage:?} lies outside tile ({}, {}) valid extent {}x{}",
                tile.x, tile.y, valid_width, valid_height
            ),
            Self::GestureAlreadyActive => write!(f, "a raster gesture is already active"),
            Self::NoActiveGesture => write!(f, "no raster gesture is active"),
            Self::WrongGesture { expected, actual } => write!(
                f,
                "gesture {actual:?} does not match active gesture {expected:?}"
            ),
        }
    }
}

impl Error for RasterError {}

#[derive(Clone)]
struct TileState {
    pixels: Box<[LinearRgba]>,
    content_bounds: Option<RectU32>,
    content_bounds_state: ContentBoundsState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentBoundsState {
    Clean,
    Shrink,
    Recompute,
}

impl TileState {
    fn empty(pixel_count: usize) -> Self {
        Self {
            pixels: vec![LinearRgba::TRANSPARENT; pixel_count].into_boxed_slice(),
            content_bounds: None,
            content_bounds_state: ContentBoundsState::Clean,
        }
    }

    fn recompute_content_bounds(&mut self, stride: usize, valid_width: u32, valid_height: u32) {
        let mut bounds: Option<RectU32> = None;

        for y in 0..valid_height {
            let row_start = y as usize * stride;
            for x in 0..valid_width {
                if !self.pixels[row_start + x as usize].is_transparent() {
                    let pixel_bounds = RectU32 {
                        min_x: x,
                        min_y: y,
                        max_x: x + 1,
                        max_y: y + 1,
                    };
                    bounds = Some(match bounds {
                        Some(existing) => existing.union(pixel_bounds),
                        None => pixel_bounds,
                    });
                }
            }
        }

        self.content_bounds = bounds;
        self.content_bounds_state = ContentBoundsState::Clean;
    }

    fn shrink_content_bounds(&mut self, stride: usize) -> u64 {
        let Some(old) = self.content_bounds else {
            self.content_bounds_state = ContentBoundsState::Clean;
            return 0;
        };
        let mut scanned = 0;

        let mut min_y = None;
        for y in old.min_y()..old.max_y() {
            let row_start = y as usize * stride;
            let mut occupied = false;
            for x in old.min_x()..old.max_x() {
                scanned += 1;
                if !self.pixels[row_start + x as usize].is_transparent() {
                    occupied = true;
                    break;
                }
            }
            if occupied {
                min_y = Some(y);
                break;
            }
        }
        let Some(min_y) = min_y else {
            self.content_bounds = None;
            self.content_bounds_state = ContentBoundsState::Clean;
            return scanned;
        };

        let mut max_y = min_y + 1;
        for y in (min_y + 1..old.max_y()).rev() {
            let row_start = y as usize * stride;
            let mut occupied = false;
            for x in old.min_x()..old.max_x() {
                scanned += 1;
                if !self.pixels[row_start + x as usize].is_transparent() {
                    occupied = true;
                    break;
                }
            }
            if occupied {
                max_y = y + 1;
                break;
            }
        }

        let mut min_x = old.min_x();
        'min_x: for x in old.min_x()..old.max_x() {
            for y in min_y..max_y {
                scanned += 1;
                if !self.pixels[y as usize * stride + x as usize].is_transparent() {
                    min_x = x;
                    break 'min_x;
                }
            }
        }

        let mut max_x = min_x + 1;
        'max_x: for x in (min_x + 1..old.max_x()).rev() {
            for y in min_y..max_y {
                scanned += 1;
                if !self.pixels[y as usize * stride + x as usize].is_transparent() {
                    max_x = x + 1;
                    break 'max_x;
                }
            }
        }

        self.content_bounds = RectU32::from_min_max(min_x, min_y, max_x, max_y);
        self.content_bounds_state = ContentBoundsState::Clean;
        scanned
    }
}

enum ContentChange {
    RecomputeBounds,
    Additive(Option<RectU32>),
    Subtractive(Option<RectU32>),
}

struct Tile {
    state: TileState,
    snapshot_gesture: u64,
    snapshot_index: usize,
}

#[derive(Clone, Copy)]
struct TileMetadata {
    content_bounds: Option<RectU32>,
    content_bounds_state: ContentBoundsState,
}

impl TileMetadata {
    fn from_state(state: &TileState) -> Self {
        Self {
            content_bounds: state.content_bounds,
            content_bounds_state: state.content_bounds_state,
        }
    }

    fn apply(self, state: &mut TileState) {
        state.content_bounds = self.content_bounds;
        state.content_bounds_state = self.content_bounds_state;
    }
}

struct BlockTileSnapshot {
    target_metadata: Option<TileMetadata>,
    block_indices: Vec<u32>,
    pixels: Vec<LinearRgba>,
    captured: Box<[u64]>,
}

impl BlockTileSnapshot {
    const BLOCK_SIZE: u32 = 16;
    const BLOCK_PIXELS: usize = (Self::BLOCK_SIZE * Self::BLOCK_SIZE) as usize;
    const BLOCK_BYTES: u64 = Self::BLOCK_PIXELS as u64 * mem::size_of::<LinearRgba>() as u64;

    fn new(target: Option<&TileState>, tile_size: u32) -> Self {
        let blocks_wide = tile_size.div_ceil(Self::BLOCK_SIZE);
        let block_count = blocks_wide
            .checked_mul(blocks_wide)
            .expect("validated tile storage also bounds block metadata");
        Self {
            target_metadata: target.map(TileMetadata::from_state),
            block_indices: Vec::new(),
            pixels: Vec::new(),
            captured: vec![0; block_count.div_ceil(64) as usize].into_boxed_slice(),
        }
    }

    fn capture_damage(
        &mut self,
        state: &TileState,
        damage: RectU32,
        tile_size: u32,
        stats: &mut RasterStats,
    ) {
        let blocks_wide = tile_size.div_ceil(Self::BLOCK_SIZE);
        let min_block_x = damage.min_x() / Self::BLOCK_SIZE;
        let min_block_y = damage.min_y() / Self::BLOCK_SIZE;
        let max_block_x = (damage.max_x() - 1) / Self::BLOCK_SIZE;
        let max_block_y = (damage.max_y() - 1) / Self::BLOCK_SIZE;

        for block_y in min_block_y..=max_block_y {
            for block_x in min_block_x..=max_block_x {
                let block_index = block_y * blocks_wide + block_x;
                let word = block_index as usize / 64;
                let bit = 1_u64 << (block_index % 64);
                if self.captured[word] & bit != 0 {
                    continue;
                }
                self.captured[word] |= bit;
                self.block_indices.push(block_index);
                stats.snapshot_blocks = stats.snapshot_blocks.saturating_add(1);
                if self.target_metadata.is_some() {
                    append_block(
                        &mut self.pixels,
                        &state.pixels,
                        block_index,
                        blocks_wide,
                        tile_size,
                    );
                    stats.snapshot_bytes = stats.snapshot_bytes.saturating_add(Self::BLOCK_BYTES);
                }
            }
        }
    }

    fn restore_target(
        &self,
        current: Option<TileState>,
        tile_size: u32,
        tile_pixel_count: usize,
    ) -> Option<TileState> {
        let metadata = self.target_metadata?;
        let mut target = current.unwrap_or_else(|| TileState::empty(tile_pixel_count));
        write_blocks(
            &mut target.pixels,
            &self.block_indices,
            &self.pixels,
            tile_size,
        );
        metadata.apply(&mut target);
        Some(target)
    }

    fn swap_target(
        &mut self,
        mut current: Option<TileState>,
        tile_size: u32,
        tile_pixel_count: usize,
        stats: &mut RasterStats,
    ) -> Option<TileState> {
        let current_was_present = current.is_some();
        let current_metadata = current.as_ref().map(TileMetadata::from_state);
        let target_metadata = mem::replace(&mut self.target_metadata, current_metadata);
        let target_was_present = target_metadata.is_some();
        let block_count = self.block_indices.len() as u64;

        match (current.as_mut(), target_metadata) {
            (Some(current_state), Some(metadata)) => {
                swap_blocks(
                    &mut current_state.pixels,
                    &self.block_indices,
                    &mut self.pixels,
                    tile_size,
                );
                metadata.apply(current_state);
            }
            (Some(current_state), None) => {
                debug_assert!(self.pixels.is_empty());
                for &block_index in &self.block_indices {
                    append_block(
                        &mut self.pixels,
                        &current_state.pixels,
                        block_index,
                        tile_size.div_ceil(Self::BLOCK_SIZE),
                        tile_size,
                    );
                }
                current = None;
            }
            (None, Some(metadata)) => {
                let mut target = TileState::empty(tile_pixel_count);
                write_blocks(
                    &mut target.pixels,
                    &self.block_indices,
                    &self.pixels,
                    tile_size,
                );
                self.pixels.clear();
                metadata.apply(&mut target);
                current = Some(target);
            }
            (None, None) => {}
        }

        if current_was_present || target_was_present {
            stats.history_swap_blocks = stats.history_swap_blocks.saturating_add(block_count);
            stats.history_swap_bytes = stats
                .history_swap_bytes
                .saturating_add(block_count.saturating_mul(Self::BLOCK_BYTES));
        }
        current
    }
}

fn append_block(
    destination: &mut Vec<LinearRgba>,
    source: &[LinearRgba],
    block_index: u32,
    blocks_wide: u32,
    tile_size: u32,
) {
    let block_x = block_index % blocks_wide;
    let block_y = block_index / blocks_wide;
    let origin_x = block_x * BlockTileSnapshot::BLOCK_SIZE;
    let origin_y = block_y * BlockTileSnapshot::BLOCK_SIZE;
    destination.reserve(BlockTileSnapshot::BLOCK_PIXELS);

    for offset_y in 0..BlockTileSnapshot::BLOCK_SIZE {
        let y = origin_y + offset_y;
        let copy_width =
            BlockTileSnapshot::BLOCK_SIZE.min(tile_size.saturating_sub(origin_x)) as usize;
        if y < tile_size {
            let start = y as usize * tile_size as usize + origin_x as usize;
            destination.extend_from_slice(&source[start..start + copy_width]);
        }
        destination.extend(std::iter::repeat_n(
            LinearRgba::TRANSPARENT,
            BlockTileSnapshot::BLOCK_SIZE as usize - copy_width,
        ));
        if y >= tile_size {
            destination.extend(std::iter::repeat_n(LinearRgba::TRANSPARENT, copy_width));
        }
    }
}

fn write_blocks(
    destination: &mut [LinearRgba],
    block_indices: &[u32],
    source: &[LinearRgba],
    tile_size: u32,
) {
    debug_assert_eq!(
        source.len(),
        block_indices.len() * BlockTileSnapshot::BLOCK_PIXELS
    );
    let blocks_wide = tile_size.div_ceil(BlockTileSnapshot::BLOCK_SIZE);
    for (&block_index, block_pixels) in block_indices
        .iter()
        .zip(source.chunks_exact(BlockTileSnapshot::BLOCK_PIXELS))
    {
        let block_x = block_index % blocks_wide;
        let block_y = block_index / blocks_wide;
        let origin_x = block_x * BlockTileSnapshot::BLOCK_SIZE;
        let origin_y = block_y * BlockTileSnapshot::BLOCK_SIZE;
        for offset_y in 0..BlockTileSnapshot::BLOCK_SIZE {
            let y = origin_y + offset_y;
            if y >= tile_size {
                continue;
            }
            let copy_width =
                BlockTileSnapshot::BLOCK_SIZE.min(tile_size.saturating_sub(origin_x)) as usize;
            let destination_start = y as usize * tile_size as usize + origin_x as usize;
            let source_start = offset_y as usize * BlockTileSnapshot::BLOCK_SIZE as usize;
            destination[destination_start..destination_start + copy_width]
                .copy_from_slice(&block_pixels[source_start..source_start + copy_width]);
        }
    }
}

fn swap_blocks(
    tile_pixels: &mut [LinearRgba],
    block_indices: &[u32],
    snapshot_pixels: &mut [LinearRgba],
    tile_size: u32,
) {
    debug_assert_eq!(
        snapshot_pixels.len(),
        block_indices.len() * BlockTileSnapshot::BLOCK_PIXELS
    );
    let blocks_wide = tile_size.div_ceil(BlockTileSnapshot::BLOCK_SIZE);
    for (&block_index, block_pixels) in block_indices
        .iter()
        .zip(snapshot_pixels.chunks_exact_mut(BlockTileSnapshot::BLOCK_PIXELS))
    {
        let block_x = block_index % blocks_wide;
        let block_y = block_index / blocks_wide;
        let origin_x = block_x * BlockTileSnapshot::BLOCK_SIZE;
        let origin_y = block_y * BlockTileSnapshot::BLOCK_SIZE;
        for offset_y in 0..BlockTileSnapshot::BLOCK_SIZE {
            let y = origin_y + offset_y;
            if y >= tile_size {
                continue;
            }
            let copy_width =
                BlockTileSnapshot::BLOCK_SIZE.min(tile_size.saturating_sub(origin_x)) as usize;
            let tile_start = y as usize * tile_size as usize + origin_x as usize;
            let block_start = offset_y as usize * BlockTileSnapshot::BLOCK_SIZE as usize;
            tile_pixels[tile_start..tile_start + copy_width]
                .swap_with_slice(&mut block_pixels[block_start..block_start + copy_width]);
        }
    }
}

enum TileSnapshotState {
    Whole(Option<TileState>),
    Blocks16(BlockTileSnapshot),
}

struct TileSnapshot {
    coord: TileCoord,
    state: TileSnapshotState,
}

struct HistoryEntry {
    snapshots: Vec<TileSnapshot>,
    damage: Damage,
}

struct ActiveGesture {
    id: GestureId,
    undo_storage: UndoStorage,
    snapshots: Vec<TileSnapshot>,
    damage: Damage,
    pending_damage: Damage,
}

pub struct RasterLayer {
    width: u32,
    height: u32,
    tile_size: u32,
    tile_pixel_count: usize,
    tiles: HashMap<TileCoord, Tile>,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    next_gesture: u64,
    active_gesture: Option<ActiveGesture>,
    undo_storage: UndoStorage,
    stats: RasterStats,
}

impl RasterLayer {
    pub fn new(width: u32, height: u32, tile_size: u32) -> Result<Self, RasterError> {
        Self::new_with_undo_storage(width, height, tile_size, UndoStorage::BrushAdaptive16)
    }

    pub fn new_with_undo_storage(
        width: u32,
        height: u32,
        tile_size: u32,
        undo_storage: UndoStorage,
    ) -> Result<Self, RasterError> {
        if width == 0 || height == 0 {
            return Err(RasterError::EmptyCanvas);
        }
        if tile_size == 0 {
            return Err(RasterError::InvalidTileSize);
        }

        let tile_pixel_count_u32 = tile_size
            .checked_mul(tile_size)
            .ok_or(RasterError::TileStorageTooLarge)?;
        let tile_pixel_count =
            usize::try_from(tile_pixel_count_u32).map_err(|_| RasterError::TileStorageTooLarge)?;

        Ok(Self {
            width,
            height,
            tile_size,
            tile_pixel_count,
            tiles: HashMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            next_gesture: 1,
            active_gesture: None,
            undo_storage,
            stats: RasterStats::default(),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn tile_size(&self) -> u32 {
        self.tile_size
    }

    pub fn undo_storage(&self) -> UndoStorage {
        self.undo_storage
    }

    pub fn tile_grid_extent(&self) -> [u32; 2] {
        [
            (self.width - 1) / self.tile_size + 1,
            (self.height - 1) / self.tile_size + 1,
        ]
    }

    pub fn allocated_tile_count(&self) -> usize {
        self.tiles.len()
    }

    pub fn tile_is_allocated(&self, coord: TileCoord) -> bool {
        self.tiles.contains_key(&coord)
    }

    pub fn allocated_tile_coords(&self) -> impl Iterator<Item = TileCoord> + '_ {
        self.tiles.keys().copied()
    }

    pub fn tile(&self, coord: TileCoord) -> Option<RasterTile<'_>> {
        let tile = self.tiles.get(&coord)?;
        let bounds = self
            .tile_bounds(coord)
            .expect("allocated tiles always lie inside the canvas");
        Some(RasterTile {
            coord,
            bounds,
            pixels: &tile.state.pixels,
            stride: self.tile_size as usize,
        })
    }

    pub(crate) fn restore_tile(
        &mut self,
        coord: TileCoord,
        pixels: Box<[LinearRgba]>,
    ) -> Result<(), RasterError> {
        let bounds = self
            .tile_bounds(coord)
            .ok_or(RasterError::TileOutOfBounds(coord))?;
        if pixels.len() != self.tile_pixel_count {
            return Err(RasterError::InvalidTilePixelCount {
                expected: self.tile_pixel_count,
                actual: pixels.len(),
            });
        }

        let mut state = TileState {
            pixels,
            content_bounds: None,
            content_bounds_state: ContentBoundsState::Recompute,
        };
        state.recompute_content_bounds(self.tile_size as usize, bounds.width(), bounds.height());
        if state.content_bounds.is_some() {
            self.tiles.insert(
                coord,
                Tile {
                    state,
                    snapshot_gesture: 0,
                    snapshot_index: 0,
                },
            );
        }
        Ok(())
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    pub fn stats(&self) -> RasterStats {
        self.stats
    }

    pub fn reset_stats(&mut self) {
        self.stats = RasterStats::default();
    }

    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn tile_bounds(&self, coord: TileCoord) -> Option<RectU32> {
        let [tiles_wide, tiles_high] = self.tile_grid_extent();
        if coord.x >= tiles_wide || coord.y >= tiles_high {
            return None;
        }

        let min_x = coord.x * self.tile_size;
        let min_y = coord.y * self.tile_size;
        Some(RectU32 {
            min_x,
            min_y,
            max_x: min_x.saturating_add(self.tile_size).min(self.width),
            max_y: min_y.saturating_add(self.tile_size).min(self.height),
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<LinearRgba> {
        let coord = self.tile_coord_for_pixel(x, y)?;
        let local_x = x % self.tile_size;
        let local_y = y % self.tile_size;
        let index = local_y as usize * self.tile_size as usize + local_x as usize;

        Some(
            self.tiles
                .get(&coord)
                .map_or(LinearRgba::TRANSPARENT, |tile| tile.state.pixels[index]),
        )
    }

    pub fn content_bounds(&self) -> Option<RectU32> {
        let mut document_bounds: Option<RectU32> = None;

        for (coord, tile) in &self.tiles {
            let Some(local_bounds) = tile.state.content_bounds else {
                continue;
            };
            let tile_bounds = self
                .tile_bounds(*coord)
                .expect("allocated tiles always lie inside the canvas");
            let global_bounds = local_bounds.translated(tile_bounds.min_x(), tile_bounds.min_y());
            document_bounds = Some(match document_bounds {
                Some(existing) => existing.union(global_bounds),
                None => global_bounds,
            });
        }

        document_bounds
    }

    pub fn begin_gesture(&mut self) -> Result<GestureId, RasterError> {
        self.begin_gesture_with_storage(self.undo_storage.without_brush_hint())
    }

    pub fn begin_brush_gesture(&mut self, diameter: f32) -> Result<GestureId, RasterError> {
        let undo_storage = self
            .undo_storage
            .for_brush_diameter(diameter, self.tile_size);
        self.begin_gesture_with_storage(undo_storage)
    }

    fn begin_gesture_with_storage(
        &mut self,
        undo_storage: UndoStorage,
    ) -> Result<GestureId, RasterError> {
        debug_assert_ne!(undo_storage, UndoStorage::BrushAdaptive16);
        if self.active_gesture.is_some() {
            return Err(RasterError::GestureAlreadyActive);
        }

        let gesture_id = GestureId(self.next_gesture);
        self.next_gesture = self.next_gesture.wrapping_add(1);
        if self.next_gesture == 0 {
            for tile in self.tiles.values_mut() {
                tile.snapshot_gesture = 0;
            }
            self.next_gesture = 1;
        }

        self.active_gesture = Some(ActiveGesture {
            id: gesture_id,
            undo_storage,
            snapshots: Vec::new(),
            damage: Damage::default(),
            pending_damage: Damage::default(),
        });
        Ok(gesture_id)
    }

    pub fn scoped_gesture(&mut self) -> Result<Gesture<'_>, RasterError> {
        let gesture_id = self.begin_gesture()?;
        Ok(Gesture {
            layer: self,
            gesture_id,
            finished: false,
        })
    }

    pub fn active_gesture_id(&self) -> Option<GestureId> {
        self.active_gesture.as_ref().map(|gesture| gesture.id)
    }

    pub fn take_gesture_damage(&mut self, gesture_id: GestureId) -> Result<Damage, RasterError> {
        let gesture = self.active_gesture_mut(gesture_id)?;
        Ok(mem::take(&mut gesture.pending_damage))
    }

    pub fn set_pixel(
        &mut self,
        gesture_id: GestureId,
        x: u32,
        y: u32,
        value: LinearRgba,
    ) -> Result<bool, RasterError> {
        let current = self
            .pixel(x, y)
            .ok_or(RasterError::PixelOutOfBounds { x, y })?;
        if current == value {
            return Ok(false);
        }

        let coord = TileCoord::new(x / self.tile_size, y / self.tile_size);
        let local_x = x % self.tile_size;
        let local_y = y % self.tile_size;
        let local_damage = RectU32 {
            min_x: local_x,
            min_y: local_y,
            max_x: local_x + 1,
            max_y: local_y + 1,
        };

        if value.is_transparent() {
            self.edit_tile_subtractive(gesture_id, coord, local_damage, |tile| {
                let index = local_y as usize * tile.stride() + local_x as usize;
                tile.pixels_mut()[index] = value;
                ((), Some(local_damage))
            })?;
        } else {
            self.edit_tile_additive(gesture_id, coord, local_damage, |tile| {
                let index = local_y as usize * tile.stride() + local_x as usize;
                tile.pixels_mut()[index] = value;
                ((), Some(local_damage))
            })?;
        }
        Ok(true)
    }

    pub fn edit_tile<R>(
        &mut self,
        gesture_id: GestureId,
        coord: TileCoord,
        local_damage: RectU32,
        edit: impl FnOnce(&mut TileEdit<'_>) -> R,
    ) -> Result<R, RasterError> {
        self.edit_tile_internal(gesture_id, coord, local_damage, |tile| {
            (edit(tile), ContentChange::RecomputeBounds)
        })
    }

    pub fn edit_tile_additive<R>(
        &mut self,
        gesture_id: GestureId,
        coord: TileCoord,
        local_damage: RectU32,
        edit: impl FnOnce(&mut TileEdit<'_>) -> (R, Option<RectU32>),
    ) -> Result<R, RasterError> {
        self.edit_tile_internal(gesture_id, coord, local_damage, |tile| {
            let (result, changed_bounds) = edit(tile);
            (result, ContentChange::Additive(changed_bounds))
        })
    }

    pub fn edit_tile_subtractive<R>(
        &mut self,
        gesture_id: GestureId,
        coord: TileCoord,
        local_damage: RectU32,
        edit: impl FnOnce(&mut TileEdit<'_>) -> (R, Option<RectU32>),
    ) -> Result<R, RasterError> {
        self.edit_tile_internal(gesture_id, coord, local_damage, |tile| {
            let (result, emptied_bounds) = edit(tile);
            (result, ContentChange::Subtractive(emptied_bounds))
        })
    }

    fn edit_tile_internal<R>(
        &mut self,
        gesture_id: GestureId,
        coord: TileCoord,
        local_damage: RectU32,
        edit: impl FnOnce(&mut TileEdit<'_>) -> (R, ContentChange),
    ) -> Result<R, RasterError> {
        self.require_active_gesture(gesture_id)?;

        let tile_bounds = self
            .tile_bounds(coord)
            .ok_or(RasterError::TileOutOfBounds(coord))?;
        let valid_width = tile_bounds.width();
        let valid_height = tile_bounds.height();

        if local_damage.max_x() > valid_width || local_damage.max_y() > valid_height {
            return Err(RasterError::DamageOutsideTile {
                tile: coord,
                damage: local_damage,
                valid_width,
                valid_height,
            });
        }

        let global_damage = local_damage.translated(tile_bounds.min_x(), tile_bounds.min_y());
        let tile_pixel_count = self.tile_pixel_count;
        let tile_size_u32 = self.tile_size;
        let tile_size = tile_size_u32 as usize;
        let pixel_bytes = (tile_pixel_count * mem::size_of::<LinearRgba>()) as u64;

        let RasterLayer {
            tiles,
            active_gesture,
            stats,
            ..
        } = self;
        let gesture = active_gesture
            .as_mut()
            .expect("the active gesture was validated above");
        let undo_storage = gesture.undo_storage;
        stats.write_tile_lookups += 1;
        stats.bulk_tile_edits += 1;
        stats.conservatively_touched_pixels += local_damage.area();

        let (tile, snapshot_index) = match tiles.entry(coord) {
            Entry::Occupied(mut occupied) => {
                if occupied.get().snapshot_gesture != gesture_id.0 {
                    let snapshot_index = gesture.snapshots.len();
                    let state = match undo_storage {
                        UndoStorage::WholeTile => {
                            stats.snapshot_bytes = stats.snapshot_bytes.saturating_add(pixel_bytes);
                            TileSnapshotState::Whole(Some(occupied.get().state.clone()))
                        }
                        UndoStorage::Blocks16 => TileSnapshotState::Blocks16(
                            BlockTileSnapshot::new(Some(&occupied.get().state), tile_size_u32),
                        ),
                        UndoStorage::BrushAdaptive16 => {
                            unreachable!("brush-adaptive storage resolves at gesture start")
                        }
                    };
                    gesture.snapshots.push(TileSnapshot { coord, state });
                    stats.before_images_recorded += 1;
                    let tile = occupied.get_mut();
                    tile.snapshot_gesture = gesture_id.0;
                    tile.snapshot_index = snapshot_index;
                }
                let snapshot_index = occupied.get().snapshot_index;
                (occupied.into_mut(), snapshot_index)
            }
            Entry::Vacant(vacant) => {
                let snapshot_index = gesture.snapshots.len();
                let state = match undo_storage {
                    UndoStorage::WholeTile => TileSnapshotState::Whole(None),
                    // A newly allocated tile has no before-pixels to preserve. The absence
                    // marker also lets undo/redo transfer the whole tile without copying
                    // block payloads.
                    UndoStorage::Blocks16 => TileSnapshotState::Whole(None),
                    UndoStorage::BrushAdaptive16 => {
                        unreachable!("brush-adaptive storage resolves at gesture start")
                    }
                };
                gesture.snapshots.push(TileSnapshot { coord, state });
                stats.before_images_recorded += 1;
                stats.tiles_allocated += 1;
                (
                    vacant.insert(Tile {
                        state: TileState::empty(tile_pixel_count),
                        snapshot_gesture: gesture_id.0,
                        snapshot_index,
                    }),
                    snapshot_index,
                )
            }
        };

        if let TileSnapshotState::Blocks16(snapshot) = &mut gesture.snapshots[snapshot_index].state
        {
            snapshot.capture_damage(&tile.state, local_damage, tile_size_u32, stats);
        }
        gesture.damage.add(coord, global_damage);
        gesture.pending_damage.add(coord, global_damage);
        let mut tile_edit = TileEdit {
            pixels: &mut tile.state.pixels,
            stride: tile_size,
            valid_width,
            valid_height,
        };
        let (result, content_change) = edit(&mut tile_edit);
        match content_change {
            ContentChange::RecomputeBounds => {
                tile.state.content_bounds_state = ContentBoundsState::Recompute;
            }
            ContentChange::Additive(changed_bounds) => {
                if let Some(changed_bounds) = changed_bounds {
                    debug_assert!(changed_bounds.max_x() <= valid_width);
                    debug_assert!(changed_bounds.max_y() <= valid_height);
                    match tile.state.content_bounds_state {
                        ContentBoundsState::Clean => {
                            tile.state.content_bounds = Some(match tile.state.content_bounds {
                                Some(existing) => existing.union(changed_bounds),
                                None => changed_bounds,
                            });
                        }
                        ContentBoundsState::Shrink | ContentBoundsState::Recompute => {
                            tile.state.content_bounds_state = ContentBoundsState::Recompute;
                        }
                    }
                }
            }
            ContentChange::Subtractive(emptied_bounds) => {
                if let Some(emptied_bounds) = emptied_bounds {
                    debug_assert!(emptied_bounds.max_x() <= valid_width);
                    debug_assert!(emptied_bounds.max_y() <= valid_height);
                    if tile.state.content_bounds_state == ContentBoundsState::Clean {
                        if let Some(content_bounds) = tile.state.content_bounds {
                            let touches_boundary = emptied_bounds.min_x() <= content_bounds.min_x()
                                || emptied_bounds.min_y() <= content_bounds.min_y()
                                || emptied_bounds.max_x() >= content_bounds.max_x()
                                || emptied_bounds.max_y() >= content_bounds.max_y();
                            if touches_boundary {
                                tile.state.content_bounds_state = ContentBoundsState::Shrink;
                            }
                        }
                    }
                }
            }
        }
        Ok(result)
    }

    pub fn commit_gesture(&mut self, gesture_id: GestureId) -> Result<Option<Damage>, RasterError> {
        let mut gesture = self.take_active_gesture(gesture_id)?;
        self.refresh_touched_tiles(&gesture.snapshots);

        if gesture.snapshots.is_empty() {
            return Ok(None);
        }

        let damage = mem::take(&mut gesture.damage);
        self.undo.push(HistoryEntry {
            snapshots: gesture.snapshots,
            damage: damage.clone(),
        });
        self.redo.clear();
        Ok(Some(damage))
    }

    pub fn cancel_gesture(&mut self, gesture_id: GestureId) -> Result<Option<Damage>, RasterError> {
        let mut gesture = self.take_active_gesture(gesture_id)?;
        if gesture.snapshots.is_empty() {
            return Ok(None);
        }

        let damage = mem::take(&mut gesture.damage);
        self.restore_snapshots(&mut gesture.snapshots);
        Ok(Some(damage))
    }

    pub fn undo(&mut self) -> Option<Damage> {
        if self.active_gesture.is_some() {
            return None;
        }
        let mut entry = self.undo.pop()?;
        self.swap_history_states(&mut entry);
        let damage = entry.damage.clone();
        self.redo.push(entry);
        Some(damage)
    }

    pub fn redo(&mut self) -> Option<Damage> {
        if self.active_gesture.is_some() {
            return None;
        }
        let mut entry = self.redo.pop()?;
        self.swap_history_states(&mut entry);
        let damage = entry.damage.clone();
        self.undo.push(entry);
        Some(damage)
    }

    fn require_active_gesture(&self, gesture_id: GestureId) -> Result<(), RasterError> {
        let Some(active) = &self.active_gesture else {
            return Err(RasterError::NoActiveGesture);
        };
        if active.id != gesture_id {
            return Err(RasterError::WrongGesture {
                expected: active.id,
                actual: gesture_id,
            });
        }
        Ok(())
    }

    fn active_gesture_mut(
        &mut self,
        gesture_id: GestureId,
    ) -> Result<&mut ActiveGesture, RasterError> {
        self.require_active_gesture(gesture_id)?;
        Ok(self
            .active_gesture
            .as_mut()
            .expect("the active gesture was validated above"))
    }

    fn take_active_gesture(&mut self, gesture_id: GestureId) -> Result<ActiveGesture, RasterError> {
        self.require_active_gesture(gesture_id)?;
        Ok(self
            .active_gesture
            .take()
            .expect("the active gesture was validated above"))
    }

    fn tile_coord_for_pixel(&self, x: u32, y: u32) -> Option<TileCoord> {
        (x < self.width && y < self.height)
            .then_some(TileCoord::new(x / self.tile_size, y / self.tile_size))
    }

    fn refresh_touched_tiles(&mut self, snapshots: &[TileSnapshot]) {
        for snapshot in snapshots {
            let Some(bounds) = self.tile_bounds(snapshot.coord) else {
                continue;
            };
            let mut remove = false;

            if let Some(tile) = self.tiles.get_mut(&snapshot.coord) {
                match tile.state.content_bounds_state {
                    ContentBoundsState::Clean => {}
                    ContentBoundsState::Shrink => {
                        self.stats.content_bound_pixels_scanned +=
                            tile.state.shrink_content_bounds(self.tile_size as usize);
                    }
                    ContentBoundsState::Recompute => {
                        self.stats.content_bound_pixels_scanned += bounds.area();
                        tile.state.recompute_content_bounds(
                            self.tile_size as usize,
                            bounds.width(),
                            bounds.height(),
                        );
                    }
                }
                remove = tile.state.content_bounds.is_none();
            }

            if remove {
                self.tiles.remove(&snapshot.coord);
            }
        }
    }

    fn restore_snapshots(&mut self, snapshots: &mut [TileSnapshot]) {
        for snapshot in snapshots.iter_mut().rev() {
            let target = match &mut snapshot.state {
                TileSnapshotState::Whole(state) => state.take(),
                TileSnapshotState::Blocks16(blocks) => {
                    let current = self.tiles.remove(&snapshot.coord).map(|tile| tile.state);
                    blocks.restore_target(current, self.tile_size, self.tile_pixel_count)
                }
            };
            if let Some(state) = target {
                self.tiles.insert(
                    snapshot.coord,
                    Tile {
                        state,
                        snapshot_gesture: 0,
                        snapshot_index: 0,
                    },
                );
            } else {
                self.tiles.remove(&snapshot.coord);
            }
        }
    }

    fn swap_history_states(&mut self, entry: &mut HistoryEntry) {
        for snapshot in &mut entry.snapshots {
            let current = self.tiles.remove(&snapshot.coord).map(|tile| tile.state);
            let target = match &mut snapshot.state {
                TileSnapshotState::Whole(target) => mem::replace(target, current),
                TileSnapshotState::Blocks16(blocks) => blocks.swap_target(
                    current,
                    self.tile_size,
                    self.tile_pixel_count,
                    &mut self.stats,
                ),
            };

            if let Some(state) = target {
                self.tiles.insert(
                    snapshot.coord,
                    Tile {
                        state,
                        snapshot_gesture: 0,
                        snapshot_index: 0,
                    },
                );
            }
        }
    }
}

pub struct RasterTile<'a> {
    coord: TileCoord,
    bounds: RectU32,
    pixels: &'a [LinearRgba],
    stride: usize,
}

impl RasterTile<'_> {
    pub const fn coord(&self) -> TileCoord {
        self.coord
    }

    pub const fn bounds(&self) -> RectU32 {
        self.bounds
    }

    pub const fn stride(&self) -> usize {
        self.stride
    }

    pub fn pixels(&self) -> &[LinearRgba] {
        self.pixels
    }
}

pub struct TileEdit<'a> {
    pixels: &'a mut [LinearRgba],
    stride: usize,
    valid_width: u32,
    valid_height: u32,
}

impl TileEdit<'_> {
    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn valid_extent(&self) -> [u32; 2] {
        [self.valid_width, self.valid_height]
    }

    pub fn pixels_mut(&mut self) -> &mut [LinearRgba] {
        self.pixels
    }

    pub fn row_mut(&mut self, y: u32) -> Option<&mut [LinearRgba]> {
        if y >= self.valid_height {
            return None;
        }
        let start = y as usize * self.stride;
        Some(&mut self.pixels[start..start + self.valid_width as usize])
    }
}

pub struct Gesture<'a> {
    layer: &'a mut RasterLayer,
    gesture_id: GestureId,
    finished: bool,
}

impl Gesture<'_> {
    pub fn set_pixel(&mut self, x: u32, y: u32, value: LinearRgba) -> Result<bool, RasterError> {
        self.layer.set_pixel(self.gesture_id, x, y, value)
    }

    pub fn edit_tile<R>(
        &mut self,
        coord: TileCoord,
        local_damage: RectU32,
        edit: impl FnOnce(&mut TileEdit<'_>) -> R,
    ) -> Result<R, RasterError> {
        self.layer
            .edit_tile(self.gesture_id, coord, local_damage, edit)
    }

    pub fn edit_tile_subtractive<R>(
        &mut self,
        coord: TileCoord,
        local_damage: RectU32,
        edit: impl FnOnce(&mut TileEdit<'_>) -> (R, Option<RectU32>),
    ) -> Result<R, RasterError> {
        self.layer
            .edit_tile_subtractive(self.gesture_id, coord, local_damage, edit)
    }

    pub fn take_damage(&mut self) -> Result<Damage, RasterError> {
        self.layer.take_gesture_damage(self.gesture_id)
    }

    pub fn commit(mut self) -> Result<Option<Damage>, RasterError> {
        let damage = self.layer.commit_gesture(self.gesture_id)?;
        self.finished = true;
        Ok(damage)
    }

    pub fn cancel(mut self) -> Result<Option<Damage>, RasterError> {
        let damage = self.layer.cancel_gesture(self.gesture_id)?;
        self.finished = true;
        Ok(damage)
    }
}

impl Drop for Gesture<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.layer.cancel_gesture(self.gesture_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: LinearRgba = LinearRgba::premultiplied(1.0, 0.0, 0.0, 1.0);
    const BLUE: LinearRgba = LinearRgba::premultiplied(0.0, 0.0, 1.0, 1.0);

    fn layer() -> RasterLayer {
        RasterLayer::new(300, 200, 128).unwrap()
    }

    fn rect(min_x: u32, min_y: u32, max_x: u32, max_y: u32) -> RectU32 {
        RectU32::from_min_max(min_x, min_y, max_x, max_y).unwrap()
    }

    #[test]
    fn untouched_canvas_allocates_no_tiles() {
        let layer = layer();

        assert_eq!(layer.tile_grid_extent(), [3, 2]);
        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.pixel(299, 199), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.pixel(300, 199), None);
        assert_eq!(layer.content_bounds(), None);
    }

    #[test]
    fn bulk_edit_allocates_only_the_edge_tile() {
        let mut layer = layer();
        let edge = TileCoord::new(2, 1);
        let mut gesture = layer.scoped_gesture().unwrap();
        gesture
            .edit_tile(edge, rect(0, 0, 44, 72), |tile| {
                assert_eq!(tile.valid_extent(), [44, 72]);
                tile.row_mut(71).unwrap()[43] = RED;
            })
            .unwrap();
        let damage = gesture.commit().unwrap().unwrap();

        assert_eq!(damage.bounds(), Some(rect(256, 128, 300, 200)));
        assert_eq!(damage.tiles(), &[edge]);
        assert_eq!(layer.allocated_tile_count(), 1);
        assert_eq!(layer.pixel(299, 199), Some(RED));
        assert_eq!(layer.content_bounds(), Some(rect(299, 199, 300, 200)));
        assert_eq!(
            layer.stats(),
            RasterStats {
                write_tile_lookups: 1,
                bulk_tile_edits: 1,
                tiles_allocated: 1,
                before_images_recorded: 1,
                snapshot_blocks: 0,
                snapshot_bytes: 0,
                history_swap_blocks: 0,
                history_swap_bytes: 0,
                conservatively_touched_pixels: 44 * 72,
                content_bound_pixels_scanned: 44 * 72,
            }
        );
    }

    #[test]
    fn a_tile_is_snapshotted_once_per_gesture() {
        let mut layer = layer();
        let mut seed = layer.scoped_gesture().unwrap();
        seed.set_pixel(1, 1, RED).unwrap();
        seed.commit().unwrap();
        layer.reset_stats();

        let mut gesture = layer.scoped_gesture().unwrap();
        gesture.set_pixel(1, 1, BLUE).unwrap();
        gesture.set_pixel(2, 2, RED).unwrap();
        gesture.set_pixel(3, 3, RED).unwrap();
        gesture.commit().unwrap();

        let stats = layer.stats();
        assert_eq!(stats.write_tile_lookups, 3);
        assert_eq!(stats.before_images_recorded, 1);
        assert_eq!(
            stats.snapshot_bytes,
            128 * 128 * mem::size_of::<LinearRgba>() as u64
        );
        assert_eq!(layer.undo_depth(), 2);
    }

    #[test]
    fn brush_adaptive_undo_resolves_once_at_gesture_start() {
        let mut layer = RasterLayer::new(128, 128, 128).unwrap();
        assert_eq!(layer.undo_storage(), UndoStorage::BrushAdaptive16);

        let small = layer.begin_brush_gesture(128.0).unwrap();
        assert_eq!(
            layer.active_gesture.as_ref().unwrap().undo_storage,
            UndoStorage::Blocks16
        );
        layer.cancel_gesture(small).unwrap();

        let large = layer.begin_brush_gesture(128.01).unwrap();
        assert_eq!(
            layer.active_gesture.as_ref().unwrap().undo_storage,
            UndoStorage::WholeTile
        );
        layer.cancel_gesture(large).unwrap();

        let generic = layer.begin_gesture().unwrap();
        assert_eq!(
            layer.active_gesture.as_ref().unwrap().undo_storage,
            UndoStorage::WholeTile
        );
        layer.cancel_gesture(generic).unwrap();
    }

    #[test]
    fn block_undo_captures_each_conservative_block_once() {
        let mut layer =
            RasterLayer::new_with_undo_storage(128, 128, 128, UndoStorage::Blocks16).unwrap();
        let seed = layer.begin_gesture().unwrap();
        layer.set_pixel(seed, 1, 1, RED).unwrap();
        layer.commit_gesture(seed).unwrap();
        layer.clear_history();
        layer.reset_stats();

        let gesture = layer.begin_gesture().unwrap();
        layer.set_pixel(gesture, 1, 1, BLUE).unwrap();
        layer.set_pixel(gesture, 2, 2, RED).unwrap();
        layer.set_pixel(gesture, 20, 2, RED).unwrap();
        let damage = layer.commit_gesture(gesture).unwrap().unwrap();

        assert_eq!(layer.stats().before_images_recorded, 1);
        assert_eq!(layer.stats().snapshot_blocks, 2);
        assert_eq!(
            layer.stats().snapshot_bytes,
            2 * 16 * 16 * mem::size_of::<LinearRgba>() as u64
        );
        assert_eq!(layer.pixel(1, 1), Some(BLUE));
        assert_eq!(layer.pixel(20, 2), Some(RED));

        assert_eq!(layer.undo(), Some(damage.clone()));
        assert_eq!(layer.pixel(1, 1), Some(RED));
        assert_eq!(layer.pixel(20, 2), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.stats().history_swap_blocks, 2);
        assert_eq!(
            layer.stats().history_swap_bytes,
            2 * 16 * 16 * mem::size_of::<LinearRgba>() as u64
        );

        assert_eq!(layer.redo(), Some(damage));
        assert_eq!(layer.pixel(1, 1), Some(BLUE));
        assert_eq!(layer.pixel(20, 2), Some(RED));
        assert_eq!(layer.stats().history_swap_blocks, 4);
    }

    #[test]
    fn block_undo_swaps_new_tile_existence() {
        let mut layer =
            RasterLayer::new_with_undo_storage(128, 128, 128, UndoStorage::Blocks16).unwrap();
        let gesture = layer.begin_gesture().unwrap();
        layer.set_pixel(gesture, 4, 4, RED).unwrap();
        let damage = layer.commit_gesture(gesture).unwrap().unwrap();

        assert_eq!(layer.allocated_tile_count(), 1);
        assert_eq!(layer.stats().snapshot_blocks, 0);
        assert_eq!(layer.stats().snapshot_bytes, 0);

        assert_eq!(layer.undo(), Some(damage.clone()));
        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.pixel(4, 4), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.stats().history_swap_blocks, 0);
        assert_eq!(layer.stats().history_swap_bytes, 0);

        assert_eq!(layer.redo(), Some(damage));
        assert_eq!(layer.allocated_tile_count(), 1);
        assert_eq!(layer.pixel(4, 4), Some(RED));
    }

    #[test]
    fn block_cancel_restores_existing_and_new_tiles() {
        let mut layer =
            RasterLayer::new_with_undo_storage(256, 128, 128, UndoStorage::Blocks16).unwrap();
        let seed = layer.begin_gesture().unwrap();
        layer.set_pixel(seed, 4, 4, RED).unwrap();
        layer.commit_gesture(seed).unwrap();
        layer.clear_history();

        let gesture = layer.begin_gesture().unwrap();
        layer.set_pixel(gesture, 4, 4, BLUE).unwrap();
        layer.set_pixel(gesture, 200, 4, RED).unwrap();
        layer.cancel_gesture(gesture).unwrap();

        assert_eq!(layer.pixel(4, 4), Some(RED));
        assert_eq!(layer.pixel(200, 4), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.allocated_tile_count(), 1);
        assert_eq!(layer.undo_depth(), 0);
    }

    #[test]
    fn block_undo_restores_a_reclaimed_tile() {
        let mut layer =
            RasterLayer::new_with_undo_storage(128, 128, 128, UndoStorage::Blocks16).unwrap();
        let paint = layer.begin_gesture().unwrap();
        layer.set_pixel(paint, 4, 4, RED).unwrap();
        layer.commit_gesture(paint).unwrap();
        layer.clear_history();

        let erase = layer.begin_gesture().unwrap();
        layer
            .set_pixel(erase, 4, 4, LinearRgba::TRANSPARENT)
            .unwrap();
        let damage = layer.commit_gesture(erase).unwrap().unwrap();
        assert_eq!(layer.allocated_tile_count(), 0);

        assert_eq!(layer.undo(), Some(damage.clone()));
        assert_eq!(layer.pixel(4, 4), Some(RED));
        assert_eq!(layer.content_bounds(), Some(rect(4, 4, 5, 5)));

        assert_eq!(layer.redo(), Some(damage));
        assert_eq!(layer.pixel(4, 4), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.allocated_tile_count(), 0);
    }

    #[test]
    fn explicit_gesture_survives_updates_and_drains_incremental_damage() {
        let mut layer = layer();
        let gesture = layer.begin_gesture().unwrap();

        layer.set_pixel(gesture, 1, 1, RED).unwrap();
        let first_damage = layer.take_gesture_damage(gesture).unwrap();
        assert_eq!(first_damage.bounds(), Some(rect(1, 1, 2, 2)));
        assert_eq!(first_damage.tiles(), &[TileCoord::new(0, 0)]);

        assert!(layer.take_gesture_damage(gesture).unwrap().is_empty());
        layer.set_pixel(gesture, 10, 12, BLUE).unwrap();
        let second_damage = layer.take_gesture_damage(gesture).unwrap();
        assert_eq!(second_damage.bounds(), Some(rect(10, 12, 11, 13)));
        assert_eq!(layer.stats().before_images_recorded, 1);

        let committed = layer.commit_gesture(gesture).unwrap().unwrap();
        assert_eq!(committed.bounds(), Some(rect(1, 1, 11, 13)));
        assert_eq!(
            committed.tile_regions().collect::<Vec<_>>(),
            vec![(TileCoord::new(0, 0), rect(1, 1, 11, 13))]
        );
        assert_eq!(layer.active_gesture_id(), None);
        assert_eq!(layer.undo_depth(), 1);
    }

    #[test]
    fn cancel_restores_existing_tiles_and_removes_new_tiles() {
        let mut layer = layer();
        let mut seed = layer.scoped_gesture().unwrap();
        seed.set_pixel(4, 4, RED).unwrap();
        seed.commit().unwrap();

        let mut gesture = layer.scoped_gesture().unwrap();
        gesture.set_pixel(4, 4, BLUE).unwrap();
        gesture.set_pixel(200, 4, RED).unwrap();
        let damage = gesture.cancel().unwrap().unwrap();

        assert_eq!(damage.tiles().len(), 2);
        assert_eq!(layer.pixel(4, 4), Some(RED));
        assert_eq!(layer.pixel(200, 4), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.allocated_tile_count(), 1);
        assert_eq!(layer.undo_depth(), 1);
    }

    #[test]
    fn dropping_an_open_gesture_cancels_it() {
        let mut layer = layer();

        {
            let mut gesture = layer.scoped_gesture().unwrap();
            gesture.set_pixel(10, 10, RED).unwrap();
        }

        assert_eq!(layer.pixel(10, 10), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.undo_depth(), 0);
    }

    #[test]
    fn undo_and_redo_swap_exact_tile_states() {
        let mut layer = layer();
        let mut first = layer.scoped_gesture().unwrap();
        first.set_pixel(4, 4, RED).unwrap();
        first.commit().unwrap();

        let mut second = layer.scoped_gesture().unwrap();
        second.set_pixel(4, 4, BLUE).unwrap();
        second.set_pixel(200, 4, RED).unwrap();
        let committed_damage = second.commit().unwrap().unwrap();

        assert_eq!(layer.pixel(4, 4), Some(BLUE));
        assert_eq!(layer.pixel(200, 4), Some(RED));
        assert_eq!(layer.allocated_tile_count(), 2);

        assert_eq!(layer.undo(), Some(committed_damage.clone()));
        assert_eq!(layer.pixel(4, 4), Some(RED));
        assert_eq!(layer.pixel(200, 4), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.allocated_tile_count(), 1);
        assert_eq!(layer.undo_depth(), 1);
        assert_eq!(layer.redo_depth(), 1);

        assert_eq!(layer.redo(), Some(committed_damage));
        assert_eq!(layer.pixel(4, 4), Some(BLUE));
        assert_eq!(layer.pixel(200, 4), Some(RED));
        assert_eq!(layer.allocated_tile_count(), 2);
        assert_eq!(layer.undo_depth(), 2);
        assert_eq!(layer.redo_depth(), 0);
    }

    #[test]
    fn erasing_the_last_pixel_reclaims_the_tile_and_is_undoable() {
        let mut layer = layer();
        let mut paint = layer.scoped_gesture().unwrap();
        paint.set_pixel(4, 4, RED).unwrap();
        paint.commit().unwrap();

        let mut erase = layer.scoped_gesture().unwrap();
        erase.set_pixel(4, 4, LinearRgba::TRANSPARENT).unwrap();
        erase.commit().unwrap();

        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.content_bounds(), None);

        layer.undo();
        assert_eq!(layer.allocated_tile_count(), 1);
        assert_eq!(layer.pixel(4, 4), Some(RED));
        assert_eq!(layer.content_bounds(), Some(rect(4, 4, 5, 5)));
    }

    #[test]
    fn subtractive_edits_preserve_bounds_without_scanning_interior_removal() {
        let mut layer = RasterLayer::new(64, 64, 64).unwrap();
        let tile = TileCoord::new(0, 0);
        let painted = rect(10, 10, 50, 50);
        let paint = layer.begin_gesture().unwrap();
        layer
            .edit_tile_additive(paint, tile, painted, |tile| {
                for y in painted.min_y()..painted.max_y() {
                    tile.row_mut(y).unwrap()[painted.min_x() as usize..painted.max_x() as usize]
                        .fill(RED);
                }
                ((), Some(painted))
            })
            .unwrap();
        layer.commit_gesture(paint).unwrap();
        layer.reset_stats();

        let erased = rect(20, 20, 30, 30);
        let erase = layer.begin_gesture().unwrap();
        layer
            .edit_tile_subtractive(erase, tile, erased, |tile| {
                for y in erased.min_y()..erased.max_y() {
                    tile.row_mut(y).unwrap()[erased.min_x() as usize..erased.max_x() as usize]
                        .fill(LinearRgba::TRANSPARENT);
                }
                ((), Some(erased))
            })
            .unwrap();
        layer.commit_gesture(erase).unwrap();

        assert_eq!(layer.content_bounds(), Some(painted));
        assert_eq!(layer.stats().content_bound_pixels_scanned, 0);
    }

    #[test]
    fn subtractive_edge_removal_shrinks_bounds_with_a_boundary_scan() {
        let mut layer = RasterLayer::new(64, 64, 64).unwrap();
        let tile = TileCoord::new(0, 0);
        let painted = rect(10, 10, 50, 50);
        let paint = layer.begin_gesture().unwrap();
        layer
            .edit_tile_additive(paint, tile, painted, |tile| {
                for y in painted.min_y()..painted.max_y() {
                    tile.row_mut(y).unwrap()[painted.min_x() as usize..painted.max_x() as usize]
                        .fill(RED);
                }
                ((), Some(painted))
            })
            .unwrap();
        layer.commit_gesture(paint).unwrap();
        layer.reset_stats();

        let erased = rect(10, 10, 50, 20);
        let erase = layer.begin_gesture().unwrap();
        layer
            .edit_tile_subtractive(erase, tile, erased, |tile| {
                for y in erased.min_y()..erased.max_y() {
                    tile.row_mut(y).unwrap()[erased.min_x() as usize..erased.max_x() as usize]
                        .fill(LinearRgba::TRANSPARENT);
                }
                ((), Some(erased))
            })
            .unwrap();
        layer.commit_gesture(erase).unwrap();

        assert_eq!(layer.content_bounds(), Some(rect(10, 20, 50, 50)));
        assert!(layer.stats().content_bound_pixels_scanned > 0);
        assert!(layer.stats().content_bound_pixels_scanned < 64 * 64);
    }

    #[test]
    fn invalid_edits_do_not_allocate_or_create_history() {
        let mut layer = layer();
        let mut gesture = layer.scoped_gesture().unwrap();

        assert_eq!(
            gesture.set_pixel(300, 0, RED),
            Err(RasterError::PixelOutOfBounds { x: 300, y: 0 })
        );
        assert!(matches!(
            gesture.edit_tile(TileCoord::new(2, 1), rect(0, 0, 45, 72), |_| {}),
            Err(RasterError::DamageOutsideTile { .. })
        ));
        assert!(matches!(
            gesture.edit_tile(TileCoord::new(3, 0), rect(0, 0, 1, 1), |_| {}),
            Err(RasterError::TileOutOfBounds(TileCoord { x: 3, y: 0 }))
        ));
        assert_eq!(gesture.commit().unwrap(), None);

        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.undo_depth(), 0);
    }

    #[test]
    fn content_bounds_cover_nonempty_pixels_across_tiles() {
        let mut layer = layer();
        let mut gesture = layer.scoped_gesture().unwrap();
        gesture.set_pixel(20, 30, RED).unwrap();
        gesture.set_pixel(280, 190, BLUE).unwrap();
        gesture.commit().unwrap();

        assert_eq!(layer.content_bounds(), Some(rect(20, 30, 281, 191)));
    }
}
