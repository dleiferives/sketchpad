use crate::document::{Document, DocumentRevision, LayerId};
use std::{error::Error, fmt, mem::size_of, sync::Arc};

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentLayerMetadata {
    id: LayerId,
    name: String,
    visible: bool,
    opacity: f32,
}

impl DocumentLayerMetadata {
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

/// Immutable, raster-free identity and layer state for one document revision.
///
/// This is the metadata half of a revisioned document snapshot. Raster backends
/// may own pixels in CPU tiles, a GPU atlas, or both, but they share this exact
/// description of geometry, layer order, properties, and active selection.
#[derive(Clone)]
pub struct DocumentMetadata {
    revision: DocumentRevision,
    width: u32,
    height: u32,
    tile_size: u32,
    active_layer: LayerId,
    next_layer_id: u64,
    layers: Arc<[DocumentLayerMetadata]>,
    retained_byte_len: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DocumentMetadataEdit {
    Presence {
        layer: DocumentLayerMetadata,
        index: usize,
        before_active: LayerId,
        after_active: LayerId,
        present_after: bool,
    },
    Visibility {
        layer: LayerId,
        before: bool,
        after: bool,
    },
    Opacity {
        layer: LayerId,
        before: f32,
        after: f32,
    },
    Move {
        layer: LayerId,
        before: usize,
        after: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentMetadataEditDirection {
    Forward,
    Reverse,
}

impl DocumentMetadataEdit {
    pub const fn layer(&self) -> LayerId {
        match self {
            Self::Presence { layer, .. } => layer.id,
            Self::Visibility { layer, .. }
            | Self::Opacity { layer, .. }
            | Self::Move { layer, .. } => *layer,
        }
    }

    pub fn retained_byte_len(&self) -> u64 {
        let name_bytes = match self {
            Self::Presence { layer, .. } => layer.name.len() as u64,
            _ => 0,
        };
        (size_of::<Self>() as u64)
            .checked_add(name_bytes)
            .expect("one metadata edit fits in addressable memory")
    }
}

impl DocumentMetadata {
    pub fn from_document(document: &Document) -> Self {
        let layers: Vec<_> = document
            .layers()
            .iter()
            .map(|layer| DocumentLayerMetadata {
                id: layer.id(),
                name: layer.name().to_owned(),
                visible: layer.visible(),
                opacity: layer.opacity(),
            })
            .collect();
        Self::from_validated_parts(
            document.revision(),
            document.width(),
            document.height(),
            document.tile_size(),
            document.active_layer_id(),
            document.next_layer_id_raw(),
            layers,
        )
    }

    pub fn new_blank(
        width: u32,
        height: u32,
        tile_size: u32,
        revision: DocumentRevision,
    ) -> Result<Self, DocumentMetadataError> {
        if width == 0 || height == 0 {
            return Err(DocumentMetadataError::EmptyCanvas);
        }
        if tile_size == 0 {
            return Err(DocumentMetadataError::InvalidTileSize);
        }
        let active_layer = LayerId::from_raw(1);
        Ok(Self::from_validated_parts(
            revision,
            width,
            height,
            tile_size,
            active_layer,
            2,
            vec![DocumentLayerMetadata {
                id: active_layer,
                name: "Layer 1".to_owned(),
                visible: true,
                opacity: 1.0,
            }],
        ))
    }

    fn from_validated_parts(
        revision: DocumentRevision,
        width: u32,
        height: u32,
        tile_size: u32,
        active_layer: LayerId,
        next_layer_id: u64,
        layers: Vec<DocumentLayerMetadata>,
    ) -> Self {
        let name_bytes = layers
            .iter()
            .try_fold(0_u64, |bytes, layer| {
                bytes.checked_add(
                    u64::try_from(layer.name.len())
                        .expect("a live metadata name length fits in u64"),
                )
            })
            .expect("live metadata name bytes fit in addressable memory");
        let layer_bytes = u64::try_from(layers.len())
            .expect("a live metadata layer count fits in u64")
            .checked_mul(size_of::<DocumentLayerMetadata>() as u64)
            .expect("live metadata layer bytes fit in addressable memory");
        let retained_byte_len = (size_of::<Self>() as u64)
            .checked_add(layer_bytes)
            .and_then(|bytes| bytes.checked_add(name_bytes))
            .expect("a live document's validated metadata fits in addressable memory");
        Self {
            revision,
            width,
            height,
            tile_size,
            active_layer,
            next_layer_id,
            layers: layers.into(),
            retained_byte_len,
        }
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
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

    pub const fn active_layer(&self) -> LayerId {
        self.active_layer
    }

    pub fn layers(&self) -> &[DocumentLayerMetadata] {
        &self.layers
    }

    pub const fn retained_byte_len(&self) -> u64 {
        self.retained_byte_len
    }

    pub(crate) fn set_revision(&mut self, revision: DocumentRevision) {
        self.revision = revision;
    }

    pub(crate) fn set_active_layer(
        &mut self,
        layer: LayerId,
    ) -> Result<(), DocumentMetadataError> {
        if !self.layers.iter().any(|candidate| candidate.id == layer) {
            return Err(DocumentMetadataError::MissingLayer(layer));
        }
        self.active_layer = layer;
        Ok(())
    }

    pub fn prepare_layer_visibility(
        &self,
        layer: LayerId,
        visible: bool,
    ) -> Result<Option<DocumentMetadataEdit>, DocumentMetadataError> {
        let current = self.require_layer(layer)?.visible;
        Ok((current != visible).then_some(DocumentMetadataEdit::Visibility {
            layer,
            before: current,
            after: visible,
        }))
    }

    pub fn prepare_create_layer(
        &self,
        name: impl Into<String>,
    ) -> Result<DocumentMetadataEdit, DocumentMetadataError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DocumentMetadataError::EmptyLayerName);
        }
        let next_layer_id = self
            .next_layer_id
            .checked_add(1)
            .ok_or(DocumentMetadataError::LayerIdExhausted)?;
        let id = LayerId::from_raw(self.next_layer_id);
        if id.get() == 0 || self.layers.iter().any(|layer| layer.id == id) {
            return Err(DocumentMetadataError::LayerIdExhausted);
        }
        debug_assert_ne!(next_layer_id, 0);
        Ok(DocumentMetadataEdit::Presence {
            layer: DocumentLayerMetadata {
                id,
                name,
                visible: true,
                opacity: 1.0,
            },
            index: self.require_layer_index(self.active_layer)? + 1,
            before_active: self.active_layer,
            after_active: id,
            present_after: true,
        })
    }

    pub fn prepare_duplicate_layer(
        &self,
        source: LayerId,
    ) -> Result<DocumentMetadataEdit, DocumentMetadataError> {
        let source_index = self.require_layer_index(source)?;
        let source = &self.layers[source_index];
        let next_layer_id = self
            .next_layer_id
            .checked_add(1)
            .ok_or(DocumentMetadataError::LayerIdExhausted)?;
        let id = LayerId::from_raw(self.next_layer_id);
        if id.get() == 0 || self.layers.iter().any(|layer| layer.id == id) {
            return Err(DocumentMetadataError::LayerIdExhausted);
        }
        debug_assert_ne!(next_layer_id, 0);
        Ok(DocumentMetadataEdit::Presence {
            layer: DocumentLayerMetadata {
                id,
                name: format!("{} copy", source.name),
                visible: source.visible,
                opacity: source.opacity,
            },
            index: source_index + 1,
            before_active: self.active_layer,
            after_active: id,
            present_after: true,
        })
    }

    pub fn prepare_delete_layer(
        &self,
        layer: LayerId,
    ) -> Result<DocumentMetadataEdit, DocumentMetadataError> {
        if self.layers.len() == 1 {
            return Err(DocumentMetadataError::CannotDeleteLastLayer);
        }
        let index = self.require_layer_index(layer)?;
        let after_active = if self.active_layer == layer {
            if index + 1 < self.layers.len() {
                self.layers[index + 1].id
            } else {
                self.layers[index - 1].id
            }
        } else {
            self.active_layer
        };
        Ok(DocumentMetadataEdit::Presence {
            layer: self.layers[index].clone(),
            index,
            before_active: self.active_layer,
            after_active,
            present_after: false,
        })
    }

    pub fn prepare_layer_opacity(
        &self,
        layer: LayerId,
        opacity: f32,
    ) -> Result<Option<DocumentMetadataEdit>, DocumentMetadataError> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(DocumentMetadataError::InvalidOpacity);
        }
        let current = self.require_layer(layer)?.opacity;
        Ok((current != opacity).then_some(DocumentMetadataEdit::Opacity {
            layer,
            before: current,
            after: opacity,
        }))
    }

