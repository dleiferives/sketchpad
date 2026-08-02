use crate::{
    document::{DocumentRevision, LayerId},
    gpu_document_mirror::{
        GpuCpuMirrorSnapshot, GpuMirrorPatchBatch, GpuMirrorPatchRegion, GpuMirrorReadbackPlan,
    },
    gpu_document_undo::GPU_UNDO_BLOCK_SIZE,
    raster::{Damage, LinearRgba, RasterLayer, RectU32, TileCoord},
};
use std::{collections::BTreeMap, error::Error, fmt, mem::size_of, sync::Arc};

#[derive(Clone, Debug, PartialEq)]
pub struct GpuExactRasterRecoveryCommand {
    layer: LayerId,
    tile_size: u32,
    tiles: Box<[GpuExactRasterRecoveryTile]>,
    region_count: usize,
    pixel_count: usize,
    retained_byte_len: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct GpuExactRasterRecoveryTile {
    coord: TileCoord,
    initialized: bool,
    regions: Box<[GpuExactRasterRecoveryRegion]>,
}

#[derive(Clone, Debug, PartialEq)]
struct GpuExactRasterRecoveryRegion {
    local_bounds: RectU32,
    pixels: Arc<[LinearRgba]>,
}

impl GpuExactRasterRecoveryCommand {
    pub fn from_regions(
        tile_size: u32,
        regions: Vec<GpuMirrorPatchRegion>,
    ) -> Result<Self, Box<GpuExactRasterRecoveryBuildFailure>> {
        if tile_size == 0 {
            return Err(build_failure(
                GpuExactRasterRecoveryBuildError::InvalidTileSize,
                regions,
            ));
        }
        let borrowed: Vec<_> = regions.iter().collect();
        let metrics = match validate_regions(tile_size, &borrowed) {
            Ok(metrics) => metrics,
            Err(error) => return Err(build_failure(error, regions)),
        };

        Ok(build_validated_command(tile_size, regions, metrics))
    }

    pub fn from_mirror_revision(
        plan: &GpuMirrorReadbackPlan,
        mut batches: Vec<GpuMirrorPatchBatch>,
    ) -> Result<Self, Box<GpuExactRasterRecoveryMirrorFailure>> {
        if let Err(error) = validate_mirror_batches(plan, &batches) {
            return Err(Box::new(GpuExactRasterRecoveryMirrorFailure {
                error,
                batches,
            }));
        }
        let borrowed: Vec<_> = batches.iter().flat_map(|batch| batch.regions()).collect();
        let metrics = match validate_regions(plan.layout().tile_size(), &borrowed) {
            Ok(metrics) => metrics,
            Err(error) => {
                return Err(Box::new(GpuExactRasterRecoveryMirrorFailure {
                    error: GpuExactRasterRecoveryMirrorError::Command(error),
                    batches,
                }));
            }
        };
        batches.sort_by_key(GpuMirrorPatchBatch::index);
        let regions = batches
            .into_iter()
            .flat_map(GpuMirrorPatchBatch::into_regions)
            .collect();
        Ok(build_validated_command(
            plan.layout().tile_size(),
            regions,
            metrics,
        ))
    }

    pub const fn layer(&self) -> LayerId {
        self.layer
    }

