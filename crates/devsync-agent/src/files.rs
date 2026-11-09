//! File transfer endpoints. Uploads are staged per content hash under
//! `.devsync-agent/partial/`, so an interrupted transfer resumes where it
//! stopped in the next session, and nothing is visible until verified.

use devsync_fs::{safe_join, write, HashCache};
use devsync_proto::{hash_hex, Hash, IgnoreSpec, Response, CHUNK_SIZE};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const PARTIAL_TTL: Duration = Duration::from_secs(3 * 24 * 3600);

fn partial(state: &Path, hash: &Hash) -> PathBuf {
    state.join("partial").join(hash_hex(hash))
}

pub fn manifest(root: &Path, state: &Path, ignore: &IgnoreSpec) -> Result<Response, String> {
    let mut cache = HashCache::load(state.join("hash-cache.v2.bin"));
    devsync_fs::scan(root, ignore, &mut cache).map(Response::Manifest).map_err(|e| e.to_string())
}

pub fn partial_offsets(state: &Path, hashes: &[Hash]) -> Response {
    Response::Offsets(hashes.iter().map(|h| fs::metadata(partial(state, h)).map(|m| m.len()).unwrap_or(0)).collect())
}

pub fn put_chunk(state: &Path, hash: &Hash, offset: u64, data: &[u8]) -> Result<Response, String> {
    let raw = lz4_flex::decompress_size_prepended(data).map_err(|e| format!("corrupt chunk: {e}"))?;
    let p = partial(state, hash);
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&p)
        .map_err(|e| format!("{}: {e}", p.display()))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if offset > len {
        return Err(format!("chunk gap: have {len} bytes, got offset {offset}"));
    }
    if offset < len {
        f.set_len(offset).map_err(|e| e.to_string())?;
    }
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    f.write_all(&raw).map_err(|e| e.to_string())?;
    Ok(Response::Ok)
}

pub fn put_commit(
    root: &Path,
    state: &Path,
    rel: &str,
    hash: &Hash,
    mode: u32,
    mtime_ns: i128,
) -> Result<Response, String> {
    let dest = safe_join(root, rel).map_err(|e| e.to_string())?;
    let p = partial(state, hash);
    if !p.exists() {
        // Empty files never send a chunk.
        fs::write(&p, b"").map_err(|e| e.to_string())?;
    }
    let got = devsync_fs::hash_file(&p).map_err(|e| e.to_string())?;
    if &got != hash {
        let _ = fs::remove_file(&p);
        return Err(format!("{rel}: content hash mismatch after transfer (will retry)"));
    }
    write::commit_file(root, &p, &dest, mode, mtime_ns).map_err(|e| e.to_string())?;
    Ok(Response::Ok)
}

pub fn put_symlink(root: &Path, rel: &str, target: &str) -> Result<Response, String> {
    let dest = safe_join(root, rel).map_err(|e| e.to_string())?;
    write::write_symlink(root, &dest, target).map_err(|e| e.to_string())?;
    Ok(Response::Ok)
}

pub fn delete(root: &Path, rels: &[String]) -> Result<Response, String> {
    for rel in rels {
        let dest = safe_join(root, rel).map_err(|e| e.to_string())?;
        write::remove_path(root, &dest).map_err(|e| e.to_string())?;
    }
    Ok(Response::Ok)
}

pub fn get_chunk(root: &Path, rel: &str, offset: u64) -> Result<Response, String> {
    let path = safe_join(root, rel).map_err(|e| e.to_string())?;
    let mut f = fs::File::open(&path).map_err(|e| format!("{rel}: {e}"))?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity(CHUNK_SIZE.min(size.saturating_sub(offset) as usize));
    f.take(CHUNK_SIZE as u64).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let eof = offset + buf.len() as u64 >= size;
    Ok(Response::Chunk { data: lz4_flex::compress_prepend_size(&buf), eof, size })
}

/// Drops stale partial uploads.
pub fn gc(state: &Path) {
    let Ok(rd) = fs::read_dir(state.join("partial")) else { return };
    let now = SystemTime::now();
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| now.duration_since(t).unwrap_or_default() > PARTIAL_TTL)
            .unwrap_or(false);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}
