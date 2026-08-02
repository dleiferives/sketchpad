use crate::document::{DocumentLayer, LayerId};
use std::collections::VecDeque;

pub const MAX_DOCUMENT_HISTORY_ENTRIES: usize = 256;

#[derive(Debug, PartialEq)]
pub(crate) enum DocumentEdit {
    Raster {
        layer: LayerId,
    },
    LayerPresence {
        layer: DocumentLayer,
        index: usize,
        before_active: LayerId,
        after_active: LayerId,
        present_after: bool,
    },
    Rename {
        layer: LayerId,
        before: String,
        after: String,
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

impl DocumentEdit {
    pub(crate) const fn layer_id(&self) -> LayerId {
        match self {
            Self::Raster { layer }
            | Self::Rename { layer, .. }
            | Self::Visibility { layer, .. }
            | Self::Opacity { layer, .. }
            | Self::Move { layer, .. } => *layer,
            Self::LayerPresence { layer, .. } => layer.id(),
        }
    }
}

#[derive(Default)]
pub(crate) struct DocumentHistory {
    undo: VecDeque<DocumentEdit>,
    redo: VecDeque<DocumentEdit>,
}

impl DocumentHistory {
    pub(crate) fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub(crate) fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    pub(crate) fn undo_edits(&self) -> impl Iterator<Item = &DocumentEdit> {
        self.undo.iter()
    }

    pub(crate) fn referenced_layer_ids(&self) -> impl Iterator<Item = LayerId> + '_ {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(DocumentEdit::layer_id)
    }

    pub(crate) fn record(&mut self, edit: DocumentEdit) -> Option<DocumentEdit> {
        self.redo.clear();
        self.undo.push_back(edit);
        if self.undo.len() > MAX_DOCUMENT_HISTORY_ENTRIES {
            self.undo.pop_front()
        } else {
            None
        }
    }

    pub(crate) fn pop_undo(&mut self) -> Option<DocumentEdit> {
        self.undo.pop_back()
    }

    pub(crate) fn finish_undo(&mut self, edit: DocumentEdit) {
        self.redo.push_back(edit);
    }

    pub(crate) fn restore_undo(&mut self, edit: DocumentEdit) {
        self.undo.push_back(edit);
    }

    pub(crate) fn pop_redo(&mut self) -> Option<DocumentEdit> {
        self.redo.pop_back()
    }

    pub(crate) fn finish_redo(&mut self, edit: DocumentEdit) {
        self.undo.push_back(edit);
    }

    pub(crate) fn restore_redo(&mut self, edit: DocumentEdit) {
        self.redo.push_back(edit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster_edit(raw_layer: u64) -> DocumentEdit {
        DocumentEdit::Raster {
            layer: LayerId::from_raw(raw_layer),
        }
    }

    #[test]
    fn a_new_branch_discards_redo_without_reordering_undo() {
        let mut history = DocumentHistory::default();
        history.record(raster_edit(1));
        history.record(raster_edit(2));
        let undone = history.pop_undo().unwrap();
        history.finish_undo(undone);
        assert_eq!(history.undo_depth(), 1);
        assert_eq!(history.redo_depth(), 1);

        history.record(raster_edit(3));
        assert_eq!(history.undo_depth(), 2);
        assert_eq!(history.redo_depth(), 0);
        assert_eq!(history.pop_undo().unwrap().layer_id().get(), 3);
        assert_eq!(history.pop_undo().unwrap().layer_id().get(), 1);
    }

    #[test]
    fn capacity_returns_the_exact_evicted_command() {
        let mut history = DocumentHistory::default();
        for layer in 1..=MAX_DOCUMENT_HISTORY_ENTRIES as u64 {
            assert!(history.record(raster_edit(layer)).is_none());
        }
        let evicted = history
            .record(raster_edit(MAX_DOCUMENT_HISTORY_ENTRIES as u64 + 1))
            .unwrap();
        assert_eq!(evicted.layer_id().get(), 1);
        assert_eq!(history.undo_depth(), MAX_DOCUMENT_HISTORY_ENTRIES);
    }
}
