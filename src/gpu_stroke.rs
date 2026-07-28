use crate::raster::{Damage, LinearRgba, RasterError, RasterLayer, RectU32, TileCoord};
use std::{collections::HashSet, error::Error, fmt};

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
}
