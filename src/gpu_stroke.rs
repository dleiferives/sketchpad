use crate::{
    brush::{BrushError, BrushSample},
    contact::{for_each_subdivided_sweep, BladePose, BladeSweep},
    natural::{contact_direction_from_tilt, PaletteKnifeBrush},
    raster::{Damage, LinearRgba, RasterError, RasterLayer, RectU32, TileCoord},
};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt, mem,
};

const MAXIMUM_BLADE_ANGLE_RADIANS: f32 = 7.5_f32.to_radians();
const MAXIMUM_BLADE_SUBDIVISIONS: usize = 24;
const ORIENTATION_MOVE_DEAD_ZONE: f32 = 0.25;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuStrokeVertex {
    pub position: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrokeTileDamage {
    pub coord: TileCoord,
    pub local_damage: RectU32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuStrokeBatch {
    vertices: Vec<GpuStrokeVertex>,
    touched_tiles: Vec<StrokeTileDamage>,
    sweeps: u32,
}

impl GpuStrokeBatch {
    pub fn vertices(&self) -> &[GpuStrokeVertex] {
        &self.vertices
    }

    pub fn touched_tiles(&self) -> &[StrokeTileDamage] {
        &self.touched_tiles
    }

    pub const fn sweeps(&self) -> u32 {
        self.sweeps
    }

    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }
}

pub struct ContinuousBladeStroke {
    brush: PaletteKnifeBrush,
    canvas: [u32; 2],
    tile_size: u32,
    direction: [f32; 2],
    last_position: [f32; 2],
    previous_pose: Option<BladePose>,
    pending_vertices: Vec<GpuStrokeVertex>,
    pending_damage: HashMap<TileCoord, RectU32>,
    pending_sweeps: u32,
    total_sweeps: u64,
    finalized: bool,
}

impl ContinuousBladeStroke {
    pub fn begin(
        brush: PaletteKnifeBrush,
        canvas: [u32; 2],
        tile_size: u32,
        sample: BrushSample,
    ) -> Result<Self, ContinuousBladeError> {
        if canvas[0] == 0 || canvas[1] == 0 {
            return Err(ContinuousBladeError::EmptyCanvas);
        }
        if tile_size == 0 {
            return Err(ContinuousBladeError::InvalidTileSize);
        }
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample.into());
        }
        let mut stroke = Self {
            brush,
            canvas,
            tile_size,
            direction: [1.0, 0.0],
            last_position: sample.position,
            previous_pose: None,
            pending_vertices: Vec::new(),
            pending_damage: HashMap::new(),
            pending_sweeps: 0,
            total_sweeps: 0,
            finalized: false,
        };
        stroke.add_sample(sample)?;
        Ok(stroke)
    }

    pub const fn diameter(&self) -> f32 {
        self.brush.diameter()
    }

    pub fn color(&self) -> [f32; 4] {
        let opacity = self.brush.opacity();
        let color = self.brush.color();
        [
            color[0] * opacity,
            color[1] * opacity,
            color[2] * opacity,
            opacity,
        ]
    }

    pub const fn total_sweeps(&self) -> u64 {
        self.total_sweeps
    }

    pub fn update(&mut self, sample: BrushSample) -> Result<(), ContinuousBladeError> {
        if self.finalized {
            return Err(BrushError::StrokeFinalized.into());
        }
        if !sample.is_finite() {
            return Err(BrushError::InvalidSample.into());
        }
        self.add_sample(sample)
    }

    pub fn finish(&mut self) -> Result<(), ContinuousBladeError> {
        if self.finalized {
            return Err(BrushError::StrokeFinalized.into());
        }
        self.finalized = true;
        Ok(())
    }

    pub fn take_batch(&mut self) -> GpuStrokeBatch {
        let mut touched_tiles: Vec<_> = self
            .pending_damage
            .drain()
            .map(|(coord, local_damage)| StrokeTileDamage {
                coord,
                local_damage,
            })
            .collect();
        touched_tiles.sort_by_key(|tile| (tile.coord.y, tile.coord.x));
        GpuStrokeBatch {
            vertices: mem::take(&mut self.pending_vertices),
            touched_tiles,
            sweeps: mem::take(&mut self.pending_sweeps),
        }
    }

    fn add_sample(&mut self, sample: BrushSample) -> Result<(), ContinuousBladeError> {
        self.resolve_direction(sample);
        let pressure = sample.pressure.clamp(0.0, 1.0);
        if pressure == 0.0 || self.brush.opacity() == 0.0 {
            self.previous_pose = None;
            return Ok(());
        }
        let current = BladePose::new(
            sample.position,
            self.direction,
            self.brush.contact_half_extents(pressure),
        )
        .ok_or(ContinuousBladeError::InvalidPose)?;
        if let Some(previous) = self.previous_pose {
            let sweeps_before = self.pending_sweeps;
            let count = for_each_subdivided_sweep(
                previous,
                current,
                MAXIMUM_BLADE_ANGLE_RADIANS,
                MAXIMUM_BLADE_SUBDIVISIONS,
                |sweep| self.append_sweep(sweep),
            );
            debug_assert_eq!(self.pending_sweeps - sweeps_before, count as u32);
        } else {
            self.append_sweep(BladeSweep::between(current, current));
        }
        self.previous_pose = Some(current);
        Ok(())
    }

    fn resolve_direction(&mut self, sample: BrushSample) {
        if let Some(direction) = contact_direction_from_tilt(sample.tilt) {
            self.direction = direction;
        } else {
            let dx = sample.position[0] - self.last_position[0];
            let dy = sample.position[1] - self.last_position[1];
            let length_squared = dx * dx + dy * dy;
            if length_squared >= ORIENTATION_MOVE_DEAD_ZONE * ORIENTATION_MOVE_DEAD_ZONE {
                let inverse_length = length_squared.sqrt().recip();
                self.direction = [dx * inverse_length, dy * inverse_length];
            }
        }
        self.last_position = sample.position;
    }

    fn append_sweep(&mut self, sweep: BladeSweep) {
        let polygon = sweep.polygon();
        let vertices = polygon.vertices();
        let first = vertices[0];
        for index in 1..vertices.len() - 1 {
            self.pending_vertices.extend([
                GpuStrokeVertex { position: first },
                GpuStrokeVertex {
                    position: vertices[index],
                },
                GpuStrokeVertex {
                    position: vertices[index + 1],
                },
            ]);
        }
        self.pending_sweeps += 1;
        self.total_sweeps += 1;
        self.include_bounds(polygon.bounds());
    }

    fn include_bounds(&mut self, bounds: [f32; 4]) {
        let min_x = bounds[0].floor().clamp(0.0, self.canvas[0] as f32) as u32;
        let min_y = bounds[1].floor().clamp(0.0, self.canvas[1] as f32) as u32;
        let max_x = bounds[2].ceil().clamp(0.0, self.canvas[0] as f32) as u32;
        let max_y = bounds[3].ceil().clamp(0.0, self.canvas[1] as f32) as u32;
        if min_x >= max_x || min_y >= max_y {
            return;
        }
        let min_tile_x = min_x / self.tile_size;
        let min_tile_y = min_y / self.tile_size;
        let max_tile_x = (max_x - 1) / self.tile_size;
        let max_tile_y = (max_y - 1) / self.tile_size;
        for tile_y in min_tile_y..=max_tile_y {
            for tile_x in min_tile_x..=max_tile_x {
                let origin_x = tile_x * self.tile_size;
                let origin_y = tile_y * self.tile_size;
                let local = RectU32::from_min_max(
                    min_x.max(origin_x) - origin_x,
                    min_y.max(origin_y) - origin_y,
                    max_x.min(origin_x + self.tile_size) - origin_x,
                    max_y.min(origin_y + self.tile_size) - origin_y,
                )
                .expect("every enumerated tile intersects the clipped sweep bounds");
                self.pending_damage
                    .entry(TileCoord::new(tile_x, tile_y))
                    .and_modify(|damage| *damage = damage.union(local))
                    .or_insert(local);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContinuousBladeError {
    EmptyCanvas,
    InvalidTileSize,
    InvalidPose,
    Brush(BrushError),
}

impl fmt::Display for ContinuousBladeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCanvas => write!(formatter, "continuous blade canvas is empty"),
            Self::InvalidTileSize => write!(formatter, "continuous blade tile size is zero"),
            Self::InvalidPose => write!(formatter, "continuous blade generated an invalid pose"),
            Self::Brush(error) => error.fmt(formatter),
        }
    }
}

