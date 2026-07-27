use crate::raster::{Damage, LinearRgba, RasterError, RasterLayer, TileCoord};
use std::{collections::HashSet, error::Error, fmt};

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

pub struct RasterDocumentLayer {
    id: LayerId,
    name: String,
    visible: bool,
    opacity: f32,
    raster: RasterLayer,
}

impl RasterDocumentLayer {
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

    pub const fn raster(&self) -> &RasterLayer {
        &self.raster
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
    layers: Vec<RasterDocumentLayer>,
    active_layer: LayerId,
    next_layer_id: u64,
    composite: RasterLayer,
    composite_stats: CompositeStats,
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
        Ok(Self {
            width,
            height,
            tile_size,
            layers: vec![RasterDocumentLayer {
                id: first_id,
                name: "Layer 1".to_owned(),
                visible: true,
                opacity: 1.0,
                raster: RasterLayer::new(width, height, tile_size)?,
            }],
            active_layer: first_id,
            next_layer_id: 2,
            composite: RasterLayer::new(width, height, tile_size)?,
            composite_stats: CompositeStats::default(),
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
            layers: vec![RasterDocumentLayer {
                id: first_id,
                name,
                visible: true,
                opacity: 1.0,
                raster,
            }],
            active_layer: first_id,
            next_layer_id: 2,
            composite,
            composite_stats: CompositeStats::default(),
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
            layers.push(RasterDocumentLayer {
                id: part.id,
                name,
                visible: part.visible,
                opacity: part.opacity,
                raster: part.raster,
            });
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
            active_layer,
            next_layer_id,
            composite: RasterLayer::new(width, height, tile_size)?,
            composite_stats: CompositeStats::default(),
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

    pub fn layers(&self) -> &[RasterDocumentLayer] {
        &self.layers
    }

    pub fn layer(&self, id: LayerId) -> Option<&RasterDocumentLayer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    pub const fn active_layer_id(&self) -> LayerId {
        self.active_layer
    }

    pub fn active_layer_index(&self) -> usize {
        self.layer_index(self.active_layer)
            .expect("the active layer must belong to the document")
    }

    pub fn active_layer(&self) -> &RasterLayer {
        &self.layers[self.active_layer_index()].raster
    }

    pub fn active_layer_mut(&mut self) -> &mut RasterLayer {
        let index = self.active_layer_index();
        &mut self.layers[index].raster
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

    pub fn set_active_layer(&mut self, id: LayerId) -> Result<(), DocumentError> {
        self.require_layer(id)?;
        self.active_layer = id;
        Ok(())
    }

    pub fn create_layer(&mut self, name: impl Into<String>) -> Result<LayerId, DocumentError> {
        let id = self.allocate_layer_id();
        let layer = RasterDocumentLayer {
            id,
            name: validated_name(name.into())?,
            visible: true,
            opacity: 1.0,
            raster: RasterLayer::new(self.width, self.height, self.tile_size)?,
        };
        let insertion = self.active_layer_index() + 1;
        self.layers.insert(insertion, layer);
        self.active_layer = id;
        Ok(id)
    }

    pub fn duplicate_layer(&mut self, id: LayerId) -> Result<(LayerId, Damage), DocumentError> {
        let source_index = self.require_layer(id)?;
        let duplicate_id = self.allocate_layer_id();
        let source = &self.layers[source_index];
        let duplicate = RasterDocumentLayer {
            id: duplicate_id,
            name: format!("{} copy", source.name),
            visible: source.visible,
            opacity: source.opacity,
            raster: clone_raster(&source.raster)?,
        };
        let affected = coords_for_layer(source);
        self.layers.insert(source_index + 1, duplicate);
        self.active_layer = duplicate_id;
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        Ok((duplicate_id, damage))
    }

    pub fn delete_layer(&mut self, id: LayerId) -> Result<Damage, DocumentError> {
        if self.layers.len() == 1 {
            return Err(DocumentError::CannotDeleteLastLayer);
        }
        let index = self.require_layer(id)?;
        let affected = coords_for_layer(&self.layers[index]);
        self.layers.remove(index);
        if self.active_layer == id {
            self.active_layer = self.layers[index.min(self.layers.len() - 1)].id;
        }
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
        Ok(damage)
    }

    pub fn rename_layer(
        &mut self,
        id: LayerId,
        name: impl Into<String>,
    ) -> Result<(), DocumentError> {
        let index = self.require_layer(id)?;
        self.layers[index].name = validated_name(name.into())?;
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
        let affected = coords_for_layer(&self.layers[index]);
        self.layers[index].visible = visible;
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
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
        let affected = coords_for_layer(&self.layers[index]);
        self.layers[index].opacity = opacity;
        let damage = self.damage_for_coords(affected);
        self.recompose_damage(&damage)?;
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
        Ok(damage)
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
            let stride = self.tile_size as usize;
            let mut pixels = self.composite.tile(coord).map_or_else(
                || vec![LinearRgba::TRANSPARENT; stride * stride],
                |tile| tile.pixels().to_vec(),
            );

            {
                let sources: Vec<_> = self
                    .layers
                    .iter()
                    .filter(|layer| layer.visible && layer.opacity > 0.0)
                    .filter_map(|layer| layer.raster.tile(coord).map(|tile| (layer.opacity, tile)))
                    .collect();
                self.composite_stats.source_tile_reads = self
                    .composite_stats
                    .source_tile_reads
                    .saturating_add(sources.len() as u64);

                for y in local_min_y..local_max_y {
                    let row = y as usize * stride;
                    for x in local_min_x..local_max_x {
                        let index = row + x as usize;
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
                        pixels[index] = destination;
                    }
                }

                let area = global_region.area();
                self.composite_stats.pixels_composited =
                    self.composite_stats.pixels_composited.saturating_add(area);
                self.composite_stats.source_pixel_reads = self
                    .composite_stats
                    .source_pixel_reads
                    .saturating_add(area.saturating_mul(sources.len() as u64));
            }

            self.composite
                .restore_tile(coord, pixels.into_boxed_slice())?;
            self.composite_stats.tile_updates = self.composite_stats.tile_updates.saturating_add(1);
            recomposed.add(coord, global_region);
        }
        Ok(recomposed)
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
            coords.extend(layer.raster.allocated_tile_coords());
        }
        coords.into_iter().collect()
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

fn coords_for_layer(layer: &RasterDocumentLayer) -> Vec<TileCoord> {
    layer.raster.allocated_tile_coords().collect()
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
    LayerIndexOutOfBounds(usize),
    CannotDeleteLastLayer,
    InvalidOpacity(f32),
    EmptyLayerName,
    NoLayers,
    InvalidLayerId(LayerId),
    LayerIdExhausted,
    LayerGeometryMismatch(LayerId),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raster(error) => error.fmt(formatter),
            Self::LayerNotFound(id) => write!(formatter, "layer {} does not exist", id.get()),
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

    #[test]
    fn new_document_has_one_empty_active_layer() {
        let document = Document::new(32, 24, 8).unwrap();
        assert_eq!(document.layers().len(), 1);
        assert_eq!(document.layers()[0].id(), document.active_layer_id());
        assert_eq!(document.layers()[0].name(), "Layer 1");
        assert_eq!(document.composite().allocated_tile_count(), 0);
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
            document.layer(original).unwrap().raster().pixel(2, 2),
            Some(LinearRgba::TRANSPARENT)
        );

        document.delete_layer(duplicate).unwrap();
        assert_eq!(
            document.composite().pixel(1, 1).unwrap(),
            color(1.0, 0.0, 0.0, 0.5)
        );
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
}
