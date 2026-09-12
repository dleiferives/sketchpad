use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[derive(serde::Serialize, serde::Deserialize)]
struct RecoverySession {
    version: u32,
    recovery_header: [u8; 32],
    document_path: Option<PathBuf>,
    document_header: Option<[u8; 32]>,
    modified: bool,
}

// Checkpoint headers contain format, payload length and the checksum of all
// pixels/metadata. Startup validates the checkpoint before consulting this hint.
fn checkpoint_header(path: &Path) -> io::Result<[u8; 32]> {
    let mut header = [0; 32];
    std::fs::File::open(path)?.read_exact(&mut header)?;
    Ok(header)
}

fn session_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".session.json");
    PathBuf::from(value)
}

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

    pub fn recover_with_session(recovery_path: PathBuf) -> Self {
        let restore = (|| -> Option<RecoverySession> {
            let bytes = std::fs::read(session_path(&recovery_path)).ok()?;
            if bytes.len() > 65_536 {
                return None;
            }
            let session: RecoverySession = serde_json::from_slice(&bytes).ok()?;
            if session.version != 1
                || checkpoint_header(&recovery_path).ok()? != session.recovery_header
            {
                return None;
            }
            if let Some(path) = &session.document_path {
                if checkpoint_header(path).ok()? != session.document_header? {
                    return None;
                }
            }
            Some(session)
        })();
        match restore {
            Some(session) => Self {
                recovery_path,
                document_path: session.document_path,
                document_modified: session.modified,
                recovery_dirty: false,
            },
            None => Self::recovered(recovery_path),
        }
    }

    /// Write the filename hint only when recovery contains the current drawing.
    pub fn remember_session(&self) -> io::Result<()> {
        if self.recovery_dirty {
            return Ok(());
        }
        let document_path = self.document_path.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            }
        });
        let session = RecoverySession {
            version: 1,
            recovery_header: checkpoint_header(&self.recovery_path)?,
            document_header: document_path
                .as_ref()
                .and_then(|path| checkpoint_header(path).ok()),
            document_path,
            modified: self.document_modified,
        };
        let path = session_path(&self.recovery_path);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = path.with_extension(format!("{stamp}.tmp"));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(&session)?)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
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

    pub fn require_recovery(&mut self) {
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
    fn filename_restore_requires_matching_recovery_and_document_contents() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join(format!(".artifacts/session-{stamp}"));
        std::fs::create_dir_all(&directory).unwrap();
        let recovery = directory.join("recovery.skpr");
        let saved = directory.join("drawing.sketchpad");
        let original = crate::document::Document::new(128, 128, 128).unwrap();
        crate::checkpoint::save_document_atomic(&recovery, &original).unwrap();
        crate::checkpoint::save_document_atomic(&saved, &original).unwrap();
        let mut state = PersistenceState::fresh(recovery.clone());
        state.document_saved(saved.clone());
        state.remember_session().unwrap();
        let restored = PersistenceState::recover_with_session(recovery.clone());
        assert_eq!(restored.document_path(), Some(saved.as_path()));
        assert!(!restored.document_modified());
        let different = crate::document::Document::new(256, 128, 128).unwrap();
        crate::checkpoint::save_document_atomic(&saved, &different).unwrap();
        let changed_file = PersistenceState::recover_with_session(recovery.clone());
        assert_eq!(
            changed_file.document_path(),
            None,
            "an external edit must not be silently overwritten by recovered work"
        );
        assert!(changed_file.document_modified());
        crate::checkpoint::save_document_atomic(&saved, &original).unwrap();
        crate::checkpoint::save_document_atomic(&recovery, &different).unwrap();
        let stale_hint = PersistenceState::recover_with_session(recovery);
        assert_eq!(stale_hint.document_path(), None);
        assert!(stale_hint.document_modified());
    }

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
