use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistenceState {
    recovery_path: PathBuf,
    document_path: Option<PathBuf>,
    document_modified: bool,
    recovery_dirty: bool,
}

impl PersistenceState {
    pub fn fresh(recovery_path: PathBuf) -> Self {
        Self {
            recovery_path,
            document_path: None,
            document_modified: false,
            recovery_dirty: false,
        }
    }

    pub fn recovered(recovery_path: PathBuf) -> Self {
        Self {
            recovery_path,
            document_path: None,
            document_modified: true,
            recovery_dirty: false,
        }
    }

    pub fn recovery_path(&self) -> &Path {
        &self.recovery_path
    }

    pub fn document_path(&self) -> Option<&Path> {
        self.document_path.as_deref()
    }

    pub fn document_modified(&self) -> bool {
        self.document_modified
    }

    pub fn recovery_dirty(&self) -> bool {
        self.recovery_dirty
    }

    pub fn document_changed(&mut self) {
        self.document_modified = true;
        self.recovery_dirty = true;
    }

    pub fn recovery_saved(&mut self) {
        self.recovery_dirty = false;
    }

    pub fn document_saved(&mut self, path: PathBuf) {
        self.document_path = Some(path);
        self.document_modified = false;
    }

    pub fn document_opened(&mut self, path: PathBuf) {
        self.document_path = Some(path);
        self.document_modified = false;
        self.recovery_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::PersistenceState;
    use std::path::{Path, PathBuf};

    #[test]
    fn recovery_and_document_freshness_are_independent() {
        let mut state = PersistenceState::fresh(PathBuf::from("/tmp/recovery"));
        state.document_changed();
        assert!(state.document_modified());
        assert!(state.recovery_dirty());

        state.recovery_saved();
        assert!(state.document_modified());
        assert!(!state.recovery_dirty());

        state.document_saved(PathBuf::from("/tmp/drawing.sketchpad"));
        assert!(!state.document_modified());
        assert!(!state.recovery_dirty());
        assert_eq!(
            state.document_path(),
            Some(Path::new("/tmp/drawing.sketchpad"))
        );
    }

    #[test]
    fn recovered_work_has_no_assumed_native_path() {
        let state = PersistenceState::recovered(PathBuf::from("/tmp/recovery"));
        assert!(state.document_modified());
        assert!(!state.recovery_dirty());
        assert_eq!(state.document_path(), None);
    }

    #[test]
    fn opening_a_document_requires_a_new_recovery_snapshot() {
        let mut state = PersistenceState::fresh(PathBuf::from("/tmp/recovery"));
        state.document_opened(PathBuf::from("/tmp/opened.sketchpad"));

        assert!(!state.document_modified());
        assert!(state.recovery_dirty());
        assert_eq!(
            state.document_path(),
            Some(Path::new("/tmp/opened.sketchpad"))
        );
    }

    #[test]
    fn explicit_save_does_not_claim_recovery_is_current() {
        let mut state = PersistenceState::fresh(PathBuf::from("/tmp/recovery"));
        state.document_changed();
        state.document_saved(PathBuf::from("/tmp/drawing.sketchpad"));

        assert!(!state.document_modified());
        assert!(state.recovery_dirty());
    }
}
