//! Session-only, lossless history storage. Handles own their reservations and
//! files; shared recovery snapshots keep data alive after history eviction.
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use std::{
    collections::HashMap,
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveError {
    Unavailable,
    Full,
    Corrupt,
}
impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "undo cache could not be read or written",
            Self::Full => "undo cache is full",
            Self::Corrupt => "undo cache failed lossless integrity validation",
        })
    }
}
impl std::error::Error for ArchiveError {}

#[derive(Clone, Copy, Debug, Default)]
pub struct ArchiveUsage {
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}
#[derive(Debug)]
struct Store {
    directory: PathBuf,
    memory_limit: u64,
    disk_limit: u64,
    state: Mutex<State>,
    _lease: fs::File,
}
#[derive(Debug, Default)]
struct State {
    usage: ArchiveUsage,
    next_file: u64,
    // Weak handles do not extend blob lifetime. Hash collisions are checked
    // against encoded bytes before sharing; a hash is never pixel identity.
    shared: HashMap<(u64, usize), Vec<Weak<Blob>>>,
}
impl Drop for Store {
    fn drop(&mut self) {
        remove_session_files(&self.directory);
    }
}
#[derive(Clone, Debug)]
pub struct HistoryStore(Arc<Store>);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);
impl HistoryStore {
    pub fn new(parent: &Path, memory_limit: u64, disk_limit: u64) -> Result<Self, ArchiveError> {
        fs::create_dir_all(parent).map_err(|_| ArchiveError::Unavailable)?;
        clean_abandoned_sessions(parent);
        let directory = loop {
            let serial = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("session-{}-{serial}", std::process::id()));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(ArchiveError::Unavailable),
            }
        };
        let lease = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.join("lease"))
            .map_err(|_| ArchiveError::Unavailable)?;
        lease.try_lock().map_err(|_| ArchiveError::Unavailable)?;
        Ok(Self(Arc::new(Store {
            directory,
            memory_limit,
            disk_limit,
            state: Mutex::new(State::default()),
            _lease: lease,
        })))
    }
    pub fn usage(&self) -> ArchiveUsage {
        self.0.state.lock().unwrap().usage
    }
    pub fn store(&self, raw: &[u8]) -> Result<HistoryBlob, ArchiveError> {
        // Each caller supplies one bounded tile region, never a whole canvas.
        if raw.is_empty() || raw.len() > 1024 * 1024 || !raw.len().is_multiple_of(16) {
            return Err(ArchiveError::Corrupt);
        }
        let checksum = hash(raw);
        let uniform = raw.chunks_exact(16).all(|pixel| pixel == &raw[..16]);
        let (codec, encoded) = if uniform {
            (Codec::Uniform, raw[..16].to_vec())
        } else {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder
                .write_all(raw)
                .map_err(|_| ArchiveError::Unavailable)?;
            let compressed = encoder.finish().map_err(|_| ArchiveError::Unavailable)?;
            if compressed.len() < raw.len() {
                (Codec::Deflate, compressed)
            } else {
                (Codec::Raw, raw.to_vec())
            }
        };
        let key = (checksum, raw.len());
        // Do not hold the accounting lock while dropping the last blob handle.
        let candidates: Vec<_> = {
            let mut state = self.0.state.lock().unwrap();
            if state.next_file.is_multiple_of(256) {
                state.shared.retain(|_, handles| {
                    handles.retain(|h| h.strong_count() != 0);
                    !handles.is_empty()
                });
            }
            state
                .shared
                .get(&key)
                .into_iter()
                .flatten()
                .filter_map(Weak::upgrade)
                .collect()
        };
        for existing in candidates {
            if existing.codec == codec && existing.encoded().is_ok_and(|bytes| bytes == encoded) {
                return Ok(HistoryBlob(existing));
            }
        }
        let bytes = encoded.len() as u64;
        let mut state = self.0.state.lock().unwrap();
        state.next_file += 1;
        let location = if state.usage.memory_bytes + bytes <= self.0.memory_limit {
            state.usage.memory_bytes += bytes;
            Location::Memory(encoded)
        } else {
            let charged = bytes.div_ceil(4096) * 4096;
            if state.usage.disk_bytes + charged > self.0.disk_limit {
                return Err(ArchiveError::Full);
            }
            let path = self.0.directory.join(format!("{}.blob", state.next_file));
            // The handle is published only after a complete successful write.
            if fs::write(&path, &encoded).is_err() {
                if fs::remove_file(path).is_err() {
                    state.usage.disk_bytes += charged;
                }
                return Err(ArchiveError::Unavailable);
            }
            state.usage.disk_bytes += charged;
            Location::Disk(path)
        };
        let blob = Arc::new(Blob {
            store: self.0.clone(),
            location,
            codec,
            checksum,
            raw_len: raw.len(),
            encoded_len: bytes,
        });
        state
            .shared
            .entry(key)
            .or_default()
            .push(Arc::downgrade(&blob));
        Ok(HistoryBlob(blob))
    }
}
// Only remove our known cache files after acquiring a session lease. A live
// document (including another process) keeps its lease locked until all shared
// history and recovery handles have gone away. No source/document files live here.
fn clean_abandoned_sessions(parent: &Path) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("session-")
            || !entry.file_type().is_ok_and(|t| t.is_dir())
        {
            continue;
        }
        let path = entry.path();
        let Ok(lease) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join("lease"))
        else {
            continue;
        };
        if lease.try_lock().is_err() {
            continue;
        }
        remove_session_files(&path);
    }
}
fn remove_session_files(path: &Path) {
    let mut removed = true;
    if let Ok(files) = fs::read_dir(path) {
        for file in files.flatten() {
            let name = file.file_name();
            let name = name.to_string_lossy();
            if name
                .strip_suffix(".blob")
                .is_some_and(|stem| stem.parse::<u64>().is_ok())
            {
                removed &= fs::remove_file(file.path()).is_ok();
            }
        }
    }
    // Leave an unlocked lease for a later cleanup if the filesystem failed.
    if removed {
        let _ = fs::remove_file(path.join("lease"));
        let _ = fs::remove_dir(path);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Uniform,
    Deflate,
    Raw,
}
#[derive(Debug)]
enum Location {
    Memory(Vec<u8>),
    Disk(PathBuf),
}
#[derive(Debug)]
struct Blob {
    store: Arc<Store>,
    location: Location,
    codec: Codec,
    checksum: u64,
    raw_len: usize,
    encoded_len: u64,
}
impl Blob {
    fn encoded(&self) -> Result<Vec<u8>, ArchiveError> {
        match &self.location {
            Location::Memory(bytes) => Ok(bytes.clone()),
            Location::Disk(path) => {
                let file = fs::File::open(path).map_err(|_| ArchiveError::Unavailable)?;
                if file
                    .metadata()
                    .map_err(|_| ArchiveError::Unavailable)?
                    .len()
                    != self.encoded_len
                {
                    return Err(ArchiveError::Corrupt);
                }
                let mut bytes = Vec::with_capacity(self.encoded_len as usize);
                file.take(self.encoded_len + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| ArchiveError::Unavailable)?;
                if bytes.len() as u64 != self.encoded_len {
                    return Err(ArchiveError::Corrupt);
                }
                Ok(bytes)
            }
        }
    }
}
impl Drop for Blob {
    fn drop(&mut self) {
        let mut state = self.store.state.lock().unwrap();
        match &self.location {
            Location::Memory(_) => state.usage.memory_bytes -= self.encoded_len,
            Location::Disk(path) => {
                // Failed deletion remains charged to the session's disk budget.
                if fs::remove_file(path).is_ok() {
                    state.usage.disk_bytes -= self.encoded_len.div_ceil(4096) * 4096;
                }
            }
        }
    }
}
#[derive(Clone, Debug)]
pub struct HistoryBlob(Arc<Blob>);
impl PartialEq for HistoryBlob {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl HistoryBlob {
    pub fn memory_bytes(&self) -> u64 {
        std::mem::size_of::<Blob>() as u64
            + match self.0.location {
                Location::Memory(_) => self.0.encoded_len,
                Location::Disk(_) => 0,
            }
    }
    pub fn read(&self) -> Result<Vec<u8>, ArchiveError> {
        let encoded = self.0.encoded()?;
        let raw = match self.0.codec {
            Codec::Uniform => {
                if encoded.len() != 16 {
                    return Err(ArchiveError::Corrupt);
                }
                encoded.repeat(self.0.raw_len / 16)
            }
            Codec::Raw => encoded,
            Codec::Deflate => {
                let mut raw = Vec::with_capacity(self.0.raw_len);
                ZlibDecoder::new(encoded.as_slice())
                    .take(self.0.raw_len as u64 + 1)
                    .read_to_end(&mut raw)
                    .map_err(|_| ArchiveError::Corrupt)?;
                raw
            }
        };
        if raw.len() != self.0.raw_len || hash(&raw) != self.0.checksum {
            return Err(ArchiveError::Corrupt);
        }
        Ok(raw)
    }
}
// A session-local integrity checksum, not an authentication mechanism.
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store(memory: u64, disk: u64) -> HistoryStore {
        HistoryStore::new(
            &std::env::temp_dir().join("sketchpad-history-tests"),
            memory,
            disk,
        )
        .unwrap()
    }
    #[test]
    fn uniform_data_shares_one_reservation_and_outlives_store() {
        let store = store(16, 0);
        let raw = [0, 0, 0, 128, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12].repeat(16384);
        let first = store.store(&raw).unwrap();
        let second = store.store(&raw).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.usage().memory_bytes, 16);
        let directory = store.0.directory.clone();
        drop(store);
        drop(first);
        assert_eq!(second.read().unwrap(), raw);
        drop(second);
        assert!(!directory.exists());
    }
    #[test]
    fn disk_corruption_is_rejected_and_last_handle_releases_budget() {
        let store = store(0, 100_000);
        let raw: Vec<u8> = (0..65536).map(|i| (i * 7 % 251) as u8).collect();
        let blob = store.store(&raw).unwrap();
        assert_eq!(blob.read().unwrap(), raw);
        assert!(store.usage().disk_bytes < raw.len() as u64);
        let Location::Disk(path) = &blob.0.location else {
            panic!()
        };
        fs::write(path, vec![7; blob.0.encoded_len as usize]).unwrap();
        assert_eq!(blob.read(), Err(ArchiveError::Corrupt));
        drop(blob);
        assert_eq!(store.usage().disk_bytes, 0);
    }
    #[test]
    fn full_cache_does_not_change_live_handles() {
        let store = store(0, 4096);
        let first = store.store(&[1; 256]).unwrap();
        assert_eq!(store.store(&[2; 256]), Err(ArchiveError::Full));
        assert_eq!(first.read().unwrap(), [1; 256]);
        drop(first);
        assert!(store.store(&[2; 256]).is_ok());
    }
    #[test]
    fn cleanup_preserves_active_sessions_and_reclaims_abandoned_cache() {
        let first = store(0, 4096);
        let parent = first.0.directory.parent().unwrap();
        let live = first.store(&[3; 256]).unwrap();
        let abandoned = parent.join(format!(
            "session-abandoned-{}",
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&abandoned).unwrap();
        fs::write(abandoned.join("lease"), []).unwrap();
        fs::write(abandoned.join("1.blob"), [7; 512]).unwrap();
        clean_abandoned_sessions(parent);
        assert!(!abandoned.exists());
        assert_eq!(live.read().unwrap(), [3; 256]);
    }

    #[test]
    fn incompressible_float_bits_round_trip_without_expansion() {
        let store = store(65536, 0);
        let mut seed = 41u32;
        let raw: Vec<u8> = (0..65536)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let blob = store.store(&raw).unwrap();
        assert_eq!(blob.0.codec, Codec::Raw);
        assert_eq!(blob.read().unwrap(), raw);
        assert_eq!(store.usage().memory_bytes, 65536);
    }
}