    pub const fn tile_size(&self) -> u32 {
        self.tile_size
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    pub const fn region_count(&self) -> usize {
        self.region_count
    }

    pub const fn pixel_count(&self) -> usize {
        self.pixel_count
    }

    pub const fn retained_byte_len(&self) -> u64 {
        self.retained_byte_len
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GpuExactRasterRecoveryTransition {
    revision: DocumentRevision,
    before: GpuExactRasterRecoveryCommand,
    after: GpuExactRasterRecoveryCommand,
    retained_byte_len: u64,
}

impl GpuExactRasterRecoveryTransition {
    pub fn from_mirror_revision(
        before: &GpuCpuMirrorSnapshot,
        plan: &GpuMirrorReadbackPlan,
        batches: Vec<GpuMirrorPatchBatch>,
    ) -> Result<Self, Box<GpuExactRasterRecoveryTransitionFailure>> {
        if before.revision() != plan.source_revision() {
            return Err(transition_failure(
                GpuExactRasterRecoveryTransitionError::BaseRevisionMismatch {
                    base: before.revision(),
                    source: plan.source_revision(),
                },
                batches,
            ));
        }
        if before.tile_size() != plan.layout().tile_size() {
            return Err(transition_failure(
                GpuExactRasterRecoveryTransitionError::TileSizeMismatch {
                    base: before.tile_size(),
                    transition: plan.layout().tile_size(),
                },
                batches,
            ));
        }
        if let Err(error) = validate_mirror_batches(plan, &batches) {
            return Err(transition_failure(
                GpuExactRasterRecoveryTransitionError::After(error),
                batches,
            ));
        }
        let borrowed: Vec<_> = batches.iter().flat_map(|batch| batch.regions()).collect();
        if let Err(error) = validate_regions(plan.layout().tile_size(), &borrowed) {
            return Err(transition_failure(
                GpuExactRasterRecoveryTransitionError::After(
                    GpuExactRasterRecoveryMirrorError::Command(error),
                ),
                batches,
            ));
        }

        let before_regions = match capture_snapshot_regions(before, plan) {
            Ok(regions) => regions,
            Err(error) => return Err(transition_failure(error, batches)),
        };
        let before_command = match Self::build_before(plan.layout().tile_size(), before_regions) {
            Ok(command) => command,
            Err(error) => return Err(transition_failure(error, batches)),
        };
        let after = match GpuExactRasterRecoveryCommand::from_mirror_revision(plan, batches) {
            Ok(command) => command,
            Err(failure) => {
                return Err(transition_failure(
                    GpuExactRasterRecoveryTransitionError::After(failure.error),
                    failure.batches,
                ));
            }
        };
        debug_assert_eq!(before_command.layer(), after.layer());
        let retained_byte_len = transition_retained_byte_len(&before_command, &after)
            .expect("validated addressable recovery commands have representable combined bytes");
        Ok(Self {
            revision: plan.revision(),
            before: before_command,
            after,
            retained_byte_len,
        })
    }

    fn build_before(
        tile_size: u32,
        regions: Vec<GpuMirrorPatchRegion>,
    ) -> Result<GpuExactRasterRecoveryCommand, GpuExactRasterRecoveryTransitionError> {
        GpuExactRasterRecoveryCommand::from_regions(tile_size, regions)
            .map_err(|failure| GpuExactRasterRecoveryTransitionError::Before(failure.error))
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn layer(&self) -> LayerId {
        self.before.layer()
    }

    pub const fn before(&self) -> &GpuExactRasterRecoveryCommand {
        &self.before
    }

    pub const fn after(&self) -> &GpuExactRasterRecoveryCommand {
        &self.after
    }

    pub const fn retained_byte_len(&self) -> u64 {
        self.retained_byte_len
    }

    pub fn into_commands(self) -> (GpuExactRasterRecoveryCommand, GpuExactRasterRecoveryCommand) {
        (self.before, self.after)
    }
}

fn capture_snapshot_regions(
    snapshot: &GpuCpuMirrorSnapshot,
    plan: &GpuMirrorReadbackPlan,
) -> Result<Vec<GpuMirrorPatchRegion>, GpuExactRasterRecoveryTransitionError> {
    let region_count = plan
        .batches()
        .iter()
        .try_fold(0_usize, |count, batch| {
            count.checked_add(batch.regions().len())
        })
        .ok_or(GpuExactRasterRecoveryTransitionError::ByteCountOverflow)?;
    let mut captured = Vec::with_capacity(region_count);
    for region in plan.batches().iter().flat_map(|batch| batch.regions()) {
        if snapshot.tile_bounds(region.key.tile).is_none() {
            return Err(GpuExactRasterRecoveryTransitionError::TileOutOfBounds(
                region.key.tile,
            ));
        }
        let existing = snapshot.tile_pixels(region.key);
        let pixels = existing.map_or_else(
            || Arc::from([]),
            |tile| {
                let stride = snapshot.tile_size() as usize;
                let width = region.local_bounds.width() as usize;
                let mut pixels = Vec::with_capacity(region.local_bounds.area() as usize);
                for y in region.local_bounds.min_y()..region.local_bounds.max_y() {
                    let start = y as usize * stride + region.local_bounds.min_x() as usize;
                    pixels.extend_from_slice(&tile[start..start + width]);
                }
                pixels.into()
            },
        );
        captured.push(GpuMirrorPatchRegion {
            key: region.key,
            local_bounds: region.local_bounds,
            initialized: existing.is_some(),
            pixels,
        });
    }
    Ok(captured)
}

fn transition_retained_byte_len(
    before: &GpuExactRasterRecoveryCommand,
    after: &GpuExactRasterRecoveryCommand,
) -> Option<u64> {
    (size_of::<GpuExactRasterRecoveryTransition>() as u64)
        .checked_add(
            before
                .retained_byte_len()
                .checked_sub(size_of::<GpuExactRasterRecoveryCommand>() as u64)?,
        )?
        .checked_add(
            after
                .retained_byte_len()
                .checked_sub(size_of::<GpuExactRasterRecoveryCommand>() as u64)?,
        )
}

fn transition_failure(
    error: GpuExactRasterRecoveryTransitionError,
    batches: Vec<GpuMirrorPatchBatch>,
) -> Box<GpuExactRasterRecoveryTransitionFailure> {
    Box::new(GpuExactRasterRecoveryTransitionFailure { error, batches })
}

pub struct GpuExactRasterRecoveryTransitionFailure {
    pub error: GpuExactRasterRecoveryTransitionError,
    pub batches: Vec<GpuMirrorPatchBatch>,
}

impl fmt::Debug for GpuExactRasterRecoveryTransitionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuExactRasterRecoveryTransitionFailure")
            .field("error", &self.error)
            .field("batch_count", &self.batches.len())
            .finish()
    }
}

impl fmt::Display for GpuExactRasterRecoveryTransitionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuExactRasterRecoveryTransitionFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuExactRasterRecoveryTransitionError {
    BaseRevisionMismatch {
        base: DocumentRevision,
        source: DocumentRevision,
    },
    TileSizeMismatch {
        base: u32,
        transition: u32,
    },
    TileOutOfBounds(TileCoord),
    Before(GpuExactRasterRecoveryBuildError),
    After(GpuExactRasterRecoveryMirrorError),
    ByteCountOverflow,
}

impl fmt::Display for GpuExactRasterRecoveryTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BaseRevisionMismatch { base, source } => write!(
                formatter,
                "GPU recovery transition starts at revision {}, not CPU base {}",
                source.get(),
                base.get()
            ),
            Self::TileSizeMismatch { base, transition } => write!(
                formatter,
                "GPU recovery transition tile size {transition} does not match CPU base {base}"
            ),
            Self::TileOutOfBounds(tile) => {
                write!(
                    formatter,
                    "GPU recovery transition tile {tile:?} is outside the CPU base"
                )
            }
            Self::Before(error) => write!(formatter, "invalid recovery before-state: {error}"),
            Self::After(error) => write!(formatter, "invalid recovery after-state: {error}"),
            Self::ByteCountOverflow => write!(formatter, "GPU recovery transition bytes overflow"),
        }
    }
}