    pub fn prepare_layer_move(
        &self,
        layer: LayerId,
        destination: usize,
    ) -> Result<Option<DocumentMetadataEdit>, DocumentMetadataError> {
        if destination >= self.layers.len() {
            return Err(DocumentMetadataError::LayerIndexOutOfBounds(destination));
        }
        let current = self.require_layer_index(layer)?;
        Ok((current != destination).then_some(DocumentMetadataEdit::Move {
            layer,
            before: current,
            after: destination,
        }))
    }

    pub fn apply_edit(
        &mut self,
        edit: &DocumentMetadataEdit,
        direction: DocumentMetadataEditDirection,
        revision: DocumentRevision,
    ) -> Result<(), DocumentMetadataError> {
        if self.revision.checked_next() != Some(revision) {
            return Err(DocumentMetadataError::RevisionNotNext {
                current: self.revision,
                requested: revision,
            });
        }
        let mut layers = self.layers.to_vec();
        match edit {
            DocumentMetadataEdit::Presence {
                layer,
                index,
                before_active,
                after_active,
                present_after,
            } => {
                let (replacement_active, should_be_present) = match direction {
                    DocumentMetadataEditDirection::Forward => (*after_active, *present_after),
                    DocumentMetadataEditDirection::Reverse => (*before_active, !*present_after),
                };
                if should_be_present {
                    if *index > layers.len() {
                        return Err(DocumentMetadataError::LayerIndexOutOfBounds(*index));
                    }
                    if layers.iter().any(|candidate| candidate.id == layer.id) {
                        return Err(DocumentMetadataError::DuplicateLayer(layer.id));
                    }
                    layers.insert(*index, layer.clone());
                    if self.next_layer_id <= layer.id.get() {
                        self.next_layer_id = layer
                            .id
                            .get()
                            .checked_add(1)
                            .ok_or(DocumentMetadataError::LayerIdExhausted)?;
                    }
                } else {
                    if layers.len() == 1 {
                        return Err(DocumentMetadataError::CannotDeleteLastLayer);
                    }
                    let actual = self.require_layer_index(layer.id)?;
                    if actual != *index || layers[actual] != *layer {
                        return Err(DocumentMetadataError::EditStateChanged(layer.id));
                    }
                    layers.remove(actual);
                }
                if !layers
                    .iter()
                    .any(|candidate| candidate.id == replacement_active)
                {
                    return Err(DocumentMetadataError::MissingLayer(replacement_active));
                }
                self.active_layer = replacement_active;
            }
            DocumentMetadataEdit::Visibility {
                layer,
                before,
                after,
            } => {
                let index = self.require_layer_index(*layer)?;
                let (expected, replacement) = edit_sides(*before, *after, direction);
                if layers[index].visible != expected {
                    return Err(DocumentMetadataError::EditStateChanged(*layer));
                }
                layers[index].visible = replacement;
            }
            DocumentMetadataEdit::Opacity {
                layer,
                before,
                after,
            } => {
                let index = self.require_layer_index(*layer)?;
                let (expected, replacement) = edit_sides(*before, *after, direction);
                if !expected.is_finite()
                    || !(0.0..=1.0).contains(&expected)
                    || !replacement.is_finite()
                    || !(0.0..=1.0).contains(&replacement)
                {
                    return Err(DocumentMetadataError::InvalidOpacity);
                }
                if layers[index].opacity != expected {
                    return Err(DocumentMetadataError::EditStateChanged(*layer));
                }
                layers[index].opacity = replacement;
            }
            DocumentMetadataEdit::Move {
                layer,
                before,
                after,
            } => {
                let (expected, destination) = edit_sides(*before, *after, direction);
                if expected >= layers.len() || destination >= layers.len() {
                    return Err(DocumentMetadataError::LayerIndexOutOfBounds(
                        expected.max(destination),
                    ));
                }
                let current = self.require_layer_index(*layer)?;
                if current != expected {
                    return Err(DocumentMetadataError::EditStateChanged(*layer));
                }
                let moved = layers.remove(current);
                layers.insert(destination, moved);
            }
        }
        self.layers = layers.into();
        self.revision = revision;
        Ok(())
    }

