use crate::{
    document_history::{DocumentEdit, DocumentHistory},
    raster::{
        Damage, DerivedContentChange, LinearRgba, RasterError, RasterLayer, RectU32, TileCoord,
    },
};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt, mem,
};

pub use crate::document_history::MAX_DOCUMENT_HISTORY_ENTRIES;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentRevision(u64);

impl DocumentRevision {
    pub const INITIAL: Self = Self(0);

    pub const fn get(self) -> u64 {
        self.0
    }

    fn advance(&mut self) {
        self.0 = self
            .0
            .checked_add(1)
            .expect("document revision space is exhausted");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LayerId(u64);

impl LayerId {
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) const fn from_raw(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentLayer {
    id: LayerId,
    name: String,
    visible: bool,
    opacity: f32,
}

impl DocumentLayer {
    pub const fn id(&self) -> LayerId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn visible(&self) -> bool {
        self.visible
    }

    pub const fn opacity(&self) -> f32 {
        self.opacity
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompositeStats {
    pub tile_updates: u64,
    pub pixels_composited: u64,
    pub source_tile_reads: u64,
    pub source_pixel_reads: u64,
}

pub struct Document {
    width: u32,
    height: u32,
    tile_size: u32,
    layers: Vec<DocumentLayer>,
    rasters: HashMap<LayerId, RasterLayer>,
    active_layer: LayerId,
    next_layer_id: u64,
    composite: RasterLayer,
    composite_stats: CompositeStats,
    history: DocumentHistory,
    revision: DocumentRevision,
}

pub(crate) struct DocumentLayerParts {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub raster: RasterLayer,
}

impl Document {
    pub fn new(width: u32, height: u32, tile_size: u32) -> Result<Self, DocumentError> {
        let first_id = LayerId(1);
        let first_raster = RasterLayer::new(width, height, tile_size)?;
        Ok(Self {
            width,
            height,
            tile_size,
            layers: vec![DocumentLayer {
                id: first_id,
                name: "Layer 1".to_owned(),
                visible: true,
                opacity: 1.0,
            }],
            rasters: HashMap::from([(first_id, first_raster)]),
            active_layer: first_id,
            next_layer_id: 2,
            composite: RasterLayer::new(width, height, tile_size)?,
            composite_stats: CompositeStats::default(),
            history: DocumentHistory::default(),
            revision: DocumentRevision::INITIAL,
        })
    }

    pub fn from_flattened(
        raster: RasterLayer,
        name: impl Into<String>,
    ) -> Result<Self, DocumentError> {
        let name = validated_name(name.into())?;
        let width = raster.width();
        let height = raster.height();
        let tile_size = raster.tile_size();
        let composite = clone_raster(&raster)?;
        let first_id = LayerId(1);
        Ok(Self {
            width,
            height,
            tile_size,
            layers: vec![DocumentLayer {
                id: first_id,
                name,
                visible: true,
                opacity: 1.0,
            }],
            rasters: HashMap::from([(first_id, raster)]),
            active_layer: first_id,
            next_layer_id: 2,
            composite,
            composite_stats: CompositeStats::default(),
            history: DocumentHistory::default(),
            revision: DocumentRevision::INITIAL,
        })
    }

    pub(crate) fn from_layer_parts(
        width: u32,
        height: u32,
        tile_size: u32,
        active_layer: LayerId,
        parts: Vec<DocumentLayerParts>,
    ) -> Result<Self, DocumentError> {
        if parts.is_empty() {
            return Err(DocumentError::NoLayers);
        }
        let mut ids = HashSet::new();
        let mut layers = Vec::with_capacity(parts.len());
        let mut rasters = HashMap::with_capacity(parts.len());
        let mut max_id = 0;
        for part in parts {
            if part.id.get() == 0 || !ids.insert(part.id) {
                return Err(DocumentError::InvalidLayerId(part.id));
            }
            if part.raster.width() != width
                || part.raster.height() != height
                || part.raster.tile_size() != tile_size
            {
                return Err(DocumentError::LayerGeometryMismatch(part.id));
            }
            let name = validated_name(part.name)?;
            if !part.opacity.is_finite() || !(0.0..=1.0).contains(&part.opacity) {
                return Err(DocumentError::InvalidOpacity(part.opacity));
            }
            max_id = max_id.max(part.id.get());
            layers.push(DocumentLayer {
                id: part.id,
                name,
                visible: part.visible,
                opacity: part.opacity,
            });
            rasters.insert(part.id, part.raster);
        }
        if !ids.contains(&active_layer) {
            return Err(DocumentError::LayerNotFound(active_layer));
        }
        let next_layer_id = max_id
            .checked_add(1)
            .ok_or(DocumentError::LayerIdExhausted)?;
        let mut document = Self {
            width,
            height,
            tile_size,
            layers,
            rasters,
            active_layer,
            next_layer_id,
            composite: RasterLayer::new(width, height, tile_size)?,
            composite_stats: CompositeStats::default(),
            history: DocumentHistory::default(),
            revision: DocumentRevision::INITIAL,
        };
        document.recompose_all()?;
        document.reset_composite_stats();
        Ok(document)
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn tile_size(&self) -> u32 {
        self.tile_size
    }

    pub fn layers(&self) -> &[DocumentLayer] {
        &self.layers
    }

    pub fn layer(&self, id: LayerId) -> Option<&DocumentLayer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    pub fn layer_raster(&self, id: LayerId) -> Option<&RasterLayer> {
        self.layer_index(id)?;
        self.rasters.get(&id)
    }

    pub const fn active_layer_id(&self) -> LayerId {
        self.active_layer
    }

    pub fn active_layer_index(&self) -> usize {
        self.layer_index(self.active_layer)
            .expect("the active layer must belong to the document")
    }

    pub fn active_layer(&self) -> &RasterLayer {
        self.rasters
            .get(&self.active_layer)
            .expect("the active layer must have a raster payload")
    }

    pub fn active_layer_mut(&mut self) -> &mut RasterLayer {
        self.rasters
            .get_mut(&self.active_layer)
            .expect("the active layer must have a raster payload")
    }

    pub const fn composite(&self) -> &RasterLayer {
        &self.composite
    }

    pub const fn composite_stats(&self) -> CompositeStats {
        self.composite_stats
    }

    pub fn reset_composite_stats(&mut self) {
        self.composite_stats = CompositeStats::default();
    }

    pub fn undo_depth(&self) -> usize {
        self.history.undo_depth()
    }

    pub fn redo_depth(&self) -> usize {
        self.history.redo_depth()
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub fn set_active_layer(&mut self, id: LayerId) -> Result<(), DocumentError> {
        self.require_layer(id)?;
        self.active_layer = id;
        Ok(())
    }

    pub fn create_layer(&mut self, name: impl Into<String>) -> Result<LayerId, DocumentError> {
        let name = validated_name(name.into())?;
        let raster = RasterLayer::new(self.width, self.height, self.tile_size)?;
        let before_active = self.active_layer;
        let id = self.allocate_layer_id();
        let layer = DocumentLayer {
            id,
            name,
            visible: true,
            opacity: 1.0,
        };
        let insertion = self.active_layer_index() + 1;
        self.layers.insert(insertion, layer.clone());
        assert!(self.rasters.insert(id, raster).is_none());
        self.active_layer = id;
        self.record_edit(DocumentEdit::LayerPresence {
            layer,
            index: insertion,
            before_active,
            after_active: id,
            present_after: true,
        });
        Ok(id)
    }

    pub fn insert_raster_layer(
        &mut self,
        name: impl Into<String>,
        mut raster: RasterLayer,
    ) -> Result<(LayerId, Damage), DocumentError> {
        if raster.width() != self.width
            || raster.height() != self.height
            || raster.tile_size() != self.tile_size
        {
            return Err(DocumentError::ImportedLayerGeometryMismatch);
        }
        let name = validated_name(name.into())?;
        raster.clear_history();
        let affected: Vec<_> = raster.allocated_tile_coords().collect();
        let before_active = self.active_layer;
        let id = self.allocate_layer_id();
        let layer = DocumentLayer {
            id,
            name,
            visible: true,
            opacity: 1.0,
        };
        let insertion = self.active_layer_index() + 1;
        self.layers.insert(insertion, layer.clone());
        assert!(self.rasters.insert(id, raster).is_none());
        self.active_layer = id;
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        self.record_edit(DocumentEdit::LayerPresence {
            layer,
            index: insertion,
            before_active,
            after_active: id,
            present_after: true,
        });
        Ok((id, damage))
    }

    pub fn duplicate_layer(&mut self, id: LayerId) -> Result<(LayerId, Damage), DocumentError> {
        let source_index = self.require_layer(id)?;
        let source = &self.layers[source_index];
        let name = format!("{} copy", source.name);
        let visible = source.visible;
        let opacity = source.opacity;
        let source_raster = self
            .rasters
            .get(&id)
            .ok_or(DocumentError::LayerStorageMissing(id))?;
        let raster = clone_raster(source_raster)?;
        let affected = coords_for_raster(source_raster);
        let before_active = self.active_layer;
        let duplicate_id = self.allocate_layer_id();
        let duplicate = DocumentLayer {
            id: duplicate_id,
            name,
            visible,
            opacity,
        };
        let insertion = source_index + 1;
        self.layers.insert(insertion, duplicate.clone());
        assert!(self.rasters.insert(duplicate_id, raster).is_none());
        self.active_layer = duplicate_id;
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        self.record_edit(DocumentEdit::LayerPresence {
            layer: duplicate,
            index: insertion,
            before_active,
            after_active: duplicate_id,
            present_after: true,
        });
        Ok((duplicate_id, damage))
    }

    pub fn delete_layer(&mut self, id: LayerId) -> Result<Damage, DocumentError> {
        if self.layers.len() == 1 {
            return Err(DocumentError::CannotDeleteLastLayer);
        }
        let index = self.require_layer(id)?;
        let raster = self
            .rasters
            .get(&id)
            .ok_or(DocumentError::LayerStorageMissing(id))?;
        let affected = coords_for_raster(raster);
        let before_active = self.active_layer;
        let removed = self.layers.remove(index);
        if self.active_layer == id {
            self.active_layer = self.layers[index.min(self.layers.len() - 1)].id;
        }
        let after_active = self.active_layer;
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        self.record_edit(DocumentEdit::LayerPresence {
            layer: removed,
            index,
            before_active,
            after_active,
            present_after: false,
        });
        Ok(damage)
    }

    pub fn rename_layer(
        &mut self,
        id: LayerId,
        name: impl Into<String>,
    ) -> Result<(), DocumentError> {
        let index = self.require_layer(id)?;
        let after = validated_name(name.into())?;
        if self.layers[index].name == after {
            return Ok(());
        }
        let before = mem::replace(&mut self.layers[index].name, after.clone());
        self.record_edit(DocumentEdit::Rename {
            layer: id,
            before,
            after,
        });
        Ok(())
    }

    pub fn set_layer_visibility(
        &mut self,
        id: LayerId,
        visible: bool,
    ) -> Result<Damage, DocumentError> {
        let index = self.require_layer(id)?;
        if self.layers[index].visible == visible {
            return Ok(Damage::default());
        }
        let affected = self
            .rasters
            .get(&id)
            .map(coords_for_raster)
            .ok_or(DocumentError::LayerStorageMissing(id))?;
        let before = self.layers[index].visible;
        self.layers[index].visible = visible;
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        self.record_edit(DocumentEdit::Visibility {
            layer: id,
            before,
            after: visible,
        });
        Ok(damage)
    }

    pub fn set_layer_opacity(
        &mut self,
        id: LayerId,
        opacity: f32,
    ) -> Result<Damage, DocumentError> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(DocumentError::InvalidOpacity(opacity));
        }
        let index = self.require_layer(id)?;
        if self.layers[index].opacity == opacity {
            return Ok(Damage::default());
        }
        let affected = self
            .rasters
            .get(&id)
            .map(coords_for_raster)
            .ok_or(DocumentError::LayerStorageMissing(id))?;
        let before = self.layers[index].opacity;
        self.layers[index].opacity = opacity;
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        self.record_edit(DocumentEdit::Opacity {
            layer: id,
            before,
            after: opacity,
        });
        Ok(damage)
    }

    pub fn move_layer(
        &mut self,
        id: LayerId,
        destination_index: usize,
    ) -> Result<Damage, DocumentError> {
        let source_index = self.require_layer(id)?;
        if destination_index >= self.layers.len() {
            return Err(DocumentError::LayerIndexOutOfBounds(destination_index));
        }
        if source_index == destination_index {
            return Ok(Damage::default());
        }
        let affected = self.all_content_coords();
        let layer = self.layers.remove(source_index);
        self.layers.insert(destination_index, layer);
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        self.record_edit(DocumentEdit::Move {
            layer: id,
            before: source_index,
            after: destination_index,
        });
        Ok(damage)
    }

    pub fn record_active_raster_edit(&mut self) -> Result<(), DocumentError> {
        let layer = self.active_layer;
        self.require_layer(layer)?;
        let raster = self
            .rasters
            .get(&layer)
            .ok_or(DocumentError::LayerStorageMissing(layer))?;
        if raster.active_gesture_id().is_some() {
            return Err(DocumentError::RasterEditStillActive(layer));
        }
        let tracked = self
            .history
            .undo_edits()
            .filter(|edit| matches!(edit, DocumentEdit::Raster { layer: edit_layer } if *edit_layer == layer))
            .count();
        let actual = raster.undo_depth();
        if actual != tracked + 1 {
            return Err(DocumentError::RasterHistoryMismatch {
                layer,
                tracked,
                actual,
            });
        }
        self.record_edit(DocumentEdit::Raster { layer });
        Ok(())
    }

    pub fn undo(&mut self) -> Result<Option<Damage>, DocumentError> {
        let Some(mut edit) = self.history.pop_undo() else {
            return Ok(None);
        };
        match self.apply_edit(&mut edit, false) {
            Ok(damage) => {
                self.history.finish_undo(edit);
                self.revision.advance();
                Ok(Some(damage))
            }
            Err(error) => {
                self.history.restore_undo(edit);
                Err(error)
            }
        }
    }

    pub fn redo(&mut self) -> Result<Option<Damage>, DocumentError> {
        let Some(mut edit) = self.history.pop_redo() else {
            return Ok(None);
        };
        match self.apply_edit(&mut edit, true) {
            Ok(damage) => {
                self.history.finish_redo(edit);
                self.revision.advance();
                Ok(Some(damage))
            }
            Err(error) => {
                self.history.restore_redo(edit);
                Err(error)
            }
        }
    }

    pub fn recompose_all(&mut self) -> Result<Damage, DocumentError> {
        let affected = self.all_content_coords();
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        Ok(damage)
    }

    pub fn recompose_damage(&mut self, damage: &Damage) -> Result<Damage, DocumentError> {
        if damage.is_empty() {
            return Ok(Damage::default());
        }

        let mut recomposed = Damage::default();
        for (coord, global_region) in damage.tile_regions() {
            let tile_bounds = self
                .composite
                .tile_bounds(coord)
                .ok_or(RasterError::TileOutOfBounds(coord))?;
            let local_min_x = global_region.min_x() - tile_bounds.min_x();
            let local_min_y = global_region.min_y() - tile_bounds.min_y();
            let local_max_x = global_region.max_x() - tile_bounds.min_x();
            let local_max_y = global_region.max_y() - tile_bounds.min_y();
            let local_region =
                RectU32::from_min_max(local_min_x, local_min_y, local_max_x, local_max_y)
                    .expect("nonempty global damage produces nonempty local damage");
            let stride = self.tile_size as usize;
            let mut sources = Vec::new();
            for layer in self
                .layers
                .iter()
                .filter(|layer| layer.visible && layer.opacity > 0.0)
            {
                let raster = self
                    .rasters
                    .get(&layer.id)
                    .ok_or(DocumentError::LayerStorageMissing(layer.id))?;
                if let Some(tile) = raster.tile(coord) {
                    sources.push((layer.opacity, tile));
                }
            }
            self.composite_stats.source_tile_reads = self
                .composite_stats
                .source_tile_reads
                .saturating_add(sources.len() as u64);
            self.composite
                .edit_tile_derived(coord, local_region, |tile| {
                    let pixels = tile.pixels_mut();
                    let mut added_bounds = None;
                    let mut emptied_bounds = None;
                    for y in local_min_y..local_max_y {
                        let row = y as usize * stride;
                        for x in local_min_x..local_max_x {
                            let index = row + x as usize;
                            let previous = pixels[index];
                            let mut destination = LinearRgba::TRANSPARENT;
                            for (opacity, source_tile) in &sources {
                                let source = source_tile.pixels()[index];
                                let source_alpha = source.a * *opacity;
                                let keep_destination = 1.0 - source_alpha;
                                destination = LinearRgba::premultiplied(
                                    source.r * *opacity + destination.r * keep_destination,
                                    source.g * *opacity + destination.g * keep_destination,
                                    source.b * *opacity + destination.b * keep_destination,
                                    source_alpha + destination.a * keep_destination,
                                );
                            }
                            if previous.a == 0.0 && destination.a != 0.0 {
                                include_pixel(&mut added_bounds, x, y);
                            } else if previous.a != 0.0 && destination.a == 0.0 {
                                include_pixel(&mut emptied_bounds, x, y);
                            }
                            pixels[index] = destination;
                        }
                    }
                    DerivedContentChange {
                        added_bounds,
                        emptied_bounds,
                    }
                })?;
            let area = global_region.area();
            self.composite_stats.pixels_composited =
                self.composite_stats.pixels_composited.saturating_add(area);
            self.composite_stats.source_pixel_reads = self
                .composite_stats
                .source_pixel_reads
                .saturating_add(area.saturating_mul(sources.len() as u64));
            self.composite_stats.tile_updates = self.composite_stats.tile_updates.saturating_add(1);
            recomposed.add(coord, global_region);
        }
        Ok(recomposed)
    }

    fn record_edit(&mut self, edit: DocumentEdit) {
        self.clear_raster_redo_history();
        if let Some(DocumentEdit::Raster { layer }) = self.history.record(edit) {
            let raster = self
                .rasters
                .get_mut(&layer)
                .expect("a retained raster edit must retain its stable layer");
            assert!(
                raster.discard_oldest_undo(),
                "a retained raster edit must own one local memento"
            );
        }
        self.prune_detached_rasters();
        self.revision.advance();
    }

    fn clear_raster_redo_history(&mut self) {
        for raster in self.rasters.values_mut() {
            raster.clear_redo_history();
        }
    }

    fn apply_edit(
        &mut self,
        edit: &mut DocumentEdit,
        apply_after: bool,
    ) -> Result<Damage, DocumentError> {
        match edit {
            DocumentEdit::Raster { layer } => {
                self.require_layer(*layer)?;
                let raster = self
                    .rasters
                    .get_mut(layer)
                    .ok_or(DocumentError::LayerStorageMissing(*layer))?;
                let damage = if apply_after {
                    raster.redo()
                } else {
                    raster.undo()
                }
                .ok_or(DocumentError::RasterHistoryMismatch {
                    layer: *layer,
                    tracked: 1,
                    actual: 0,
                })?;
                self.recompose_damage(&damage)
            }
            DocumentEdit::LayerPresence {
                layer,
                index,
                before_active,
                after_active,
                present_after,
            } => {
                let layer_id = layer.id;
                let should_be_present = if apply_after {
                    *present_after
                } else {
                    !*present_after
                };
                let target_active = if apply_after {
                    *after_active
                } else {
                    *before_active
                };
                let affected = if should_be_present {
                    if self.layer_index(layer_id).is_some()
                        || !self.rasters.contains_key(&layer_id)
                        || *index > self.layers.len()
                    {
                        return Err(DocumentError::HistoryInvariant);
                    }
                    let affected = self
                        .rasters
                        .get(&layer_id)
                        .map(coords_for_raster)
                        .ok_or(DocumentError::LayerStorageMissing(layer_id))?;
                    self.layers.insert(*index, layer.clone());
                    affected
                } else {
                    let current = self.require_layer(layer_id)?;
                    if current != *index || self.layers[current] != *layer {
                        return Err(DocumentError::HistoryInvariant);
                    }
                    let affected = self
                        .rasters
                        .get(&layer_id)
                        .map(coords_for_raster)
                        .ok_or(DocumentError::LayerStorageMissing(layer_id))?;
                    self.layers.remove(current);
                    affected
                };
                self.require_layer(target_active)?;
                self.active_layer = target_active;
                let damage = self.damage_for_coords(affected);
                self.recompose_damage(&damage)
            }
            DocumentEdit::Rename {
                layer,
                before,
                after,
            } => {
                let index = self.require_layer(*layer)?;
                self.layers[index].name = if apply_after {
                    after.clone()
                } else {
                    before.clone()
                };
                Ok(Damage::default())
            }
            DocumentEdit::Visibility {
                layer,
                before,
                after,
            } => {
                let index = self.require_layer(*layer)?;
                let affected = self
                    .rasters
                    .get(layer)
                    .map(coords_for_raster)
                    .ok_or(DocumentError::LayerStorageMissing(*layer))?;
                self.layers[index].visible = if apply_after { *after } else { *before };
                let damage = self.damage_for_coords(affected);
                self.recompose_damage(&damage)
            }
            DocumentEdit::Opacity {
                layer,
                before,
                after,
            } => {
                let index = self.require_layer(*layer)?;
                let affected = self
                    .rasters
                    .get(layer)
                    .map(coords_for_raster)
                    .ok_or(DocumentError::LayerStorageMissing(*layer))?;
                self.layers[index].opacity = if apply_after { *after } else { *before };
                let damage = self.damage_for_coords(affected);
                self.recompose_damage(&damage)
            }
            DocumentEdit::Move {
                layer,
                before,
                after,
            } => {
                let source = self.require_layer(*layer)?;
                let target = if apply_after { *after } else { *before };
                if target >= self.layers.len() {
                    return Err(DocumentError::HistoryInvariant);
                }
                let affected = self.all_content_coords();
                let layer = self.layers.remove(source);
                self.layers.insert(target, layer);
                let damage = self.damage_for_coords(affected);
                self.recompose_damage(&damage)
            }
        }
    }

    fn allocate_layer_id(&mut self) -> LayerId {
        let id = LayerId(self.next_layer_id);
        self.next_layer_id = self.next_layer_id.wrapping_add(1).max(1);
        id
    }

    fn layer_index(&self, id: LayerId) -> Option<usize> {
        self.layers.iter().position(|layer| layer.id == id)
    }

    fn require_layer(&self, id: LayerId) -> Result<usize, DocumentError> {
        self.layer_index(id).ok_or(DocumentError::LayerNotFound(id))
    }

    fn all_content_coords(&self) -> Vec<TileCoord> {
        let mut coords = HashSet::new();
        coords.extend(self.composite.allocated_tile_coords());
        for layer in &self.layers {
            let raster = self
                .rasters
                .get(&layer.id)
                .expect("every present layer must have a raster payload");
            coords.extend(raster.allocated_tile_coords());
        }
        coords.into_iter().collect()
    }

    fn prune_detached_rasters(&mut self) {
        let mut retained: HashSet<_> = self.layers.iter().map(|layer| layer.id).collect();
        retained.extend(self.history.referenced_layer_ids());
        self.rasters.retain(|id, _| retained.contains(id));
    }

    fn damage_for_coords(&self, coords: impl IntoIterator<Item = TileCoord>) -> Damage {
        let mut damage = Damage::default();
        for coord in coords {
            if let Some(bounds) = self.composite.tile_bounds(coord) {
                damage.add(coord, bounds);
            }
        }
        damage
    }
}

fn include_pixel(bounds: &mut Option<RectU32>, x: u32, y: u32) {
    let pixel = RectU32::from_xywh(x, y, 1, 1).expect("one pixel is a valid rectangle");
    *bounds = Some(match *bounds {
        Some(existing) => existing.union(pixel),
        None => pixel,
    });
}

fn coords_for_raster(raster: &RasterLayer) -> Vec<TileCoord> {
    raster.allocated_tile_coords().collect()
}

fn clone_raster(source: &RasterLayer) -> Result<RasterLayer, RasterError> {
    let mut cloned = RasterLayer::new(source.width(), source.height(), source.tile_size())?;
    for coord in source.allocated_tile_coords() {
        let pixels = source
            .tile(coord)
            .expect("allocated coordinates must resolve to tiles")
            .pixels()
            .to_vec()
            .into_boxed_slice();
        cloned.restore_tile(coord, pixels)?;
    }
    Ok(cloned)
}

fn validated_name(name: String) -> Result<String, DocumentError> {
    if name.trim().is_empty() {
        Err(DocumentError::EmptyLayerName)
    } else {
        Ok(name)
    }
}

#[derive(Debug)]
pub enum DocumentError {
    Raster(RasterError),
    LayerNotFound(LayerId),
    LayerStorageMissing(LayerId),
    LayerIndexOutOfBounds(usize),
    CannotDeleteLastLayer,
    InvalidOpacity(f32),
    EmptyLayerName,
    NoLayers,
    InvalidLayerId(LayerId),
    LayerIdExhausted,
    LayerGeometryMismatch(LayerId),
    ImportedLayerGeometryMismatch,
    RasterEditStillActive(LayerId),
    RasterHistoryMismatch {
        layer: LayerId,
        tracked: usize,
        actual: usize,
    },
    HistoryInvariant,
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raster(error) => error.fmt(formatter),
            Self::LayerNotFound(id) => write!(formatter, "layer {} does not exist", id.get()),
            Self::LayerStorageMissing(id) => {
                write!(formatter, "layer {} has no raster payload", id.get())
            }
            Self::LayerIndexOutOfBounds(index) => {
                write!(formatter, "layer index {index} is out of bounds")
            }
            Self::CannotDeleteLastLayer => write!(formatter, "the last layer cannot be deleted"),
            Self::InvalidOpacity(opacity) => write!(formatter, "invalid layer opacity {opacity}"),
            Self::EmptyLayerName => write!(formatter, "layer names cannot be empty"),
            Self::NoLayers => write!(formatter, "a document must contain at least one layer"),
            Self::InvalidLayerId(id) => {
                write!(formatter, "invalid or duplicate layer ID {}", id.get())
            }
            Self::LayerIdExhausted => write!(formatter, "layer IDs are exhausted"),
            Self::LayerGeometryMismatch(id) => {
                write!(formatter, "layer {} has incompatible geometry", id.get())
            }
            Self::ImportedLayerGeometryMismatch => {
                write!(
                    formatter,
                    "imported raster has incompatible document geometry"
                )
            }
            Self::RasterEditStillActive(layer) => {
                write!(
                    formatter,
                    "layer {} raster edit is still active",
                    layer.get()
                )
            }
            Self::RasterHistoryMismatch {
                layer,
                tracked,
                actual,
            } => write!(
                formatter,
                "layer {} has {actual} local undo entries but {tracked} tracked document edits",
                layer.get()
            ),
            Self::HistoryInvariant => write!(formatter, "document history invariant failed"),
        }
    }
}

impl Error for DocumentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Raster(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RasterError> for DocumentError {
    fn from(value: RasterError) -> Self {
        Self::Raster(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(r: f32, g: f32, b: f32, a: f32) -> LinearRgba {
        LinearRgba::from_straight(r, g, b, a)
    }

    fn paint_pixel(document: &mut Document, x: u32, y: u32, value: LinearRgba) -> Damage {
        let mut gesture = document.active_layer_mut().scoped_gesture().unwrap();
        gesture.set_pixel(x, y, value).unwrap();
        let damage = gesture.commit().unwrap().unwrap();
        document.recompose_damage(&damage).unwrap()
    }

    fn paint_recorded(document: &mut Document, x: u32, y: u32, value: LinearRgba) -> Damage {
        let damage = paint_pixel(document, x, y, value);
        document.record_active_raster_edit().unwrap();
        damage
    }

    #[test]
    fn new_document_has_one_empty_active_layer() {
        let document = Document::new(32, 24, 8).unwrap();
        assert_eq!(document.revision(), DocumentRevision::INITIAL);
        assert_eq!(document.layers().len(), 1);
        assert_eq!(document.layers()[0].id(), document.active_layer_id());
        assert_eq!(document.layers()[0].name(), "Layer 1");
        assert_eq!(document.composite().allocated_tile_count(), 0);
    }

    #[test]
    fn revision_identifies_each_committed_state_transition() {
        let mut document = Document::new(16, 16, 8).unwrap();
        let layer = document.active_layer_id();

        document.set_active_layer(layer).unwrap();
        assert_eq!(document.revision().get(), 0);

        document.rename_layer(layer, "Ink").unwrap();
        assert_eq!(document.revision().get(), 1);
        document.rename_layer(layer, "Ink").unwrap();
        assert_eq!(document.revision().get(), 1);

        paint_recorded(&mut document, 2, 3, color(1.0, 0.0, 0.0, 1.0));
        assert_eq!(document.revision().get(), 2);

        document.undo().unwrap().unwrap();
        assert_eq!(document.revision().get(), 3);
        document.redo().unwrap().unwrap();
        assert_eq!(document.revision().get(), 4);

        assert!(document.redo().unwrap().is_none());
        assert_eq!(document.revision().get(), 4);
    }

    #[test]
    fn composite_uses_bottom_to_top_source_over() {
        let mut document = Document::new(16, 16, 8).unwrap();
        let bottom = document.active_layer_id();
        paint_pixel(&mut document, 3, 4, color(1.0, 0.0, 0.0, 1.0));
        let top = document.create_layer("Top").unwrap();
        paint_pixel(&mut document, 3, 4, color(0.0, 0.0, 1.0, 0.5));

        assert_eq!(
            document.composite().pixel(3, 4).unwrap(),
            LinearRgba::premultiplied(0.5, 0.0, 0.5, 1.0)
        );

        document.move_layer(bottom, 1).unwrap();
        assert_eq!(document.layers()[0].id(), top);
        assert_eq!(
            document.composite().pixel(3, 4).unwrap(),
            LinearRgba::premultiplied(1.0, 0.0, 0.0, 1.0)
        );
    }

    #[test]
    fn visibility_and_opacity_recompose_only_content_tiles() {
        let mut document = Document::new(64, 64, 16).unwrap();
        let layer = document.active_layer_id();
        paint_pixel(&mut document, 2, 2, color(0.0, 1.0, 0.0, 1.0));
        document.reset_composite_stats();

        let damage = document.set_layer_opacity(layer, 0.25).unwrap();
        assert_eq!(damage.tiles(), &[TileCoord::new(0, 0)]);
        assert_eq!(
            document.composite().pixel(2, 2).unwrap(),
            LinearRgba::premultiplied(0.0, 0.25, 0.0, 0.25)
        );
        assert_eq!(document.composite_stats().tile_updates, 1);
        assert_eq!(document.composite_stats().pixels_composited, 256);

        document.set_layer_visibility(layer, false).unwrap();
        assert_eq!(document.composite().allocated_tile_count(), 0);
    }

    #[test]
    fn ordinary_damage_does_not_recompose_the_canvas_or_unrelated_tiles() {
        let mut document = Document::new(1024, 1024, 128).unwrap();
        document.reset_composite_stats();
        let damage = paint_pixel(&mut document, 400, 500, color(0.2, 0.3, 0.4, 1.0));

        assert_eq!(damage.tiles(), &[TileCoord::new(3, 3)]);
        assert_eq!(document.composite_stats().tile_updates, 1);
        assert_eq!(document.composite_stats().pixels_composited, 1);
        assert_eq!(document.composite().allocated_tile_count(), 1);
    }

    #[test]
    fn duplicate_is_independent_and_delete_restores_lower_result() {
        let mut document = Document::new(16, 16, 8).unwrap();
        let original = document.active_layer_id();
        paint_pixel(&mut document, 1, 1, color(1.0, 0.0, 0.0, 0.5));
        let (duplicate, _) = document.duplicate_layer(original).unwrap();
        assert_eq!(
            document.composite().pixel(1, 1).unwrap(),
            LinearRgba::premultiplied(0.75, 0.0, 0.0, 0.75)
        );

        paint_pixel(&mut document, 2, 2, color(0.0, 0.0, 1.0, 1.0));
        assert_eq!(
            document.layer_raster(original).unwrap().pixel(2, 2),
            Some(LinearRgba::TRANSPARENT)
        );

        document.delete_layer(duplicate).unwrap();
        assert_eq!(
            document.composite().pixel(1, 1).unwrap(),
            color(1.0, 0.0, 0.0, 0.5)
        );
    }

    #[test]
    fn inserted_raster_becomes_active_without_copying_or_flattening() {
        let mut document = Document::new(32, 32, 8).unwrap();
        let mut imported = RasterLayer::new(32, 32, 8).unwrap();
        let gesture = imported.begin_gesture().unwrap();
        imported
            .set_pixel(gesture, 17, 9, color(0.2, 0.4, 0.6, 0.8))
            .unwrap();
        imported.commit_gesture(gesture).unwrap();

        let (id, damage) = document.insert_raster_layer("Imported", imported).unwrap();

        assert_eq!(document.active_layer_id(), id);
        assert_eq!(document.layers().len(), 2);
        assert_eq!(document.layer(id).unwrap().name(), "Imported");
        assert_eq!(
            document.active_layer().pixel(17, 9),
            Some(color(0.2, 0.4, 0.6, 0.8))
        );
        assert_eq!(
            document.composite().pixel(17, 9),
            Some(color(0.2, 0.4, 0.6, 0.8))
        );
        assert_eq!(damage.tiles(), &[TileCoord::new(2, 1)]);
    }

    #[test]
    fn incompatible_insert_leaves_document_unchanged() {
        let mut document = Document::new(32, 32, 8).unwrap();
        let imported = RasterLayer::new(16, 32, 8).unwrap();
        let active = document.active_layer_id();

        assert!(matches!(
            document.insert_raster_layer("Wrong size", imported),
            Err(DocumentError::ImportedLayerGeometryMismatch)
        ));
        assert_eq!(document.layers().len(), 1);
        assert_eq!(document.active_layer_id(), active);
    }

    #[test]
    fn last_layer_and_invalid_properties_are_rejected_without_mutation() {
        let mut document = Document::new(16, 16, 8).unwrap();
        let layer = document.active_layer_id();
        assert!(matches!(
            document.delete_layer(layer),
            Err(DocumentError::CannotDeleteLastLayer)
        ));
        assert!(matches!(
            document.set_layer_opacity(layer, f32::NAN),
            Err(DocumentError::InvalidOpacity(_))
        ));
        assert!(matches!(
            document.rename_layer(layer, " "),
            Err(DocumentError::EmptyLayerName)
        ));
        assert_eq!(document.layers().len(), 1);
    }

    #[test]
    fn flattened_layer_becomes_exact_initial_composite() {
        let mut raster = RasterLayer::new(32, 32, 8).unwrap();
        let mut gesture = raster.scoped_gesture().unwrap();
        gesture.set_pixel(9, 7, color(0.2, 0.4, 0.8, 0.75)).unwrap();
        gesture.commit().unwrap();

        let document = Document::from_flattened(raster, "Recovered").unwrap();
        assert_eq!(document.layers()[0].name(), "Recovered");
        assert_eq!(
            document.composite().pixel(9, 7),
            document.active_layer().pixel(9, 7)
        );
    }

    #[test]
    fn one_history_orders_raster_and_structural_edits() {
        let mut document = Document::new(16, 16, 8).unwrap();
        let bottom = document.active_layer_id();
        let red = color(1.0, 0.0, 0.0, 1.0);
        let blue = color(0.0, 0.0, 1.0, 1.0);
        paint_recorded(&mut document, 3, 4, red);
        let top = document.create_layer("Top").unwrap();
        paint_recorded(&mut document, 3, 4, blue);
        document.set_layer_visibility(bottom, false).unwrap();
        assert_eq!(document.undo_depth(), 4);
        assert_eq!(document.composite().pixel(3, 4), Some(blue));

        document.undo().unwrap().unwrap();
        assert!(document.layer(bottom).unwrap().visible());
        document.undo().unwrap().unwrap();
        assert_eq!(document.active_layer_id(), top);
        assert_eq!(
            document.active_layer().pixel(3, 4),
            Some(LinearRgba::TRANSPARENT)
        );
        assert_eq!(document.composite().pixel(3, 4), Some(red));
        document.undo().unwrap().unwrap();
        assert_eq!(document.layers().len(), 1);
        assert_eq!(document.active_layer_id(), bottom);
        document.undo().unwrap().unwrap();
        assert_eq!(
            document.composite().pixel(3, 4),
            Some(LinearRgba::TRANSPARENT)
        );
        assert!(document.undo().unwrap().is_none());

        for _ in 0..4 {
            document.redo().unwrap().unwrap();
        }
        assert_eq!(document.layers().len(), 2);
        assert!(!document.layer(bottom).unwrap().visible());
        assert_eq!(document.composite().pixel(3, 4), Some(blue));
    }

    #[test]
    fn imported_layer_is_one_undoable_command_with_stable_identity() {
        let mut document = Document::new(32, 32, 8).unwrap();
        let previous = document.active_layer_id();
        let mut imported = RasterLayer::new(32, 32, 8).unwrap();
        let gesture = imported.begin_gesture().unwrap();
        imported
            .set_pixel(gesture, 17, 9, color(0.2, 0.4, 0.6, 0.8))
            .unwrap();
        imported.commit_gesture(gesture).unwrap();

        let (imported_id, _) = document.insert_raster_layer("Imported", imported).unwrap();
        assert_eq!(document.undo_depth(), 1);
        assert_eq!(document.active_layer().undo_depth(), 0);

        document.undo().unwrap().unwrap();
        assert_eq!(document.layers().len(), 1);
        assert_eq!(document.active_layer_id(), previous);
        document.redo().unwrap().unwrap();
        assert_eq!(document.active_layer_id(), imported_id);
        assert_eq!(
            document.active_layer().pixel(17, 9),
            Some(color(0.2, 0.4, 0.6, 0.8))
        );
    }

    #[test]
    fn new_raster_edit_clears_document_wide_redo() {
        let mut document = Document::new(16, 16, 8).unwrap();
        let bottom = document.active_layer_id();
        let temporary = document.create_layer("Temporary").unwrap();
        assert_eq!(document.rasters.len(), 2);
        document.undo().unwrap().unwrap();
        assert_eq!(document.redo_depth(), 1);
        assert_eq!(document.active_layer_id(), bottom);
        assert!(document.rasters.contains_key(&temporary));

        paint_recorded(&mut document, 1, 1, color(1.0, 0.0, 0.0, 1.0));

        assert_eq!(document.redo_depth(), 0);
        assert!(document.redo().unwrap().is_none());
        assert_eq!(document.layers().len(), 1);
        assert_eq!(document.rasters.len(), 1);
        assert!(!document.rasters.contains_key(&temporary));
    }

    #[test]
    fn deleting_a_layer_also_clears_its_local_redo() {
        let mut document = Document::new(16, 16, 8).unwrap();
        let bottom = document.active_layer_id();
        document.create_layer("Top").unwrap();
        document.set_active_layer(bottom).unwrap();
        paint_recorded(&mut document, 1, 1, color(1.0, 0.0, 0.0, 1.0));
        document.undo().unwrap().unwrap();
        assert_eq!(document.layer_raster(bottom).unwrap().redo_depth(), 1);

        document.delete_layer(bottom).unwrap();
        document.undo().unwrap().unwrap();

        assert_eq!(document.layer_raster(bottom).unwrap().redo_depth(), 0);
    }

    #[test]
    fn history_bound_evicts_the_matching_oldest_raster_mementos() {
        let mut document = Document::new(8, 8, 8).unwrap();
        let red = color(1.0, 0.0, 0.0, 1.0);
        let blue = color(0.0, 0.0, 1.0, 1.0);
        for index in 0..(MAX_DOCUMENT_HISTORY_ENTRIES + 4) {
            let value = if index % 2 == 0 { red } else { blue };
            paint_recorded(&mut document, 2, 2, value);
        }

        assert_eq!(document.undo_depth(), MAX_DOCUMENT_HISTORY_ENTRIES);
        assert_eq!(
            document.active_layer().undo_depth(),
            MAX_DOCUMENT_HISTORY_ENTRIES
        );
        for _ in 0..MAX_DOCUMENT_HISTORY_ENTRIES {
            document.undo().unwrap().unwrap();
        }
        assert_eq!(document.composite().pixel(2, 2), Some(blue));
        assert!(document.undo().unwrap().is_none());
    }
}