impl Error for GpuExactRasterRecoveryTransitionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Before(error) => Some(error),
            Self::After(error) => Some(error),
            _ => None,
        }
    }
}

fn build_validated_command(
    tile_size: u32,
    mut regions: Vec<GpuMirrorPatchRegion>,
    metrics: RecoveryMetrics,
) -> GpuExactRasterRecoveryCommand {
    let RecoveryMetrics {
        layer,
        pixel_count,
        retained_byte_len,
    } = metrics;

    regions.sort_by_key(|region| {
        (
            region.key.tile.y,
            region.key.tile.x,
            region.local_bounds.min_y(),
            region.local_bounds.min_x(),
            region.local_bounds.max_y(),
            region.local_bounds.max_x(),
        )
    });
    let region_count = regions.len();
    let mut grouped = BTreeMap::<(u32, u32), Vec<GpuMirrorPatchRegion>>::new();
    for region in regions {
        grouped
            .entry((region.key.tile.y, region.key.tile.x))
            .or_default()
            .push(region);
    }

    let mut tiles = Vec::with_capacity(grouped.len());
    for ((tile_y, tile_x), grouped_regions) in grouped {
        let initialized = grouped_regions[0].initialized;
        let regions = if initialized {
            grouped_regions
                .into_iter()
                .map(|region| GpuExactRasterRecoveryRegion {
                    local_bounds: region.local_bounds,
                    pixels: region.pixels,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice()
        } else {
            Box::new([])
        };
        tiles.push(GpuExactRasterRecoveryTile {
            coord: TileCoord::new(tile_x, tile_y),
            initialized,
            regions,
        });
    }

    GpuExactRasterRecoveryCommand {
        layer,
        tile_size,
        tiles: tiles.into_boxed_slice(),
        region_count,
        pixel_count,
        retained_byte_len,
    }
}

pub fn replay_exact_raster_recovery_command(
    layer_id: LayerId,
    layer: &mut RasterLayer,
    command: &GpuExactRasterRecoveryCommand,
) -> Result<GpuExactRasterRecoveryReplay, GpuExactRasterRecoveryReplayError> {
    if layer_id != command.layer {
        return Err(GpuExactRasterRecoveryReplayError::LayerMismatch {
            expected: command.layer,
            actual: layer_id,
        });
    }
    if layer.tile_size() != command.tile_size {
        return Err(GpuExactRasterRecoveryReplayError::TileSizeMismatch {
            expected: command.tile_size,
            actual: layer.tile_size(),
        });
    }

    let tile_pixel_count = usize::try_from(
        command
            .tile_size
            .checked_mul(command.tile_size)
            .expect("a validated recovery tile size has representable storage"),
    )
    .expect("a RasterLayer supports its validated tile storage on this platform");
    let mut prepared = Vec::with_capacity(command.tiles.len());
    let mut damage = Damage::default();
    let mut pixels_changed = 0_u64;

    for tile in &command.tiles {
        let valid_bounds = layer.tile_bounds(tile.coord).ok_or(
            GpuExactRasterRecoveryReplayError::TileOutOfBounds(tile.coord),
        )?;
        let valid_width = valid_bounds.width();
        let valid_height = valid_bounds.height();
        let was_allocated = layer.tile_is_allocated(tile.coord);
        let mut pixels = layer.tile(tile.coord).map_or_else(
            || vec![LinearRgba::TRANSPARENT; tile_pixel_count],
            |tile| tile.pixels().to_vec(),
        );
        let before = pixels.clone();

        if tile.initialized {
            for region in &tile.regions {
                copy_clipped_region(
                    &mut pixels,
                    command.tile_size,
                    valid_width,
                    valid_height,
                    region,
                );
            }
        } else {
            pixels.fill(LinearRgba::TRANSPARENT);
        }

        let changed_bounds = changed_pixel_bounds(
            &before,
            &pixels,
            command.tile_size,
            valid_width,
            valid_height,
            &mut pixels_changed,
        );
        if let Some(changed_bounds) = changed_bounds {
            damage.add(
                tile.coord,
                translate_to_canvas(changed_bounds, valid_bounds),
            );
            prepared.push(PreparedRecoveryTile {
                coord: tile.coord,
                pixels: pixels.into_boxed_slice(),
                was_allocated,
            });
        }
    }

    let mut tiles_written = 0_u32;
    let mut tiles_removed = 0_u32;
    for tile in prepared {
        layer
            .restore_tile(tile.coord, tile.pixels)
            .expect("recovery replay prevalidated every tile and pixel count");
        if layer.tile_is_allocated(tile.coord) {
            tiles_written = tiles_written.saturating_add(1);
        } else if tile.was_allocated {
            tiles_removed = tiles_removed.saturating_add(1);
        }
    }

    Ok(GpuExactRasterRecoveryReplay {
        damage: (!damage.is_empty()).then_some(damage),
        pixels_changed,
        tiles_written,
        tiles_removed,
    })
}

struct PreparedRecoveryTile {
    coord: TileCoord,
    pixels: Box<[LinearRgba]>,
    was_allocated: bool,
}

struct RecoveryMetrics {
    layer: LayerId,
    pixel_count: usize,
    retained_byte_len: u64,
}

fn validate_regions(
    tile_size: u32,
    regions: &[&GpuMirrorPatchRegion],
) -> Result<RecoveryMetrics, GpuExactRasterRecoveryBuildError> {
    if tile_size == 0 {
        return Err(GpuExactRasterRecoveryBuildError::InvalidTileSize);
    }
    let Some(first) = regions.first() else {
        return Err(GpuExactRasterRecoveryBuildError::EmptyCommand);
    };
    let layer = first.key.layer;
    let mut by_tile = BTreeMap::<(u32, u32), Vec<&GpuMirrorPatchRegion>>::new();
    let mut pixel_count = 0_usize;
    for &region in regions {
        if region.key.layer != layer {
            return Err(GpuExactRasterRecoveryBuildError::MixedLayers {
                first: layer,
                second: region.key.layer,
            });
        }
        if region.local_bounds.max_x() > tile_size || region.local_bounds.max_y() > tile_size {
            return Err(GpuExactRasterRecoveryBuildError::RegionOutsideTile {
                tile: region.key.tile,
                bounds: region.local_bounds,
                tile_size,
            });
        }
        if !region
            .local_bounds
            .min_x()
            .is_multiple_of(GPU_UNDO_BLOCK_SIZE)
            || !region
                .local_bounds
                .min_y()
                .is_multiple_of(GPU_UNDO_BLOCK_SIZE)
            || !region
                .local_bounds
                .max_x()
                .is_multiple_of(GPU_UNDO_BLOCK_SIZE)
            || !region
                .local_bounds
                .max_y()
                .is_multiple_of(GPU_UNDO_BLOCK_SIZE)
        {
            return Err(GpuExactRasterRecoveryBuildError::RegionNotBlockAligned {
                tile: region.key.tile,
                bounds: region.local_bounds,
                block_size: GPU_UNDO_BLOCK_SIZE,
            });
        }
        let expected = if region.initialized {
            usize::try_from(region.local_bounds.area())
                .map_err(|_| GpuExactRasterRecoveryBuildError::PixelCountOverflow)?
        } else {
            0
        };
        if region.pixels.len() != expected {
            return Err(GpuExactRasterRecoveryBuildError::InvalidPixelCount {
                tile: region.key.tile,
                expected,
                actual: region.pixels.len(),
            });
        }
        if let Some((index, _)) = region
            .pixels
            .iter()
            .enumerate()
            .find(|(_, pixel)| !pixel_is_finite(**pixel))
        {
            return Err(GpuExactRasterRecoveryBuildError::NonFinitePixel {
                tile: region.key.tile,
                index,
            });
        }
        pixel_count = pixel_count
            .checked_add(region.pixels.len())
            .ok_or(GpuExactRasterRecoveryBuildError::ByteCountOverflow)?;
        by_tile
            .entry((region.key.tile.y, region.key.tile.x))
            .or_default()
            .push(region);
    }

    for grouped in by_tile.values() {
        let initialized = grouped[0].initialized;
        if let Some(conflicting) = grouped
            .iter()
            .copied()
            .find(|region| region.initialized != initialized)
        {
            return Err(GpuExactRasterRecoveryBuildError::ConflictingResidentState(
                conflicting.key.tile,
            ));
        }
        if !initialized {
            continue;
        }
        for (index, region) in grouped.iter().enumerate() {
            if grouped[index + 1..]
                .iter()
                .any(|other| rectangles_overlap(region.local_bounds, other.local_bounds))
            {
                return Err(GpuExactRasterRecoveryBuildError::OverlappingRegions(
                    region.key.tile,
                ));
            }
        }
    }
    let retained_byte_len = retained_byte_len(by_tile.len(), regions.len(), pixel_count)
        .ok_or(GpuExactRasterRecoveryBuildError::ByteCountOverflow)?;
    Ok(RecoveryMetrics {
        layer,
        pixel_count,
        retained_byte_len,
    })
}

fn validate_mirror_batches(
    plan: &GpuMirrorReadbackPlan,
    batches: &[GpuMirrorPatchBatch],
) -> Result<(), GpuExactRasterRecoveryMirrorError> {
    if batches.len() != plan.batches().len() {
        return Err(GpuExactRasterRecoveryMirrorError::BatchCountMismatch {
            expected: plan.batches().len(),
            actual: batches.len(),
        });
    }
    let mut seen = vec![false; batches.len()];
    for batch in batches {
        if batch.revision() != plan.revision() {
            return Err(GpuExactRasterRecoveryMirrorError::RevisionMismatch {
                expected: plan.revision(),
                actual: batch.revision(),
            });
        }
        let index = batch.index() as usize;
        let Some(expected) = plan.batches().get(index) else {
            return Err(GpuExactRasterRecoveryMirrorError::BatchIndexOutOfRange {
                index: batch.index(),
                count: plan.batches().len(),
            });
        };
        if std::mem::replace(&mut seen[index], true) {
            return Err(GpuExactRasterRecoveryMirrorError::DuplicateBatch(
                batch.index(),
            ));
        }
        if batch.byte_len() != expected.byte_len() {
            return Err(GpuExactRasterRecoveryMirrorError::BatchByteMismatch {
                index: batch.index(),
                expected: expected.byte_len(),
                actual: batch.byte_len(),
            });
        }
        let shape_matches = batch.regions().len() == expected.regions().len()
            && batch
                .regions()
                .iter()
                .zip(expected.regions())
                .all(|(actual, expected)| {
                    actual.key == expected.key
                        && actual.local_bounds == expected.local_bounds
                        && actual.initialized == expected.initialized
                });
        if !shape_matches {
            return Err(GpuExactRasterRecoveryMirrorError::BatchShapeMismatch(
                batch.index(),
            ));
        }
    }
    Ok(())
}

fn retained_byte_len(tile_count: usize, region_count: usize, pixel_count: usize) -> Option<u64> {
    let tile_bytes = u64::try_from(tile_count)
        .ok()?
        .checked_mul(size_of::<GpuExactRasterRecoveryTile>() as u64)?;
    let region_bytes = u64::try_from(region_count)
        .ok()?
        .checked_mul(size_of::<GpuExactRasterRecoveryRegion>() as u64)?;
    let pixel_bytes = u64::try_from(pixel_count)
        .ok()?
        .checked_mul(size_of::<LinearRgba>() as u64)?;
    (size_of::<GpuExactRasterRecoveryCommand>() as u64)
        .checked_add(tile_bytes)?
        .checked_add(region_bytes)?
        .checked_add(pixel_bytes)
}

fn copy_clipped_region(
    destination: &mut [LinearRgba],
    tile_size: u32,
    valid_width: u32,
    valid_height: u32,
    region: &GpuExactRasterRecoveryRegion,
) {
    let copy_max_x = region.local_bounds.max_x().min(valid_width);
    let copy_max_y = region.local_bounds.max_y().min(valid_height);
    if region.local_bounds.min_x() >= copy_max_x || region.local_bounds.min_y() >= copy_max_y {
        return;
    }
    let source_stride = region.local_bounds.width() as usize;
    let copy_width = (copy_max_x - region.local_bounds.min_x()) as usize;
    for y in region.local_bounds.min_y()..copy_max_y {
        let destination_start =
            y as usize * tile_size as usize + region.local_bounds.min_x() as usize;
        let source_start = (y - region.local_bounds.min_y()) as usize * source_stride;
        destination[destination_start..destination_start + copy_width]
            .copy_from_slice(&region.pixels[source_start..source_start + copy_width]);
    }
}

fn changed_pixel_bounds(
    before: &[LinearRgba],
    after: &[LinearRgba],
    tile_size: u32,
    valid_width: u32,
    valid_height: u32,
    changed_count: &mut u64,
) -> Option<RectU32> {
    let mut bounds = None::<RectU32>;
    for y in 0..valid_height {
        let row = y as usize * tile_size as usize;
        for x in 0..valid_width {
            let index = row + x as usize;
            if pixel_bits_equal(before[index], after[index]) {
                continue;
            }
            *changed_count = changed_count.saturating_add(1);
            let changed = RectU32::from_xywh(x, y, 1, 1).expect("one pixel has nonempty bounds");
            bounds = Some(bounds.map_or(changed, |bounds| bounds.union(changed)));
        }
    }
    bounds
}

fn translate_to_canvas(local: RectU32, tile: RectU32) -> RectU32 {
    RectU32::from_min_max(
        tile.min_x() + local.min_x(),
        tile.min_y() + local.min_y(),
        tile.min_x() + local.max_x(),
        tile.min_y() + local.max_y(),
    )
    .expect("a nonempty local recovery region remains nonempty when translated")
}

fn rectangles_overlap(first: RectU32, second: RectU32) -> bool {
    first.min_x() < second.max_x()
        && second.min_x() < first.max_x()
        && first.min_y() < second.max_y()
        && second.min_y() < first.max_y()
}

fn pixel_is_finite(pixel: LinearRgba) -> bool {
    pixel.r.is_finite() && pixel.g.is_finite() && pixel.b.is_finite() && pixel.a.is_finite()
}

fn pixel_bits_equal(first: LinearRgba, second: LinearRgba) -> bool {
    first.r.to_bits() == second.r.to_bits()
        && first.g.to_bits() == second.g.to_bits()
        && first.b.to_bits() == second.b.to_bits()
        && first.a.to_bits() == second.a.to_bits()
}

fn build_failure(
    error: GpuExactRasterRecoveryBuildError,
    regions: Vec<GpuMirrorPatchRegion>,
) -> Box<GpuExactRasterRecoveryBuildFailure> {
    Box::new(GpuExactRasterRecoveryBuildFailure { error, regions })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuExactRasterRecoveryReplay {
    pub damage: Option<Damage>,
    pub pixels_changed: u64,
    pub tiles_written: u32,
    pub tiles_removed: u32,
}

pub struct GpuExactRasterRecoveryBuildFailure {
    pub error: GpuExactRasterRecoveryBuildError,
    pub regions: Vec<GpuMirrorPatchRegion>,
}

impl fmt::Debug for GpuExactRasterRecoveryBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuExactRasterRecoveryBuildFailure")
            .field("error", &self.error)
            .field("region_count", &self.regions.len())
            .finish()
    }
}

impl fmt::Display for GpuExactRasterRecoveryBuildFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuExactRasterRecoveryBuildFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

pub struct GpuExactRasterRecoveryMirrorFailure {
    pub error: GpuExactRasterRecoveryMirrorError,
    pub batches: Vec<GpuMirrorPatchBatch>,
}

impl fmt::Debug for GpuExactRasterRecoveryMirrorFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuExactRasterRecoveryMirrorFailure")
            .field("error", &self.error)
            .field("batch_count", &self.batches.len())
            .finish()
    }
}