    fn require_layer(
        &self,
        layer: LayerId,
    ) -> Result<&DocumentLayerMetadata, DocumentMetadataError> {
        self.layers
            .iter()
            .find(|candidate| candidate.id == layer)
            .ok_or(DocumentMetadataError::MissingLayer(layer))
    }

    fn require_layer_index(&self, layer: LayerId) -> Result<usize, DocumentMetadataError> {
        self.layers
            .iter()
            .position(|candidate| candidate.id == layer)
            .ok_or(DocumentMetadataError::MissingLayer(layer))
    }
}

fn edit_sides<T: Copy>(
    before: T,
    after: T,
    direction: DocumentMetadataEditDirection,
) -> (T, T) {
    match direction {
        DocumentMetadataEditDirection::Forward => (before, after),
        DocumentMetadataEditDirection::Reverse => (after, before),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentMetadataError {
    EmptyCanvas,
    InvalidTileSize,
    MissingLayer(LayerId),
    DuplicateLayer(LayerId),
    EmptyLayerName,
    LayerIdExhausted,
    CannotDeleteLastLayer,
    InvalidOpacity,
    LayerIndexOutOfBounds(usize),
    RevisionNotNext {
        current: DocumentRevision,
        requested: DocumentRevision,
    },
    EditStateChanged(LayerId),
}

impl fmt::Display for DocumentMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCanvas => write!(formatter, "document metadata requires a nonempty canvas"),
            Self::InvalidTileSize => write!(formatter, "document metadata requires a tile size"),
            Self::MissingLayer(layer) => {
                write!(formatter, "document metadata has no layer {}", layer.get())
            }
            Self::DuplicateLayer(layer) => {
                write!(formatter, "document metadata repeats layer {}", layer.get())
            }
            Self::EmptyLayerName => write!(formatter, "document metadata layer name is empty"),
            Self::LayerIdExhausted => {
                write!(formatter, "document metadata layer IDs are exhausted")
            }
            Self::CannotDeleteLastLayer => {
                write!(formatter, "document metadata cannot delete its last layer")
            }
            Self::InvalidOpacity => write!(formatter, "document metadata opacity is invalid"),
            Self::LayerIndexOutOfBounds(index) => {
                write!(formatter, "document metadata layer index {index} is out of bounds")
            }
            Self::RevisionNotNext { current, requested } => write!(
                formatter,
                "document metadata revision {} cannot advance directly to {}",
                current.get(),
                requested.get()
            ),
            Self::EditStateChanged(layer) => write!(
                formatter,
                "document metadata layer {} changed after edit preparation",
                layer.get()
            ),
        }
    }
}

