use crate::{
    document::LayerId,
    gpu_atlas::{
        AtlasAllocation, AtlasError, AtlasLayout, AtlasPageBatch, LayerTileKey, SparseAtlasPlanner,
    },
    raster::{RectU32, TileCoord},
    round_geometry::RoundSweepGeometry,
    stroke::{RoundContact, RoundPathCommand, StrokeError},
};
use std::{collections::HashMap, error::Error, fmt};

const COVERAGE_FRINGE: f64 = 0.5;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RoundMaskInstance {
    from: [f32; 2],
    to: [f32; 2],
    radii: [f32; 2],
    clip_min: [f32; 2],
    clip_max: [f32; 2],
}

impl RoundMaskInstance {
    pub const fn from(self) -> [f32; 2] {
        self.from
    }

    pub const fn to(self) -> [f32; 2] {
        self.to
    }

    pub const fn radii(self) -> [f32; 2] {
        self.radii
    }

    pub const fn clip_min(self) -> [f32; 2] {
        self.clip_min
    }

    pub const fn clip_max(self) -> [f32; 2] {
        self.clip_max
    }

    pub fn coverage_at(self, physical_point: [f32; 2]) -> f32 {
        if physical_point[0] < self.clip_min[0]
            || physical_point[1] < self.clip_min[1]
            || physical_point[0] >= self.clip_max[0]
            || physical_point[1] >= self.clip_max[1]
        {
            return 0.0;
        }
        RoundSweepGeometry::new(
            RoundContact {
                center: self.from,
                radius: self.radii[0],
                elapsed_micros: 0,
            },
            RoundContact {
                center: self.to,
                radius: self.radii[1],
                elapsed_micros: 0,
            },
        )
        .expect("scheduled round contacts are valid")
        .coverage(physical_point)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundMaskTileDamage {
    pub key: LayerTileKey,
    pub local_damage: RectU32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoundMaskBatch {
    pages: Vec<AtlasPageBatch<RoundMaskInstance>>,
    touched_tiles: Vec<RoundMaskTileDamage>,
    allocations: Vec<AtlasAllocation>,
    commands: u32,
    primitives: u32,
}

impl RoundMaskBatch {
    pub fn pages(&self) -> &[AtlasPageBatch<RoundMaskInstance>] {
        &self.pages
    }

    pub fn touched_tiles(&self) -> &[RoundMaskTileDamage] {
        &self.touched_tiles
    }

    pub fn allocations(&self) -> &[AtlasAllocation] {
        &self.allocations
    }

    pub const fn commands(&self) -> u32 {
        self.commands
    }

    pub const fn primitives(&self) -> u32 {
        self.primitives
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

pub struct RoundMaskScheduler {
    canvas: [u32; 2],
    layer: LayerId,
    layout: AtlasLayout,
    active_contact: Option<RoundContact>,
}

impl RoundMaskScheduler {
    pub fn new(
        canvas: [u32; 2],
        layer: LayerId,
        layout: AtlasLayout,
    ) -> Result<Self, RoundMaskError> {
        if canvas[0] == 0 || canvas[1] == 0 {
            return Err(RoundMaskError::EmptyCanvas);
        }
        Ok(Self {
            canvas,
            layer,
            layout,
            active_contact: None,
        })
    }

    pub const fn layer(&self) -> LayerId {
        self.layer
    }

    pub const fn is_active(&self) -> bool {
        self.active_contact.is_some()
    }

    pub fn schedule(
        &mut self,
        atlas: &mut SparseAtlasPlanner,
        commands: &[RoundPathCommand],
    ) -> Result<RoundMaskBatch, RoundMaskError> {
        if atlas.layout() != self.layout {
            return Err(RoundMaskError::LayoutMismatch {
                expected: self.layout,
                actual: atlas.layout(),
            });
        }

        let mut next_active = self.active_contact;
        let mut primitives = Vec::new();
        for command in commands {
            match *command {
                RoundPathCommand::Begin(contact) => {
                    if next_active.is_some() {
                        return Err(StrokeError::PathAlreadyActive.into());
                    }
                    validate_contact(contact)?;
                    next_active = Some(contact);
                    primitives.push((contact, contact));
                }
                RoundPathCommand::Sweep { from, to } => {
                    validate_contact(from)?;
                    validate_contact(to)?;
                    if next_active.is_none() {
                        return Err(StrokeError::PathNotActive.into());
                    }
                    if next_active != Some(from) {
                        return Err(StrokeError::DiscontinuousPath.into());
                    }
                    next_active = Some(to);
                    primitives.push((from, to));
                }
                RoundPathCommand::End { at, .. } => {
                    validate_contact(at)?;
                    if next_active.is_none() {
                        return Err(StrokeError::PathNotActive.into());
                    }
                    if next_active != Some(at) {
                        return Err(StrokeError::DiscontinuousPath.into());
                    }
                    next_active = None;
                }
            }
        }

        let mut damage = HashMap::<LayerTileKey, RectU32>::new();
        let mut logical_work = Vec::new();
        for (from, to) in &primitives {
            self.append_primitive_work(*from, *to, &mut damage, &mut logical_work);
        }
        let mut keys: Vec<_> = damage.keys().copied().collect();
        keys.sort_by_key(|key| (key.layer.get(), key.tile.y, key.tile.x));
        let allocations = atlas.allocate_batch(keys.iter().copied())?;

        let mut physical_work = Vec::with_capacity(logical_work.len());
        for (key, from, to, valid_extent) in logical_work {
            let slot = atlas
                .slot(key)
                .expect("every scheduled tile was allocated in this transaction");
            let tile_origin = [
                key.tile.x * self.layout.tile_size(),
                key.tile.y * self.layout.tile_size(),
            ];
            let slot_origin = slot.origin();
            physical_work.push((
                key,
                RoundMaskInstance {
                    from: [
                        slot_origin[0] as f32 + from.center[0] - tile_origin[0] as f32,
                        slot_origin[1] as f32 + from.center[1] - tile_origin[1] as f32,
                    ],
                    to: [
                        slot_origin[0] as f32 + to.center[0] - tile_origin[0] as f32,
                        slot_origin[1] as f32 + to.center[1] - tile_origin[1] as f32,
                    ],
                    radii: [from.radius, to.radius],
                    clip_min: [slot_origin[0] as f32, slot_origin[1] as f32],
                    clip_max: [
                        (slot_origin[0] + valid_extent[0]) as f32,
                        (slot_origin[1] + valid_extent[1]) as f32,
                    ],
                },
            ));
        }
        let pages = atlas.batch_resident_work(physical_work)?;
        let touched_tiles = keys
            .into_iter()
            .map(|key| RoundMaskTileDamage {
                key,
                local_damage: damage[&key],
            })
            .collect();

        self.active_contact = next_active;
        Ok(RoundMaskBatch {
            pages,
            touched_tiles,
            allocations,
            commands: commands.len() as u32,
            primitives: primitives.len() as u32,
        })
    }

    fn append_primitive_work(
        &self,
        from: RoundContact,
        to: RoundContact,
        damage: &mut HashMap<LayerTileKey, RectU32>,
        work: &mut Vec<(LayerTileKey, RoundContact, RoundContact, [u32; 2])>,
    ) {
        let min_x = ((from.center[0] as f64 - from.radius as f64)
            .min(to.center[0] as f64 - to.radius as f64)
            - COVERAGE_FRINGE)
            .floor()
            .clamp(0.0, self.canvas[0] as f64) as u32;
        let min_y = ((from.center[1] as f64 - from.radius as f64)
            .min(to.center[1] as f64 - to.radius as f64)
            - COVERAGE_FRINGE)
            .floor()
            .clamp(0.0, self.canvas[1] as f64) as u32;
        let max_x = ((from.center[0] as f64 + from.radius as f64)
            .max(to.center[0] as f64 + to.radius as f64)
            + COVERAGE_FRINGE)
            .ceil()
            .clamp(0.0, self.canvas[0] as f64) as u32;
        let max_y = ((from.center[1] as f64 + from.radius as f64)
            .max(to.center[1] as f64 + to.radius as f64)
            + COVERAGE_FRINGE)
            .ceil()
            .clamp(0.0, self.canvas[1] as f64) as u32;
        if min_x >= max_x || min_y >= max_y {
            return;
        }

        let tile_size = self.layout.tile_size();
        for tile_y in min_y / tile_size..=(max_y - 1) / tile_size {
            for tile_x in min_x / tile_size..=(max_x - 1) / tile_size {
                let tile_origin = [tile_x * tile_size, tile_y * tile_size];
                let valid_extent = [
                    tile_size.min(self.canvas[0] - tile_origin[0]),
                    tile_size.min(self.canvas[1] - tile_origin[1]),
                ];
                let local_damage = RectU32::from_min_max(
                    min_x.max(tile_origin[0]) - tile_origin[0],
                    min_y.max(tile_origin[1]) - tile_origin[1],
                    max_x.min(tile_origin[0] + valid_extent[0]) - tile_origin[0],
                    max_y.min(tile_origin[1] + valid_extent[1]) - tile_origin[1],
                )
                .expect("every enumerated tile intersects the clipped round bounds");
                let key = LayerTileKey::new(self.layer, TileCoord::new(tile_x, tile_y));
                damage
                    .entry(key)
                    .and_modify(|existing| *existing = existing.union(local_damage))
                    .or_insert(local_damage);
                work.push((key, from, to, valid_extent));
            }
        }
    }
}

fn validate_contact(contact: RoundContact) -> Result<(), RoundMaskError> {
    if contact.center.iter().any(|value| !value.is_finite())
        || !contact.radius.is_finite()
        || contact.radius <= 0.0
    {
        return Err(StrokeError::InvalidContact.into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum RoundMaskError {
    EmptyCanvas,
    LayoutMismatch {
        expected: AtlasLayout,
        actual: AtlasLayout,
    },
    Stroke(StrokeError),
    Atlas(AtlasError),
}

impl fmt::Display for RoundMaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCanvas => write!(formatter, "round mask canvas is empty"),
            Self::LayoutMismatch { expected, actual } => write!(
                formatter,
                "round mask atlas layout mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::Stroke(error) => error.fmt(formatter),
            Self::Atlas(error) => error.fmt(formatter),
        }
    }
}

impl Error for RoundMaskError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Stroke(error) => Some(error),
            Self::Atlas(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StrokeError> for RoundMaskError {
    fn from(error: StrokeError) -> Self {
        Self::Stroke(error)
    }
}

impl From<AtlasError> for RoundMaskError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(center: [f32; 2], radius: f32, elapsed_micros: u64) -> RoundContact {
        RoundContact {
            center,
            radius,
            elapsed_micros,
        }
    }

    fn scheduler(layout: AtlasLayout, canvas: [u32; 2]) -> RoundMaskScheduler {
        RoundMaskScheduler::new(canvas, LayerId::from_raw(3), layout).unwrap()
    }

    #[test]
    fn a_dot_allocates_and_translates_one_tile() {
        let layout = AtlasLayout::document_default();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let mut scheduler = scheduler(layout, [4_096, 4_096]);
        let dot = contact([140.0, 150.0], 4.0, 0);
        let batch = scheduler
            .schedule(&mut atlas, &[RoundPathCommand::Begin(dot)])
            .unwrap();
        assert_eq!(batch.primitives(), 1);
        assert_eq!(batch.touched_tiles().len(), 1);
        assert_eq!(batch.touched_tiles()[0].key.tile, TileCoord::new(1, 1));
        assert_eq!(batch.pages().len(), 1);
        let instance = batch.pages()[0].work[0].payload;
        assert_eq!(instance.from(), [12.0, 22.0]);
        assert_eq!(instance.to(), [12.0, 22.0]);
        assert_eq!(instance.coverage_at([12.0, 22.0]), 1.0);
    }

    #[test]
    fn one_wide_sweep_is_one_page_batch_across_many_tiles() {
        let layout = AtlasLayout::document_default();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let mut scheduler = scheduler(layout, [4_096, 4_096]);
        let first = contact([20.0, 64.0], 10.0, 0);
        let last = contact([1_100.0, 64.0], 10.0, 1);
        let batch = scheduler
            .schedule(
                &mut atlas,
                &[
                    RoundPathCommand::Begin(first),
                    RoundPathCommand::Sweep {
                        from: first,
                        to: last,
                    },
                ],
            )
            .unwrap();
        assert_eq!(batch.touched_tiles().len(), 9);
        assert_eq!(batch.pages().len(), 1);
        assert_eq!(batch.pages()[0].work.len(), 10);
    }

    #[test]
    fn page_count_grows_only_after_sixty_four_resident_tiles() {
        let layout = AtlasLayout::document_default();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let mut scheduler = scheduler(layout, [16_384, 256]);
        let first = contact([1.0, 64.0], 1.0, 0);
        let last = contact([8_300.0, 64.0], 1.0, 1);
        let batch = scheduler
            .schedule(
                &mut atlas,
                &[
                    RoundPathCommand::Begin(first),
                    RoundPathCommand::Sweep {
                        from: first,
                        to: last,
                    },
                ],
            )
            .unwrap();
        assert_eq!(batch.touched_tiles().len(), 65);
        assert_eq!(batch.pages().len(), 2);
        assert_eq!(atlas.retained_page_count(), 2);
    }

    #[test]
    fn incremental_commands_preserve_scheduler_continuity() {
        let layout = AtlasLayout::document_default();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let mut scheduler = scheduler(layout, [512, 512]);
        let first = contact([10.0, 10.0], 2.0, 0);
        let second = contact([30.0, 10.0], 3.0, 1);
        scheduler
            .schedule(&mut atlas, &[RoundPathCommand::Begin(first)])
            .unwrap();
        assert!(scheduler.is_active());
        scheduler
            .schedule(
                &mut atlas,
                &[RoundPathCommand::Sweep {
                    from: first,
                    to: second,
                }],
            )
            .unwrap();
        scheduler
            .schedule(
                &mut atlas,
                &[RoundPathCommand::End {
                    at: second,
                    elapsed_micros: 2,
                }],
            )
            .unwrap();
        assert!(!scheduler.is_active());
    }

    #[test]
    fn invalid_batch_changes_neither_scheduler_nor_atlas() {
        let layout = AtlasLayout::document_default();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let mut scheduler = scheduler(layout, [512, 512]);
        let first = contact([10.0, 10.0], 2.0, 0);
        let wrong = contact([20.0, 10.0], 2.0, 1);
        let result = scheduler.schedule(
            &mut atlas,
            &[
                RoundPathCommand::Begin(first),
                RoundPathCommand::End {
                    at: wrong,
                    elapsed_micros: 2,
                },
            ],
        );
        assert_eq!(result, Err(StrokeError::DiscontinuousPath.into()));
        assert!(!scheduler.is_active());
        assert_eq!(atlas.resident_tile_count(), 0);
        assert_eq!(atlas.retained_page_count(), 0);
    }

    #[test]
    fn edge_tiles_clip_instance_and_damage_to_the_canvas() {
        let layout = AtlasLayout::new(256, 128, 1).unwrap();
        let mut atlas = SparseAtlasPlanner::new(layout);
        let mut scheduler = scheduler(layout, [150, 140]);
        let edge = contact([149.0, 139.0], 10.0, 0);
        let batch = scheduler
            .schedule(&mut atlas, &[RoundPathCommand::Begin(edge)])
            .unwrap();
        assert_eq!(batch.touched_tiles().len(), 1);
        assert_eq!(batch.touched_tiles()[0].key.tile, TileCoord::new(1, 1));
        assert_eq!(batch.touched_tiles()[0].local_damage.max_x(), 22);
        assert_eq!(batch.touched_tiles()[0].local_damage.max_y(), 12);
        let instance = batch.pages()[0].work[0].payload;
        assert_eq!(instance.clip_max(), [22.0, 12.0]);
        assert_eq!(instance.coverage_at([21.0, 11.0]), 1.0);
        assert_eq!(instance.coverage_at([22.0, 11.0]), 0.0);
    }

    #[test]
    fn layout_mismatch_fails_before_allocation() {
        let expected = AtlasLayout::document_default();
        let actual = AtlasLayout::new(512, 128, 1).unwrap();
        let mut atlas = SparseAtlasPlanner::new(actual);
        let mut scheduler = scheduler(expected, [512, 512]);
        let result = scheduler.schedule(
            &mut atlas,
            &[RoundPathCommand::Begin(contact([10.0; 2], 2.0, 0))],
        );
        assert!(matches!(result, Err(RoundMaskError::LayoutMismatch { .. })));
        assert_eq!(atlas.resident_tile_count(), 0);
    }
}