impl fmt::Display for GpuExactRasterRecoveryMirrorFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for GpuExactRasterRecoveryMirrorFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuExactRasterRecoveryMirrorError {
    BatchCountMismatch {
        expected: usize,
        actual: usize,
    },
    RevisionMismatch {
        expected: DocumentRevision,
        actual: DocumentRevision,
    },
    BatchIndexOutOfRange {
        index: u32,
        count: usize,
    },
    DuplicateBatch(u32),
    BatchByteMismatch {
        index: u32,
        expected: u64,
        actual: u64,
    },
    BatchShapeMismatch(u32),
    Command(GpuExactRasterRecoveryBuildError),
}

impl fmt::Display for GpuExactRasterRecoveryMirrorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BatchCountMismatch { expected, actual } => write!(
                formatter,
                "GPU raster recovery has {actual} mapped batches, expected {expected}"
            ),
            Self::RevisionMismatch { expected, actual } => write!(
                formatter,
                "GPU raster recovery batch revision {} does not match {}",
                actual.get(),
                expected.get()
            ),
            Self::BatchIndexOutOfRange { index, count } => write!(
                formatter,
                "GPU raster recovery batch {index} lies outside {count} batches"
            ),
            Self::DuplicateBatch(index) => {
                write!(formatter, "GPU raster recovery batch {index} is duplicated")
            }
            Self::BatchByteMismatch {
                index,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU raster recovery batch {index} has {actual} bytes, expected {expected}"
            ),
            Self::BatchShapeMismatch(index) => {
                write!(
                    formatter,
                    "GPU raster recovery batch {index} shape differs from its plan"
                )
            }
            Self::Command(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuExactRasterRecoveryMirrorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Command(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuExactRasterRecoveryBuildError {
    InvalidTileSize,
    EmptyCommand,
    MixedLayers {
        first: LayerId,
        second: LayerId,
    },
    RegionOutsideTile {
        tile: TileCoord,
        bounds: RectU32,
        tile_size: u32,
    },
    RegionNotBlockAligned {
        tile: TileCoord,
        bounds: RectU32,
        block_size: u32,
    },
    PixelCountOverflow,
    InvalidPixelCount {
        tile: TileCoord,
        expected: usize,
        actual: usize,
    },
    NonFinitePixel {
        tile: TileCoord,
        index: usize,
    },
    ConflictingResidentState(TileCoord),
    OverlappingRegions(TileCoord),
    ByteCountOverflow,
}

impl fmt::Display for GpuExactRasterRecoveryBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTileSize => write!(formatter, "GPU raster recovery tile size is zero"),
            Self::EmptyCommand => write!(formatter, "GPU raster recovery command is empty"),
            Self::MixedLayers { first, second } => write!(
                formatter,
                "GPU raster recovery command mixes layers {} and {}",
                first.get(),
                second.get()
            ),
            Self::RegionOutsideTile {
                tile,
                bounds,
                tile_size,
            } => write!(
                formatter,
                "GPU raster recovery region {bounds:?} for tile ({}, {}) exceeds {tile_size}",
                tile.x, tile.y
            ),
            Self::RegionNotBlockAligned {
                tile,
                bounds,
                block_size,
            } => write!(
                formatter,
                "GPU raster recovery region {bounds:?} for tile ({}, {}) is not aligned to {block_size}-pixel blocks",
                tile.x, tile.y
            ),
            Self::PixelCountOverflow => write!(formatter, "GPU raster recovery pixels overflow"),
            Self::InvalidPixelCount {
                tile,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU raster recovery tile ({}, {}) has {actual} pixels, expected {expected}",
                tile.x, tile.y
            ),
            Self::NonFinitePixel { tile, index } => write!(
                formatter,
                "GPU raster recovery tile ({}, {}) pixel {index} is non-finite",
                tile.x, tile.y
            ),
            Self::ConflictingResidentState(tile) => write!(
                formatter,
                "GPU raster recovery tile ({}, {}) has conflicting resident states",
                tile.x, tile.y
            ),
            Self::OverlappingRegions(tile) => write!(
                formatter,
                "GPU raster recovery tile ({}, {}) has overlapping regions",
                tile.x, tile.y
            ),
            Self::ByteCountOverflow => write!(formatter, "GPU raster recovery bytes overflow"),
        }
    }
}

