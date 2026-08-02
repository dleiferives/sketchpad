use crate::{
    document::LayerId,
    raster::{LinearRgba, RasterError, RasterLayer, TileCoord},
};
use std::{error::Error, fmt, mem::size_of, sync::Arc};

#[derive(Clone, Debug, PartialEq)]
pub struct GpuExactLayerRecoveryCommand {
    layer: LayerId,
    width: u32,
    height: u32,
    tile_size: u32,
    tiles: Box<[GpuExactLayerRecoveryTile]>,
    retained_byte_len: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct GpuExactLayerRecoveryTile {
    coord: TileCoord,
    pixels: Arc<[LinearRgba]>,
}

impl GpuExactLayerRecoveryCommand {
    pub fn from_raster(
        layer: LayerId,
        raster: &RasterLayer,
    ) -> Result<Self, GpuExactLayerRecoveryBuildError> {
        if raster.active_gesture_id().is_some() {
            return Err(GpuExactLayerRecoveryBuildError::GestureActive);
        }
        let mut source_tiles = raster.checkpoint_tiles();
        source_tiles.sort_unstable_by_key(|tile| (tile.coord.y, tile.coord.x));
        for tile in &source_tiles {
            if let Some((index, _)) = tile
                .pixels
                .iter()
                .enumerate()
                .find(|(_, pixel)| !pixel_is_finite(**pixel))
            {
                return Err(GpuExactLayerRecoveryBuildError::NonFinitePixel {
                    tile: tile.coord,
                    index,
                });
            }
        }

        let tile_count = source_tiles.len();
        let tile_pixel_count = usize::try_from(
            raster
                .tile_size()
                .checked_mul(raster.tile_size())
                .ok_or(GpuExactLayerRecoveryBuildError::ByteCountOverflow)?,
        )
        .map_err(|_| GpuExactLayerRecoveryBuildError::ByteCountOverflow)?;
        let retained_byte_len = retained_byte_len(tile_count, tile_pixel_count)
            .ok_or(GpuExactLayerRecoveryBuildError::ByteCountOverflow)?;
        let tiles = source_tiles
            .into_iter()
            .map(|tile| GpuExactLayerRecoveryTile {
                coord: tile.coord,
                pixels: tile.pixels,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Ok(Self {
            layer,
            width: raster.width(),
            height: raster.height(),
            tile_size: raster.tile_size(),
            tiles,
            retained_byte_len,
        })
    }

    pub const fn layer(&self) -> LayerId {
        self.layer
    }

    pub const fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub const fn tile_size(&self) -> u32 {
        self.tile_size
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    pub const fn retained_byte_len(&self) -> u64 {
        self.retained_byte_len
    }

    pub fn raster_layer(&self) -> Result<RasterLayer, RasterError> {
        let mut raster = RasterLayer::new(self.width, self.height, self.tile_size)?;
        for tile in &self.tiles {
            raster.restore_shared_tile(tile.coord, Arc::clone(&tile.pixels))?;
        }
        Ok(raster)
    }
}

pub fn replay_exact_layer_recovery_command(
    layer_id: LayerId,
    layer: &mut RasterLayer,
    command: &GpuExactLayerRecoveryCommand,
) -> Result<GpuExactLayerRecoveryReplay, GpuExactLayerRecoveryReplayError> {
    if layer_id != command.layer {
        return Err(GpuExactLayerRecoveryReplayError::LayerMismatch {
            expected: command.layer,
            actual: layer_id,
        });
    }
    let actual_dimensions = [layer.width(), layer.height()];
    if actual_dimensions != command.dimensions() || layer.tile_size() != command.tile_size {
        return Err(GpuExactLayerRecoveryReplayError::GeometryMismatch {
            expected_dimensions: command.dimensions(),
            actual_dimensions,
            expected_tile_size: command.tile_size,
            actual_tile_size: layer.tile_size(),
        });
    }

    let previous_tiles_discarded = u32::try_from(layer.allocated_tile_count())
        .map_err(|_| GpuExactLayerRecoveryReplayError::TileCountOverflow)?;
    let tiles_installed = u32::try_from(command.tiles.len())
        .map_err(|_| GpuExactLayerRecoveryReplayError::TileCountOverflow)?;
    let replacement = command.raster_layer()?;
    *layer = replacement;
    Ok(GpuExactLayerRecoveryReplay {
        tiles_installed,
        previous_tiles_discarded,
    })
}

fn retained_byte_len(tile_count: usize, tile_pixel_count: usize) -> Option<u64> {
    let tile_count = u64::try_from(tile_count).ok()?;
    let tile_bytes = tile_count.checked_mul(size_of::<GpuExactLayerRecoveryTile>() as u64)?;
    let pixel_bytes = tile_count
        .checked_mul(u64::try_from(tile_pixel_count).ok()?)?
        .checked_mul(size_of::<LinearRgba>() as u64)?;
    (size_of::<GpuExactLayerRecoveryCommand>() as u64)
        .checked_add(tile_bytes)?
        .checked_add(pixel_bytes)
}

fn pixel_is_finite(pixel: LinearRgba) -> bool {
    pixel.r.is_finite() && pixel.g.is_finite() && pixel.b.is_finite() && pixel.a.is_finite()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuExactLayerRecoveryReplay {
    pub tiles_installed: u32,
    pub previous_tiles_discarded: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuExactLayerRecoveryBuildError {
    GestureActive,
    NonFinitePixel { tile: TileCoord, index: usize },
    ByteCountOverflow,
}

impl fmt::Display for GpuExactLayerRecoveryBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GestureActive => {
                write!(formatter, "cannot capture a GPU recovery layer mid-gesture")
            }
            Self::NonFinitePixel { tile, index } => write!(
                formatter,
                "GPU exact layer recovery tile {tile:?} has a non-finite pixel at index {index}"
            ),
            Self::ByteCountOverflow => {
                write!(formatter, "GPU exact layer recovery byte count overflows")
            }
        }
    }
}

impl Error for GpuExactLayerRecoveryBuildError {}

#[derive(Debug)]
pub enum GpuExactLayerRecoveryReplayError {
    LayerMismatch {
        expected: LayerId,
        actual: LayerId,
    },
    GeometryMismatch {
        expected_dimensions: [u32; 2],
        actual_dimensions: [u32; 2],
        expected_tile_size: u32,
        actual_tile_size: u32,
    },
    TileCountOverflow,
    Raster(RasterError),
}

impl fmt::Display for GpuExactLayerRecoveryReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LayerMismatch { expected, actual } => write!(
                formatter,
                "GPU exact layer recovery targets layer {}, not layer {}",
                expected.get(),
                actual.get()
            ),
            Self::GeometryMismatch {
                expected_dimensions,
                actual_dimensions,
                expected_tile_size,
                actual_tile_size,
            } => write!(
                formatter,
                "GPU exact layer recovery geometry {}x{} / tile {} does not match {}x{} / tile {}",
                expected_dimensions[0],
                expected_dimensions[1],
                expected_tile_size,
                actual_dimensions[0],
                actual_dimensions[1],
                actual_tile_size
            ),
            Self::TileCountOverflow => {
                write!(
                    formatter,
                    "GPU exact layer recovery tile count overflows u32"
                )
            }
            Self::Raster(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuExactLayerRecoveryReplayError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Raster(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RasterError> for GpuExactLayerRecoveryReplayError {
    fn from(error: RasterError) -> Self {
        Self::Raster(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: LinearRgba = LinearRgba::premultiplied(1.0, 0.0, 0.0, 1.0);
    const BLUE: LinearRgba = LinearRgba::premultiplied(0.0, 0.0, 1.0, 1.0);

    fn paint(layer: &mut RasterLayer, x: u32, y: u32, color: LinearRgba) {
        let gesture = layer.begin_gesture().unwrap();
        layer.set_pixel(gesture, x, y, color).unwrap();
        layer.commit_gesture(gesture).unwrap();
    }

    #[test]
    fn capture_is_canonical_shared_and_immutable() {
        let layer_id = LayerId::from_raw(7);
        let mut source = RasterLayer::new(32, 32, 16).unwrap();
        paint(&mut source, 20, 4, RED);
        paint(&mut source, 4, 20, BLUE);
        source.clear_history();

        let command = GpuExactLayerRecoveryCommand::from_raster(layer_id, &source).unwrap();
        assert_eq!(command.layer(), layer_id);
        assert_eq!(command.dimensions(), [32, 32]);
        assert_eq!(command.tile_size(), 16);
        assert_eq!(command.tile_count(), 2);
        assert_eq!(
            command
                .tiles
                .iter()
                .map(|tile| tile.coord)
                .collect::<Vec<_>>(),
            vec![TileCoord::new(1, 0), TileCoord::new(0, 1)]
        );

        let source_tiles = source.checkpoint_tiles();
        for command_tile in command.tiles.iter() {
            let source_tile = source_tiles
                .iter()
                .find(|tile| tile.coord == command_tile.coord)
                .unwrap();
            assert!(Arc::ptr_eq(&command_tile.pixels, &source_tile.pixels));
        }

        paint(&mut source, 20, 4, BLUE);
        let recovered = command.raster_layer().unwrap();
        assert_eq!(recovered.pixel(20, 4), Some(RED));
        assert_eq!(source.pixel(20, 4), Some(BLUE));
        let recovered_tiles = recovered.checkpoint_tiles();
        for command_tile in command.tiles.iter() {
            let recovered_tile = recovered_tiles
                .iter()
                .find(|tile| tile.coord == command_tile.coord)
                .unwrap();
            assert!(Arc::ptr_eq(&command_tile.pixels, &recovered_tile.pixels));
        }
    }

    #[test]
    fn replay_replaces_the_entire_sparse_layer_transactionally() {
        let layer_id = LayerId::from_raw(3);
        let empty = RasterLayer::new(32, 32, 16).unwrap();
        let command = GpuExactLayerRecoveryCommand::from_raster(layer_id, &empty).unwrap();
        let mut target = RasterLayer::new(32, 32, 16).unwrap();
        paint(&mut target, 4, 4, RED);

        let replay = replay_exact_layer_recovery_command(layer_id, &mut target, &command).unwrap();

        assert_eq!(replay.tiles_installed, 0);
        assert_eq!(replay.previous_tiles_discarded, 1);
        assert_eq!(target.allocated_tile_count(), 0);
        assert_eq!(target.undo_depth(), 0);
    }

    #[test]
    fn capture_rejects_nonfinite_pixels_and_replay_rejects_geometry() {
        let layer_id = LayerId::from_raw(1);
        let mut active = RasterLayer::new(16, 16, 16).unwrap();
        let active_gesture = active.begin_gesture().unwrap();
        active.set_pixel(active_gesture, 1, 1, RED).unwrap();
        assert_eq!(
            GpuExactLayerRecoveryCommand::from_raster(layer_id, &active),
            Err(GpuExactLayerRecoveryBuildError::GestureActive)
        );
        active.cancel_gesture(active_gesture).unwrap();

        let mut invalid = RasterLayer::new(16, 16, 16).unwrap();
        let mut pixels = vec![LinearRgba::TRANSPARENT; 16 * 16];
        pixels[0] = LinearRgba::premultiplied(f32::NAN, 0.0, 0.0, 1.0);
        invalid
            .restore_tile(TileCoord::new(0, 0), pixels.into_boxed_slice())
            .unwrap();
        assert_eq!(
            GpuExactLayerRecoveryCommand::from_raster(layer_id, &invalid),
            Err(GpuExactLayerRecoveryBuildError::NonFinitePixel {
                tile: TileCoord::new(0, 0),
                index: 0,
            })
        );

        let valid = RasterLayer::new(16, 16, 16).unwrap();
        let command = GpuExactLayerRecoveryCommand::from_raster(layer_id, &valid).unwrap();
        let mut wrong_geometry = RasterLayer::new(32, 16, 16).unwrap();
        assert!(matches!(
            replay_exact_layer_recovery_command(layer_id, &mut wrong_geometry, &command),
            Err(GpuExactLayerRecoveryReplayError::GeometryMismatch { .. })
        ));
        assert_eq!([wrong_geometry.width(), wrong_geometry.height()], [32, 16]);
    }
}
