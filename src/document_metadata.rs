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
    layers: Arc<[DocumentLayerMetadata]>,
    retained_byte_len: u64,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentMetadataError {
    EmptyCanvas,
    InvalidTileSize,
    MissingLayer(LayerId),
}

impl fmt::Display for DocumentMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCanvas => write!(formatter, "document metadata requires a nonempty canvas"),
            Self::InvalidTileSize => write!(formatter, "document metadata requires a tile size"),
            Self::MissingLayer(layer) => {
                write!(formatter, "document metadata has no layer {}", layer.get())
            }
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
}