impl Error for GpuExactRasterRecoveryBuildError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuExactRasterRecoveryReplayError {
    LayerMismatch { expected: LayerId, actual: LayerId },
    TileSizeMismatch { expected: u32, actual: u32 },
    TileOutOfBounds(TileCoord),
}

impl fmt::Display for GpuExactRasterRecoveryReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LayerMismatch { expected, actual } => write!(
                formatter,
                "GPU raster recovery layer {} does not match target {}",
                expected.get(),
                actual.get()
            ),
            Self::TileSizeMismatch { expected, actual } => write!(
                formatter,
                "GPU raster recovery tile size {expected} does not match target {actual}"
            ),
            Self::TileOutOfBounds(tile) => write!(
                formatter,
                "GPU raster recovery tile ({}, {}) lies outside the target",
                tile.x, tile.y
            ),
        }
    }
}

impl Error for GpuExactRasterRecoveryReplayError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gpu_atlas::{AtlasLayout, LayerTileKey, SparseAtlasPlanner},
        gpu_document_mirror::{GpuMirrorBatchPlan, GpuMirrorReconciler},
        gpu_document_undo::{GpuMementoResidentState, GpuUndoCapturePlan, GPU_UNDO_BLOCK_BYTES},
        gpu_round_target::ActiveRoundMaskTile,
    };

    const RED: LinearRgba = LinearRgba::premultiplied(0.5, 0.0, 0.0, 0.5);
    const BLUE: LinearRgba = LinearRgba::premultiplied(0.0, 0.0, 0.75, 0.75);

    fn region(
        layer: LayerId,
        tile: TileCoord,
        bounds: RectU32,
        initialized: bool,
        pixel: LinearRgba,
    ) -> GpuMirrorPatchRegion {
        let count = if initialized {
            bounds.area() as usize
        } else {
            0
        };
        GpuMirrorPatchRegion {
            key: LayerTileKey::new(layer, tile),
            local_bounds: bounds,
            initialized,
            pixels: vec![pixel; count].into(),
        }
    }

    fn patch_batch(plan: &GpuMirrorBatchPlan, pixel: LinearRgba) -> GpuMirrorPatchBatch {
        let regions = plan
            .regions()
            .iter()
            .map(|expected| GpuMirrorPatchRegion {
                key: expected.key,
                local_bounds: expected.local_bounds,
                initialized: expected.initialized,
                pixels: if expected.initialized {
                    vec![pixel; expected.local_bounds.area() as usize].into()
                } else {
                    Vec::new().into()
                },
            })
            .collect();
        GpuMirrorPatchBatch::from_test_parts(
            plan.revision(),
            plan.index(),
            regions,
            plan.byte_len(),
        )
    }

    fn one_tile_plan(revision: u64, initialized: bool) -> (GpuMirrorReadbackPlan, LayerTileKey) {
        let layout = AtlasLayout::new(64, 32, 1).unwrap();
        let layer = LayerId::from_raw(11);
        let key = LayerTileKey::new(layer, TileCoord::new(0, 0));
        let mut atlas = SparseAtlasPlanner::new(layout);
        let slot = atlas.allocate(key).unwrap().slot;
        let capture = GpuUndoCapturePlan::from_active_tiles(
            layout,
            &[ActiveRoundMaskTile {
                key,
                slot,
                local_damage: RectU32::from_xywh(0, 0, 32, 32).unwrap(),
            }],
        )
        .unwrap();
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::from_raw(revision - 1),
            DocumentRevision::from_raw(revision),
            &capture,
            &[GpuMementoResidentState {
                key,
                slot,
                document_initialized: initialized,
                memento_initialized: !initialized,
            }],
            4 * GPU_UNDO_BLOCK_BYTES,
        )
        .unwrap();
        (plan, key)
    }

    #[test]
    fn construction_is_canonical_and_returns_pixels_on_validation_failure() {
        let layer = LayerId::from_raw(1);
        let tile = TileCoord::new(0, 0);
        let first = region(
            layer,
            tile,
            RectU32::from_xywh(16, 0, 16, 16).unwrap(),
            true,
            RED,
        );
        let second = region(
            layer,
            tile,
            RectU32::from_xywh(0, 0, 16, 16).unwrap(),
            true,
            BLUE,
        );
        let command = GpuExactRasterRecoveryCommand::from_regions(32, vec![first, second]).unwrap();
        assert_eq!(command.layer(), layer);
        assert_eq!(command.tile_count(), 1);
        assert_eq!(command.region_count(), 2);
        assert_eq!(command.pixel_count(), 512);
        assert!(command.retained_byte_len() >= 512 * size_of::<LinearRgba>() as u64);
        let cloned = command.clone();
        assert!(Arc::ptr_eq(
            &command.tiles[0].regions[0].pixels,
            &cloned.tiles[0].regions[0].pixels
        ));

        let overlap = vec![
            region(
                layer,
                tile,
                RectU32::from_xywh(0, 0, 16, 16).unwrap(),
                true,
                RED,
            ),
            region(
                layer,
                tile,
                RectU32::from_xywh(0, 0, 16, 16).unwrap(),
                true,
                BLUE,
            ),
        ];
        let failure = GpuExactRasterRecoveryCommand::from_regions(32, overlap).unwrap_err();
        assert_eq!(
            failure.error,
            GpuExactRasterRecoveryBuildError::OverlappingRegions(tile)
        );
        assert_eq!(failure.regions.len(), 2);
        assert_eq!(failure.regions[0].pixels[0], RED);
    }

    #[test]
    fn replay_patches_exact_bits_and_preserves_untouched_pixels() {
        let layer_id = LayerId::from_raw(7);
        let tile = TileCoord::new(0, 0);
        let mut layer = RasterLayer::new(64, 32, 32).unwrap();
        let gesture = layer.begin_gesture().unwrap();
        layer.set_pixel(gesture, 1, 1, RED).unwrap();
        layer.set_pixel(gesture, 20, 4, RED).unwrap();
        layer.commit_gesture(gesture).unwrap();
        layer.clear_history();

        let command = GpuExactRasterRecoveryCommand::from_regions(
            32,
            vec![region(
                layer_id,
                tile,
                RectU32::from_xywh(16, 0, 16, 16).unwrap(),
                true,
                BLUE,
            )],
        )
        .unwrap();
        let replay = replay_exact_raster_recovery_command(layer_id, &mut layer, &command).unwrap();

        assert_eq!(layer.pixel(1, 1), Some(RED));
        assert_eq!(layer.pixel(20, 4), Some(BLUE));
        assert_eq!(replay.pixels_changed, 256);
        assert_eq!(replay.tiles_written, 1);
        assert_eq!(replay.tiles_removed, 0);
        assert_eq!(layer.undo_depth(), 0);
    }

    #[test]
    fn complete_mirror_revision_accepts_out_of_order_batches_and_rejects_duplicates() {
        let layout = AtlasLayout::new(64, 32, 1).unwrap();
        let layer = LayerId::from_raw(8);
        let key = LayerTileKey::new(layer, TileCoord::new(0, 0));
        let mut atlas = SparseAtlasPlanner::new(layout);
        let slot = atlas.allocate(key).unwrap().slot;
        let capture = GpuUndoCapturePlan::from_active_tiles(
            layout,
            &[ActiveRoundMaskTile {
                key,
                slot,
                local_damage: RectU32::from_xywh(0, 0, 32, 32).unwrap(),
            }],
        )
        .unwrap();
        let revision = DocumentRevision::from_raw(4);
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::INITIAL,
            revision,
            &capture,
            &[GpuMementoResidentState {
                key,
                slot,
                document_initialized: true,
                memento_initialized: true,
            }],
            2 * GPU_UNDO_BLOCK_BYTES,
        )
        .unwrap();
        assert_eq!(plan.batches().len(), 2);
        let first = patch_batch(&plan.batches()[0], RED);
        let second = patch_batch(&plan.batches()[1], BLUE);

        let command = GpuExactRasterRecoveryCommand::from_mirror_revision(
            &plan,
            vec![second.clone(), first.clone()],
        )
        .unwrap();
        assert_eq!(command.layer(), layer);
        assert_eq!(command.region_count(), 2);
        assert_eq!(command.pixel_count(), 1_024);

        let failure =
            GpuExactRasterRecoveryCommand::from_mirror_revision(&plan, vec![first.clone(), first])
                .unwrap_err();
        assert_eq!(
            failure.error,
            GpuExactRasterRecoveryMirrorError::DuplicateBatch(0)
        );
        assert_eq!(failure.batches.len(), 2);
    }

    #[test]
    fn transition_owns_exact_before_and_after_blocks() {
        let (first_plan, key) = one_tile_plan(1, true);
        let mut reconciler =
            GpuMirrorReconciler::new(32, 32, 32, DocumentRevision::INITIAL).unwrap();
        reconciler.register_plan(&first_plan).unwrap();
        reconciler
            .complete_batch(patch_batch(&first_plan.batches()[0], RED))
            .unwrap();
        let base = reconciler.snapshot();
        let (second_plan, _) = one_tile_plan(2, true);
        let transition = GpuExactRasterRecoveryTransition::from_mirror_revision(
            &base,
            &second_plan,
            vec![patch_batch(&second_plan.batches()[0], BLUE)],
        )
        .unwrap();

        assert_eq!(transition.revision(), DocumentRevision::from_raw(2));
        assert_eq!(transition.layer(), key.layer);
        assert!(transition.retained_byte_len() >= 2 * 32 * 32 * 16);
        let cloned = transition.clone();
        assert!(Arc::ptr_eq(
            &transition.after.tiles[0].regions[0].pixels,
            &cloned.after.tiles[0].regions[0].pixels
        ));

        let mut raster = base.raster_layer(key.layer).unwrap();
        replay_exact_raster_recovery_command(key.layer, &mut raster, transition.after()).unwrap();
        assert_eq!(raster.pixel(8, 8), Some(BLUE));
        replay_exact_raster_recovery_command(key.layer, &mut raster, transition.before()).unwrap();
        assert_eq!(raster.pixel(8, 8), Some(RED));
    }

    #[test]
    fn first_paint_transition_retains_absence_and_returns_batches_on_failure() {
        let base =
            crate::gpu_document_mirror::GpuCpuMirror::new(32, 32, 32, DocumentRevision::INITIAL)
                .unwrap()
                .snapshot();
        let (plan, key) = one_tile_plan(1, true);
        let transition = GpuExactRasterRecoveryTransition::from_mirror_revision(
            &base,
            &plan,
            vec![patch_batch(&plan.batches()[0], BLUE)],
        )
        .unwrap();
        let mut raster = base.raster_layer(key.layer).unwrap();
        replay_exact_raster_recovery_command(key.layer, &mut raster, transition.after()).unwrap();
        assert_eq!(raster.allocated_tile_count(), 1);
        replay_exact_raster_recovery_command(key.layer, &mut raster, transition.before()).unwrap();
        assert_eq!(raster.allocated_tile_count(), 0);

        let batches = vec![patch_batch(&plan.batches()[0], BLUE)];
        let failure = GpuExactRasterRecoveryTransition::from_mirror_revision(
            &crate::gpu_document_mirror::GpuCpuMirror::new(
                32,
                32,
                32,
                DocumentRevision::from_raw(1),
            )
            .unwrap()
            .snapshot(),
            &plan,
            batches,
        )
        .unwrap_err();
        assert_eq!(
            failure.error,
            GpuExactRasterRecoveryTransitionError::BaseRevisionMismatch {
                base: DocumentRevision::from_raw(1),
                source: DocumentRevision::INITIAL,
            }
        );
        assert_eq!(failure.batches.len(), 1);
        assert_eq!(failure.batches[0].regions()[0].pixels[0], BLUE);
    }

    #[test]
    fn padded_edge_blocks_clip_to_the_canvas_and_absence_removes_the_tile() {
        let layer_id = LayerId::from_raw(9);
        let edge = TileCoord::new(1, 1);
        let block = RectU32::from_xywh(0, 0, 16, 16).unwrap();
        let present = GpuExactRasterRecoveryCommand::from_regions(
            16,
            vec![region(layer_id, edge, block, true, BLUE)],
        )
        .unwrap();
        let mut layer = RasterLayer::new(18, 18, 16).unwrap();
        let replay = replay_exact_raster_recovery_command(layer_id, &mut layer, &present).unwrap();
        assert_eq!(replay.pixels_changed, 4);
        assert_eq!(layer.pixel(17, 17), Some(BLUE));
        assert_eq!(layer.allocated_tile_count(), 1);

        let absent = GpuExactRasterRecoveryCommand::from_regions(
            16,
            vec![region(
                layer_id,
                edge,
                RectU32::from_xywh(0, 0, 16, 16).unwrap(),
                false,
                RED,
            )],
        )
        .unwrap();
        let replay = replay_exact_raster_recovery_command(layer_id, &mut layer, &absent).unwrap();
        assert_eq!(replay.pixels_changed, 4);
        assert_eq!(replay.tiles_written, 0);
        assert_eq!(replay.tiles_removed, 1);
        assert_eq!(layer.allocated_tile_count(), 0);
    }
}