impl Error for ContinuousBladeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Brush(error) => Some(error),
            _ => None,
        }
    }
}

impl From<BrushError> for ContinuousBladeError {
    fn from(error: BrushError) -> Self {
        Self::Brush(error)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceOverTile {
    coord: TileCoord,
    local_damage: RectU32,
    pixels: Box<[LinearRgba]>,
}

impl SourceOverTile {
    pub fn new(coord: TileCoord, local_damage: RectU32, pixels: Box<[LinearRgba]>) -> Self {
        Self {
            coord,
            local_damage,
            pixels,
        }
    }

    pub const fn coord(&self) -> TileCoord {
        self.coord
    }

    pub const fn local_damage(&self) -> RectU32 {
        self.local_damage
    }

    pub fn pixels(&self) -> &[LinearRgba] {
        &self.pixels
    }
}

pub fn commit_source_over_tiles(
    layer: &mut RasterLayer,
    brush_diameter: f32,
    tiles: &[SourceOverTile],
) -> Result<Option<Damage>, GpuStrokeCommitError> {
    if !brush_diameter.is_finite() || brush_diameter <= 0.0 {
        return Err(GpuStrokeCommitError::InvalidBrushDiameter(brush_diameter));
    }
    let tile_size = layer.tile_size();
    let expected_pixels = (tile_size as usize)
        .checked_mul(tile_size as usize)
        .ok_or(GpuStrokeCommitError::TilePixelCountOverflow)?;
    let mut seen = HashSet::with_capacity(tiles.len());

    for tile in tiles {
        if !seen.insert(tile.coord) {
            return Err(GpuStrokeCommitError::DuplicateTile(tile.coord));
        }
        let bounds = layer
            .tile_bounds(tile.coord)
            .ok_or(RasterError::TileOutOfBounds(tile.coord))?;
        if tile.local_damage.max_x() > bounds.width() || tile.local_damage.max_y() > bounds.height()
        {
            return Err(RasterError::DamageOutsideTile {
                tile: tile.coord,
                damage: tile.local_damage,
                valid_width: bounds.width(),
                valid_height: bounds.height(),
            }
            .into());
        }
        if tile.pixels.len() != expected_pixels {
            return Err(GpuStrokeCommitError::InvalidTilePixelCount {
                tile: tile.coord,
                expected: expected_pixels,
                actual: tile.pixels.len(),
            });
        }
        for (index, pixel) in tile.pixels.iter().copied().enumerate() {
            if !valid_premultiplied_pixel(pixel) {
                return Err(GpuStrokeCommitError::InvalidPixel {
                    tile: tile.coord,
                    index,
                    pixel,
                });
            }
        }
    }

    let mut changed = Vec::with_capacity(tiles.len());
    for (index, tile) in tiles.iter().enumerate() {
        if let Some(bounds) = changed_bounds(layer, tile) {
            changed.push((index, bounds));
        }
    }
    if changed.is_empty() {
        return Ok(None);
    }

    let gesture = layer.begin_brush_gesture(brush_diameter)?;
    for (index, changed_bounds) in changed {
        let tile = &tiles[index];
        let edit = layer.edit_tile_additive(gesture, tile.coord, changed_bounds, |destination| {
            let stride = destination.stride();
            let pixels = destination.pixels_mut();
            for y in changed_bounds.min_y()..changed_bounds.max_y() {
                let row = y as usize * stride;
                for x in changed_bounds.min_x()..changed_bounds.max_x() {
                    let pixel_index = row + x as usize;
                    let source = tile.pixels[pixel_index];
                    if source.a != 0.0 {
                        pixels[pixel_index] = source_over(source, pixels[pixel_index]);
                    }
                }
            }
            ((), Some(changed_bounds))
        });
        if let Err(error) = edit {
            let _ = layer.cancel_gesture(gesture);
            return Err(error.into());
        }
    }
    layer.commit_gesture(gesture).map_err(Into::into)
}

fn changed_bounds(layer: &RasterLayer, tile: &SourceOverTile) -> Option<RectU32> {
    let destination = layer.tile(tile.coord);
    let stride = layer.tile_size() as usize;
    let mut bounds = None;
    for y in tile.local_damage.min_y()..tile.local_damage.max_y() {
        let row = y as usize * stride;
        for x in tile.local_damage.min_x()..tile.local_damage.max_x() {
            let index = row + x as usize;
            let source = tile.pixels[index];
            if source.a == 0.0 {
                continue;
            }
            let previous = destination
                .as_ref()
                .map(|destination| destination.pixels()[index])
                .unwrap_or(LinearRgba::TRANSPARENT);
            if source_over(source, previous) != previous {
                include_pixel(&mut bounds, x, y);
            }
        }
    }
    bounds
}

fn source_over(source: LinearRgba, destination: LinearRgba) -> LinearRgba {
    let keep_destination = 1.0 - source.a;
    LinearRgba::premultiplied(
        source.r + destination.r * keep_destination,
        source.g + destination.g * keep_destination,
        source.b + destination.b * keep_destination,
        source.a + destination.a * keep_destination,
    )
}

fn valid_premultiplied_pixel(pixel: LinearRgba) -> bool {
    [pixel.r, pixel.g, pixel.b, pixel.a]
        .into_iter()
        .all(f32::is_finite)
        && pixel.r >= 0.0
        && pixel.g >= 0.0
        && pixel.b >= 0.0
        && (0.0..=1.0).contains(&pixel.a)
        && (pixel.a != 0.0 || pixel == LinearRgba::TRANSPARENT)
}

fn include_pixel(bounds: &mut Option<RectU32>, x: u32, y: u32) {
    let pixel = RectU32::from_xywh(x, y, 1, 1).expect("one pixel is nonempty");
    *bounds = Some(bounds.map_or(pixel, |bounds| bounds.union(pixel)));
}

#[derive(Clone, Debug, PartialEq)]
pub enum GpuStrokeCommitError {
    InvalidBrushDiameter(f32),
    TilePixelCountOverflow,
    DuplicateTile(TileCoord),
    InvalidTilePixelCount {
        tile: TileCoord,
        expected: usize,
        actual: usize,
    },
    InvalidPixel {
        tile: TileCoord,
        index: usize,
        pixel: LinearRgba,
    },
    Raster(RasterError),
}

impl fmt::Display for GpuStrokeCommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBrushDiameter(diameter) => {
                write!(formatter, "invalid GPU stroke brush diameter {diameter}")
            }
            Self::TilePixelCountOverflow => {
                write!(formatter, "GPU stroke tile pixel count overflows usize")
            }
            Self::DuplicateTile(tile) => {
                write!(
                    formatter,
                    "GPU stroke contains duplicate tile ({}, {})",
                    tile.x, tile.y
                )
            }
            Self::InvalidTilePixelCount {
                tile,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU stroke tile ({}, {}) has {actual} pixels, expected {expected}",
                tile.x, tile.y
            ),
            Self::InvalidPixel { tile, index, pixel } => write!(
                formatter,
                "GPU stroke tile ({}, {}) has invalid pixel {pixel:?} at index {index}",
                tile.x, tile.y
            ),
            Self::Raster(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuStrokeCommitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Raster(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RasterError> for GpuStrokeCommitError {
    fn from(error: RasterError) -> Self {
        Self::Raster(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer() -> RasterLayer {
        RasterLayer::new(8, 8, 4).unwrap()
    }

    fn tile_with_pixel(coord: TileCoord, x: u32, y: u32, pixel: LinearRgba) -> SourceOverTile {
        let mut pixels = vec![LinearRgba::TRANSPARENT; 16];
        pixels[y as usize * 4 + x as usize] = pixel;
        SourceOverTile::new(
            coord,
            RectU32::from_xywh(x, y, 1, 1).unwrap(),
            pixels.into_boxed_slice(),
        )
    }

    fn blade() -> PaletteKnifeBrush {
        PaletteKnifeBrush::new([0.2, 0.1, 0.4], 72.0, 1.0).unwrap()
    }

    fn blade_samples() -> [BrushSample; 5] {
        [
            BrushSample::with_tilt([30.0, 80.0], 0.3, [0.0, 0.8]),
            BrushSample::with_tilt([70.0, 92.0], 0.5, [0.3, 0.7]),
            BrushSample::with_tilt([110.0, 74.0], 0.8, [0.7, 0.3]),
            BrushSample::with_tilt([150.0, 100.0], 1.0, [0.8, 0.0]),
            BrushSample::with_tilt([210.0, 86.0], 0.6, [0.4, -0.6]),
        ]
    }

    #[test]
    fn opaque_readback_commits_as_one_undoable_gesture() {
        let mut layer = layer();
        let red = LinearRgba::from_straight(0.8, 0.1, 0.05, 1.0);
        let damage = commit_source_over_tiles(
            &mut layer,
            512.0,
            &[
                tile_with_pixel(TileCoord::new(0, 0), 2, 3, red),
                tile_with_pixel(TileCoord::new(1, 0), 1, 2, red),
            ],
        )
        .unwrap()
        .unwrap();

        assert_eq!(layer.pixel(2, 3), Some(red));
        assert_eq!(layer.pixel(5, 2), Some(red));
        assert_eq!(damage.tiles().len(), 2);
        assert_eq!(layer.undo_depth(), 1);
        layer.undo().unwrap();
        assert_eq!(layer.pixel(2, 3), Some(LinearRgba::TRANSPARENT));
        assert_eq!(layer.pixel(5, 2), Some(LinearRgba::TRANSPARENT));
    }

    #[test]
    fn translucent_readback_uses_premultiplied_source_over() {
        let mut layer = layer();
        let blue = LinearRgba::from_straight(0.0, 0.0, 0.8, 1.0);
        let gesture = layer.begin_gesture().unwrap();
        layer.set_pixel(gesture, 1, 1, blue).unwrap();
        layer.commit_gesture(gesture).unwrap();
        layer.clear_history();

        let red = LinearRgba::from_straight(0.8, 0.0, 0.0, 0.25);
        commit_source_over_tiles(
            &mut layer,
            48.0,
            &[tile_with_pixel(TileCoord::new(0, 0), 1, 1, red)],
        )
        .unwrap();
        assert_eq!(
            layer.pixel(1, 1),
            Some(LinearRgba::premultiplied(0.2, 0.0, 0.6, 1.0))
        );
        layer.undo().unwrap();
        assert_eq!(layer.pixel(1, 1), Some(blue));
    }

    #[test]
    fn transparent_or_exact_overlay_is_a_noop() {
        let mut layer = layer();
        let transparent = tile_with_pixel(TileCoord::new(0, 0), 1, 1, LinearRgba::TRANSPARENT);
        assert_eq!(
            commit_source_over_tiles(&mut layer, 16.0, &[transparent]).unwrap(),
            None
        );
        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.undo_depth(), 0);
    }

    #[test]
    fn malformed_readback_is_rejected_before_layer_mutation() {
        let mut layer = layer();
        let coord = TileCoord::new(0, 0);
        let damage = RectU32::from_xywh(0, 0, 1, 1).unwrap();
        let invalid_length = SourceOverTile::new(
            coord,
            damage,
            vec![LinearRgba::TRANSPARENT; 15].into_boxed_slice(),
        );
        assert!(matches!(
            commit_source_over_tiles(&mut layer, 32.0, &[invalid_length]),
            Err(GpuStrokeCommitError::InvalidTilePixelCount { .. })
        ));

        let invalid_pixel = tile_with_pixel(
            coord,
            0,
            0,
            LinearRgba::premultiplied(f32::NAN, 0.0, 0.0, 1.0),
        );
        assert!(matches!(
            commit_source_over_tiles(&mut layer, 32.0, &[invalid_pixel]),
            Err(GpuStrokeCommitError::InvalidPixel { .. })
        ));

        let valid = tile_with_pixel(coord, 0, 0, LinearRgba::from_straight(0.2, 0.3, 0.4, 1.0));
        assert!(matches!(
            commit_source_over_tiles(&mut layer, 32.0, &[valid.clone(), valid]),
            Err(GpuStrokeCommitError::DuplicateTile(_))
        ));
        assert_eq!(layer.allocated_tile_count(), 0);
        assert_eq!(layer.undo_depth(), 0);
    }

    #[test]
    fn continuous_blade_batches_do_not_change_generated_geometry() {
        let samples = blade_samples();
        let mut whole = ContinuousBladeStroke::begin(blade(), [256, 192], 64, samples[0]).unwrap();
        for sample in &samples[1..] {
            whole.update(*sample).unwrap();
        }
        whole.finish().unwrap();
        let whole = whole.take_batch();

        let mut incremental =
            ContinuousBladeStroke::begin(blade(), [256, 192], 64, samples[0]).unwrap();
        let mut vertices = Vec::new();
        let mut sweeps = 0;
        let mut damage = HashMap::new();
        let mut collect = |batch: GpuStrokeBatch| {
            vertices.extend_from_slice(batch.vertices());
            sweeps += batch.sweeps();
            for tile in batch.touched_tiles() {
                damage
                    .entry(tile.coord)
                    .and_modify(|current: &mut RectU32| *current = current.union(tile.local_damage))
                    .or_insert(tile.local_damage);
            }
        };
        collect(incremental.take_batch());
        for sample in &samples[1..] {
            incremental.update(*sample).unwrap();
            collect(incremental.take_batch());
        }
        incremental.finish().unwrap();
        collect(incremental.take_batch());
        let mut touched_tiles: Vec<_> = damage
            .into_iter()
            .map(|(coord, local_damage)| StrokeTileDamage {
                coord,
                local_damage,
            })
            .collect();
        touched_tiles.sort_by_key(|tile| (tile.coord.y, tile.coord.x));

        assert_eq!(vertices, whole.vertices());
        assert_eq!(sweeps, whole.sweeps());
        assert_eq!(touched_tiles, whole.touched_tiles());
    }

    #[test]
    fn zero_pressure_breaks_contact_without_emitting_geometry() {
        let zero = BrushSample::with_tilt([32.0, 32.0], 0.0, [0.0, 0.8]);
        let mut stroke = ContinuousBladeStroke::begin(blade(), [128, 128], 64, zero).unwrap();
        assert!(stroke.take_batch().is_empty());

        stroke
            .update(BrushSample::with_tilt([48.0, 48.0], 1.0, [0.0, 0.8]))
            .unwrap();
        let first_contact = stroke.take_batch();
        assert_eq!(first_contact.sweeps(), 1);
        assert!(!first_contact.is_empty());

        stroke.update(zero).unwrap();
        assert!(stroke.take_batch().is_empty());
    }

    #[test]
    fn continuous_blade_damage_is_clipped_to_edge_tiles() {
        let sample = BrushSample::with_tilt([2.0, 2.0], 1.0, [0.0, 0.8]);
        let mut stroke = ContinuousBladeStroke::begin(blade(), [130, 130], 64, sample).unwrap();
        let batch = stroke.take_batch();
        assert!(!batch.is_empty());
        for tile in batch.touched_tiles() {
            let valid_width = 64.min(130 - tile.coord.x * 64);
            let valid_height = 64.min(130 - tile.coord.y * 64);
            assert!(tile.local_damage.max_x() <= valid_width);
            assert!(tile.local_damage.max_y() <= valid_height);
        }
    }
}
