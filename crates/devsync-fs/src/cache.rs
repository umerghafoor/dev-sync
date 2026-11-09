//! Persistent (size, mtime) -> hash cache so unchanged files are not re-read.

use devsync_proto::Hash;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Files modified this close to the cache write time are re-hashed: their
/// mtime may not have changed even though their content did ("racy" entries).
const RACY_WINDOW_NS: i128 = 2_000_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Item {
    size: u64,
    mtime_ns: i128,
    hash: Hash,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    written_at_ns: i128,
    items: HashMap<String, Item>,
}

#[derive(Debug, Default)]
pub struct HashCache {
    file: Option<PathBuf>,
    old: Data,
    new: HashMap<String, Item>,
    dirty: bool,
}

pub fn now_ns() -> i128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0)
}

impl HashCache {
    /// In-memory cache that is never persisted.
    pub fn ephemeral() -> Self {
        Self::default()
    }

    /// Loads the cache from `file`; a missing or corrupt file yields an empty cache.
    pub fn load(file: impl Into<PathBuf>) -> Self {
        let file = file.into();
        let old = std::fs::read(&file).ok().and_then(|b| bincode::deserialize::<Data>(&b).ok()).unwrap_or_default();
        Self { file: Some(file), old, new: HashMap::new(), dirty: false }
    }

    pub fn get(&self, rel: &str, size: u64, mtime_ns: i128) -> Option<Hash> {
        let it = self.new.get(rel).or_else(|| self.old.items.get(rel))?;
        let racy = mtime_ns >= self.old.written_at_ns - RACY_WINDOW_NS;
        (it.size == size && it.mtime_ns == mtime_ns && !racy).then_some(it.hash)
    }

    /// Records an entry seen in the current scan. Entries not recorded are
    /// dropped on save, so the cache never grows with deleted files.
    pub fn put(&mut self, rel: &str, size: u64, mtime_ns: i128, hash: Hash) {
        let changed =
            self.old.items.get(rel).is_none_or(|o| o.size != size || o.mtime_ns != mtime_ns || o.hash != hash);
        self.dirty |= changed;
        self.new.insert(rel.to_string(), Item { size, mtime_ns, hash });
    }

    /// Persists entries recorded since load (atomically). Errors are ignored:
    /// the cache is an optimisation only.
    pub fn save(&mut self) {
        let Some(file) = &self.file else { return };
        if !self.dirty && self.new.len() == self.old.items.len() {
            return;
        }
        let data = Data { written_at_ns: now_ns(), items: std::mem::take(&mut self.new) };
        if let Ok(bytes) = bincode::serialize(&data) {
            let _ = write_atomic(file, &bytes);
        }
        self.old = data;
        self.dirty = false;
    }
}

fn write_atomic(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, file)
}