impl Error for DocumentMetadataError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn snapshot_preserves_raster_free_document_identity() {
        let mut document = Document::new(48, 40, 8).unwrap();
        let bottom = document.active_layer_id();
        let top = document.create_layer("Highlights").unwrap();
        document.set_layer_opacity(top, 0.625).unwrap();
        document.set_layer_visibility(bottom, false).unwrap();

        let metadata = DocumentMetadata::from_document(&document);
        assert_eq!(metadata.revision(), document.revision());
        assert_eq!(metadata.dimensions(), [48, 40]);
        assert_eq!(metadata.tile_size(), 8);
        assert_eq!(metadata.active_layer(), top);
        assert_eq!(metadata.layers().len(), 2);
        assert_eq!(metadata.layers()[0].id(), bottom);
        assert!(!metadata.layers()[0].visible());
        assert_eq!(metadata.layers()[1].name(), "Highlights");
        assert_eq!(metadata.layers()[1].opacity(), 0.625);
        assert!(metadata.retained_byte_len() >= size_of::<DocumentMetadata>() as u64);
    }

    #[test]
    fn snapshots_do_not_alias_later_metadata_edits() {
        let mut document = Document::new(32, 32, 8).unwrap();
        let layer = document.active_layer_id();
        let snapshot = DocumentMetadata::from_document(&document);

        document.rename_layer(layer, "Ink").unwrap();
        assert_eq!(snapshot.layers()[0].name(), "Layer 1");
        assert_eq!(snapshot.revision(), DocumentRevision::INITIAL);
    }

    #[test]
    fn blank_metadata_rejects_invalid_geometry() {
        assert!(matches!(
            DocumentMetadata::new_blank(0, 32, 8, DocumentRevision::INITIAL),
            Err(DocumentMetadataError::EmptyCanvas)
        ));
        assert!(matches!(
            DocumentMetadata::new_blank(32, 32, 0, DocumentRevision::INITIAL),
            Err(DocumentMetadataError::InvalidTileSize)
        ));
    }

    #[test]
    fn active_selection_changes_no_revision_and_rejects_missing_layers() {
        let mut document = Document::new(32, 32, 8).unwrap();
        let first = document.active_layer_id();
        let second = document.create_layer("Second").unwrap();
        let mut metadata = DocumentMetadata::from_document(&document);
        let revision = metadata.revision();

        metadata.set_active_layer(first).unwrap();
        assert_eq!(metadata.active_layer(), first);
        assert_eq!(metadata.revision(), revision);
        assert!(matches!(
            metadata.set_active_layer(LayerId::from_raw(second.get() + 1)),
            Err(DocumentMetadataError::MissingLayer(_))
        ));
        assert_eq!(metadata.active_layer(), first);
        assert_eq!(metadata.revision(), revision);
    }

    #[test]
    fn reversible_property_edits_require_exact_state_and_next_revision() {
        let mut document = Document::new(32, 32, 8).unwrap();
        let first = document.active_layer_id();
        let second = document.create_layer("Second").unwrap();
        let mut metadata = DocumentMetadata::from_document(&document);

        let visibility = metadata
            .prepare_layer_visibility(first, false)
            .unwrap()
            .unwrap();
        let revision_1 = metadata.revision().checked_next().unwrap();
        metadata
            .apply_edit(
                &visibility,
                DocumentMetadataEditDirection::Forward,
                revision_1,
            )
            .unwrap();
        assert!(!metadata.require_layer(first).unwrap().visible());
        assert!(matches!(
            metadata.apply_edit(
                &visibility,
                DocumentMetadataEditDirection::Forward,
                revision_1.checked_next().unwrap()
            ),
            Err(DocumentMetadataError::EditStateChanged(layer)) if layer == first
        ));

        let revision_2 = revision_1.checked_next().unwrap();
        metadata
            .apply_edit(
                &visibility,
                DocumentMetadataEditDirection::Reverse,
                revision_2,
            )
            .unwrap();
        assert!(metadata.require_layer(first).unwrap().visible());

        let opacity = metadata
            .prepare_layer_opacity(second, 0.25)
            .unwrap()
            .unwrap();
        let revision_3 = revision_2.checked_next().unwrap();
        metadata
            .apply_edit(
                &opacity,
                DocumentMetadataEditDirection::Forward,
                revision_3,
            )
            .unwrap();
        assert_eq!(metadata.require_layer(second).unwrap().opacity(), 0.25);
        assert_eq!(metadata.active_layer(), second);
    }

    #[test]
    fn reversible_move_preserves_active_identity_and_layer_order() {
        let mut document = Document::new(32, 32, 8).unwrap();
        let first = document.active_layer_id();
        let second = document.create_layer("Second").unwrap();
        let mut metadata = DocumentMetadata::from_document(&document);
        let edit = metadata.prepare_layer_move(second, 0).unwrap().unwrap();
        let revision_1 = metadata.revision().checked_next().unwrap();

        metadata
            .apply_edit(
                &edit,
                DocumentMetadataEditDirection::Forward,
                revision_1,
            )
            .unwrap();
        assert_eq!(metadata.layers()[0].id(), second);
        assert_eq!(metadata.layers()[1].id(), first);
        assert_eq!(metadata.active_layer(), second);

        metadata
            .apply_edit(
                &edit,
                DocumentMetadataEditDirection::Reverse,
                revision_1.checked_next().unwrap(),
            )
            .unwrap();
        assert_eq!(metadata.layers()[0].id(), first);
        assert_eq!(metadata.layers()[1].id(), second);
        assert_eq!(metadata.active_layer(), second);
    }

    #[test]
    fn reversible_presence_keeps_stable_identity_and_monotonic_allocation() {
        let mut metadata =
            DocumentMetadata::new_blank(64, 64, 16, DocumentRevision::INITIAL).unwrap();
        let original = metadata.active_layer();
        let edit = metadata.prepare_create_layer("Ink").unwrap();
        let created = edit.layer();
        let revision_1 = metadata.revision().checked_next().unwrap();
        metadata
            .apply_edit(&edit, DocumentMetadataEditDirection::Forward, revision_1)
            .unwrap();
        assert_eq!(metadata.layers().len(), 2);
        assert_eq!(metadata.active_layer(), created);

        metadata.set_active_layer(original).unwrap();
        let revision_2 = revision_1.checked_next().unwrap();
        metadata
            .apply_edit(&edit, DocumentMetadataEditDirection::Reverse, revision_2)
            .unwrap();
        assert_eq!(metadata.layers().len(), 1);
        assert_eq!(metadata.active_layer(), original);

        let next = metadata.prepare_create_layer("Paint").unwrap();
        assert!(next.layer().get() > created.get());
        assert!(matches!(
            metadata.prepare_create_layer("   "),
            Err(DocumentMetadataError::EmptyLayerName)
        ));
    }

    #[test]
    fn duplicate_presence_inherits_properties_and_follows_its_source() {
        let mut document = Document::new(32, 32, 8).unwrap();
        let source = document.active_layer_id();
        document.rename_layer(source, "Ink").unwrap();
        document.set_layer_visibility(source, false).unwrap();
        document.set_layer_opacity(source, 0.375).unwrap();
        let upper = document.create_layer("Upper").unwrap();
        let mut metadata = DocumentMetadata::from_document(&document);

        let edit = metadata.prepare_duplicate_layer(source).unwrap();
        let duplicate = edit.layer();
        let revision = metadata.revision().checked_next().unwrap();
        metadata
            .apply_edit(&edit, DocumentMetadataEditDirection::Forward, revision)
            .unwrap();

        assert_eq!(metadata.layers()[0].id(), source);
        assert_eq!(metadata.layers()[1].id(), duplicate);
        assert_eq!(metadata.layers()[2].id(), upper);
        assert_eq!(metadata.layers()[1].name(), "Ink copy");
        assert!(!metadata.layers()[1].visible());
        assert_eq!(metadata.layers()[1].opacity(), 0.375);
        assert_eq!(metadata.active_layer(), duplicate);
    }
}
