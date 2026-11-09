//! Push (local -> remote), pull-back of files changed by remote commands,
//! and explicit pulls. Each attempt uses one short session; a failed attempt
//! is retried from scratch, which is cheap because hashes are cached and
//! partially uploaded files resume.

use crate::events::{Event, Reporter};
use crate::project::Project;
use crate::{ClientError, Result};
use devsync_fs::{scan, scan_one, write, HashCache, Plan};
use devsync_proto::{hash_hex, Entry, EntryKind, Hash, Request, Response, CHUNK_SIZE};
use devsync_transport::{Session, TransportError};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Requests in flight before waiting for acknowledgements.
const WINDOW: usize = 32;

#[derive(Debug, Default, Clone, Copy)]
pub struct PushOptions {
    pub dry_run: bool,
    /// Allow deleting more than `sync.max_delete` files.
    pub force: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PushStats {
    pub files: usize,
    pub bytes: u64,
    pub deleted: usize,
    pub plan: Plan,
}

/// Hashes of what the remote holds after our last push/pull, used to tell
/// "the user edited this locally" apart from "this is what we sent".
pub struct Snapshot {
    path: PathBuf,
    map: HashMap<String, Hash>,
}

impl Snapshot {
    pub fn load(p: &Project) -> Self {
        let path = p.state_dir().join("synced.v2.bin");
        let map = fs::read(&path).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default();
        Self { path, map }
    }

    pub fn get(&self, rel: &str) -> Option<&Hash> {
        self.map.get(rel)
    }

    pub fn set_all(&mut self, entries: &[Entry]) {
        self.map = entries.iter().map(|e| (e.path.clone(), e.hash)).collect();
    }

    pub fn set(&mut self, rel: &str, h: Hash) {
        self.map.insert(rel.to_string(), h);
    }

    pub fn save(&self) {
        if let Ok(b) = postcard::to_allocvec(&self.map) {
            if let Some(d) = self.path.parent() {
                let _ = fs::create_dir_all(d);
            }
            let tmp = self.path.with_extension("tmp");
            if fs::write(&tmp, b).is_ok() {
                let _ = fs::rename(tmp, &self.path);
            }
        }
    }
}

pub fn local_manifest(p: &Project) -> Result<Vec<Entry>> {
    let mut cache = HashCache::load(p.state_dir().join("hash-cache.v2.bin"));
    Ok(scan(&p.root, &p.ignore_spec(), &mut cache)?)
}

/// Files that differ from what was last synced (no network needed).
pub fn pending(p: &Project) -> Result<Vec<String>> {
    Ok(changes(p)?.into_iter().map(|c| c.path).collect())
}

/// How a local file differs from what the remote last received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Change {
    pub path: String,
    pub status: ChangeKind,
}

/// Local changes not yet on the remote, like `git status` (no network).
/// Deletions are listed only when `sync.delete` is on, since otherwise
/// they are never propagated.
pub fn changes(p: &Project) -> Result<Vec<Change>> {
    let snap = Snapshot::load(p);
    let local = local_manifest(p)?;
    let mut out: Vec<Change> = local
        .iter()
        .filter_map(|e| {
            match snap.get(&e.path) {
                None => Some(ChangeKind::Added),
                Some(h) if h != &e.hash => Some(ChangeKind::Modified),
                Some(_) => None,
            }
            .map(|status| Change { path: e.path.clone(), status })
        })
        .collect();
    if p.config.sync.delete {
        let present: std::collections::HashSet<&str> = local.iter().map(|e| e.path.as_str()).collect();
        out.extend(
            snap.map
                .keys()
                .filter(|k| !present.contains(k.as_str()))
                .map(|k| Change { path: k.clone(), status: ChangeKind::Deleted }),
        );
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

struct Pipe<'a> {
    s: &'a mut Session,
    inflight: usize,
}

impl Pipe<'_> {
    fn send(&mut self, req: &Request) -> Result<()> {
        self.s.send(req)?;
        self.inflight += 1;
        if self.inflight >= WINDOW {
            self.recv_one()?;
        }
        Ok(())
    }

    fn recv_one(&mut self) -> Result<()> {
        match self.s.recv()? {
            Response::Ok => {
                self.inflight -= 1;
                Ok(())
            }
            other => Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
        }
    }

    fn drain(&mut self) -> Result<()> {
        while self.inflight > 0 {
            self.recv_one()?;
        }
        Ok(())
    }
}

fn upload_file(pipe: &mut Pipe, root: &Path, e: &Entry, offset: u64) -> Result<()> {
    let path = root.join(&e.path);
    if e.size > offset {
        let mut f = fs::File::open(&path).map_err(|err| ClientError::Other(format!("{}: {err}", e.path)))?;
        f.seek(SeekFrom::Start(offset))?;
        let mut pos = offset;
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let n = read_full(&mut f, &mut buf)?;
            if n == 0 {
                break;
            }
            let data = lz4_flex::compress_prepend_size(&buf[..n]);
            pipe.send(&Request::PutChunk { hash: e.hash, offset: pos, data })?;
            pos += n as u64;
        }
    }
    pipe.send(&Request::PutCommit { path: e.path.clone(), hash: e.hash, mode: e.mode, mtime_ns: e.mtime_ns })
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn push_once(p: &Project, rep: &dyn Reporter, opts: PushOptions) -> Result<PushStats> {
    let started = Instant::now();
    let local = local_manifest(p)?;
    rep.event(Event::Connecting { host: p.host_label() });
    let mut s = p.connect()?;
    let remote = match s.call(&Request::Manifest { ignore: p.ignore_spec() })? {
        Response::Manifest(m) => m,
        other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
    };
    let plan = devsync_fs::plan(&local, &remote, p.config.sync.delete);
    rep.event(Event::Planned {
        upload: plan.upload.len(),
        upload_bytes: plan.upload_bytes(),
        delete: plan.delete.len(),
    });
    if plan.delete.len() > p.config.sync.max_delete && !opts.force {
        return Err(ClientError::Refused(format!(
            "refusing to delete {} remote files (sync.max_delete = {}). Re-run with --force if intended",
            plan.delete.len(),
            p.config.sync.max_delete
        )));
    }
    let mut stats = PushStats { plan: plan.clone(), ..Default::default() };
    if opts.dry_run || plan.is_empty() {
        s.close();
        if plan.is_empty() {
            Snapshot::load(p).tap_set_all(&local);
            rep.event(Event::UpToDate);
        }
        return Ok(stats);
    }

    let files: Vec<&Entry> = plan.upload.iter().filter(|e| e.kind == EntryKind::File).collect();
    let offsets = match s.call(&Request::PartialOffsets { files: files.iter().map(|e| e.hash).collect() })? {
        Response::Offsets(o) => o,
        other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
    };
    let offset_of: HashMap<&str, u64> =
        files.iter().zip(offsets.iter()).map(|(e, o)| (e.path.as_str(), (*o).min(e.size))).collect();

    let mut pipe = Pipe { s: &mut s, inflight: 0 };
    for e in &plan.upload {
        match e.kind {
            EntryKind::File => {
                upload_file(&mut pipe, &p.root, e, offset_of.get(e.path.as_str()).copied().unwrap_or(0))?
            }
            EntryKind::Symlink => {
                pipe.send(&Request::PutSymlink { path: e.path.clone(), target: e.link.clone().unwrap_or_default() })?
            }
        }
        stats.files += 1;
        stats.bytes += e.size;
        rep.event(Event::Uploaded { path: e.path.clone(), bytes: e.size });
    }
    if !plan.delete.is_empty() {
        for chunk in plan.delete.chunks(500) {
            pipe.send(&Request::Delete { paths: chunk.to_vec() })?;
        }
        for d in &plan.delete {
            rep.event(Event::Deleted { path: d.clone() });
        }
        stats.deleted = plan.delete.len();
    }
    pipe.drain()?;
    s.close();
    Snapshot::load(p).tap_set_all(&local);
    rep.event(Event::Synced {
        files: stats.files,
        bytes: stats.bytes,
        deleted: stats.deleted,
        millis: started.elapsed().as_millis() as u64,
    });
    Ok(stats)
}

trait TapSetAll {
    fn tap_set_all(self, e: &[Entry]);
}

impl TapSetAll for Snapshot {
    fn tap_set_all(mut self, e: &[Entry]) {
        self.set_all(e);
        self.save();
    }
}

/// Makes the remote tree match the local one.
pub fn push(p: &Project, rep: &dyn Reporter, opts: PushOptions) -> Result<PushStats> {
    push_with(p, rep, opts, p.retry_policy())
}

/// [`push`] with an explicit retry policy (e.g. single attempt for `watch`).
pub fn push_with(
    p: &Project,
    rep: &dyn Reporter,
    opts: PushOptions,
    policy: devsync_transport::RetryPolicy,
) -> Result<PushStats> {
    p.with_retry_policy(policy, rep, || push_once(p, rep, opts))
}

/// Downloads remote `e` to `dest` (verified, atomic).
fn download(s: &mut Session, dest_root: &Path, e: &Entry, dest: &Path) -> Result<()> {
    if e.kind == EntryKind::Symlink {
        write::write_symlink(dest_root, dest, e.link.as_deref().unwrap_or_default())?;
        return Ok(());
    }
    write::prepare_parent(dest_root, dest)?;
    let tmp = dest.with_file_name(format!(
        ".{}.devsync-dl",
        dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    let res = (|| -> Result<()> {
        let mut f = fs::File::create(&tmp)?;
        let mut offset = 0u64;
        loop {
            match s.call(&Request::GetChunk { path: e.path.clone(), offset })? {
                Response::Chunk { data, eof, .. } => {
                    let raw = lz4_flex::decompress_size_prepended(&data)
                        .map_err(|err| ClientError::Other(format!("corrupt chunk: {err}")))?;
                    f.write_all(&raw)?;
                    offset += raw.len() as u64;
                    if eof || raw.is_empty() {
                        break;
                    }
                }
                other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
            }
        }
        f.sync_all()?;
        drop(f);
        if devsync_fs::hash_file(&tmp)? != e.hash {
            return Err(TransportError::Remote(format!("{} changed during download (will retry)", e.path)).into());
        }
        write::commit_file(dest_root, &tmp, dest, e.mode, e.mtime_ns)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

#[derive(Debug, Default, Clone)]
pub struct PullBackStats {
    pub pulled: Vec<String>,
    pub conflicts: Vec<(String, String)>,
}

fn pull_back_once(p: &Project, rep: &dyn Reporter, job_id: &str) -> Result<PullBackStats> {
    let mut s = p.connect()?;
    let changed = match s.call(&Request::JobChanges { id: job_id.to_string(), ignore: p.ignore_spec() })? {
        Response::Changes(c) => c,
        other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
    };
    let mut snap = Snapshot::load(p);
    let mut cache = HashCache::load(p.state_dir().join("hash-cache.v2.bin"));
    let mut stats = PullBackStats::default();
    for e in changed {
        let local = scan_one(&p.root, &e.path, &mut cache)?;
        let local_hash = local.as_ref().map(|l| l.hash);
        if local_hash == Some(e.hash) && local.as_ref().map(|l| l.kind) == Some(e.kind) {
            snap.set(&e.path, e.hash);
            continue;
        }
        // Safe to overwrite only if the local file is exactly what we last
        // synced (or absent on both sides); otherwise the user edited it.
        let safe = local_hash.as_ref() == snap.get(&e.path);
        if safe {
            download(&mut s, &p.root, &e, &p.root.join(&e.path))?;
            snap.set(&e.path, e.hash);
            rep.event(Event::Pulled { path: e.path.clone() });
            stats.pulled.push(e.path);
        } else {
            let alt = format!("{}.devsync-remote", e.path);
            download(&mut s, &p.root, &e, &p.root.join(&alt))?;
            rep.event(Event::Conflict { path: e.path.clone(), saved_as: alt.clone() });
            stats.conflicts.push((e.path, alt));
        }
    }
    s.close();
    snap.save();
    Ok(stats)
}

/// Brings back files that remote job `job_id` created or modified.
pub fn pull_back(p: &Project, rep: &dyn Reporter, job_id: &str) -> Result<PullBackStats> {
    p.with_retry(rep, || pull_back_once(p, rep, job_id))
}

/// Downloads a remote file or directory (relative to the remote root) into
/// `dest_root` (default: the local project root), preserving relative paths.
pub fn pull(p: &Project, rep: &dyn Reporter, rel: &str, dest_root: Option<&Path>) -> Result<Vec<String>> {
    let rel = rel.trim_end_matches('/').to_string();
    let dest_root = dest_root.map(Path::to_path_buf).unwrap_or_else(|| p.root.clone());
    p.with_retry(rep, || {
        let mut s = p.connect()?;
        // Explicit pulls ignore the sync excludes: if you ask for it, you get it.
        let spec = devsync_proto::IgnoreSpec { use_gitignore: false, patterns: Vec::new() };
        let all = match s.call(&Request::Manifest { ignore: spec })? {
            Response::Manifest(m) => m,
            other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
        };
        let wanted: Vec<Entry> = all
            .into_iter()
            .filter(|e| rel.is_empty() || rel == "." || e.path == rel || e.path.starts_with(&format!("{rel}/")))
            .collect();
        if wanted.is_empty() {
            return Err(ClientError::Other(format!("nothing at remote path {rel:?}")));
        }
        let mut done = Vec::new();
        for e in &wanted {
            download(&mut s, &dest_root, e, &dest_root.join(&e.path))?;
            rep.event(Event::Pulled { path: e.path.clone() });
            done.push(e.path.clone());
        }
        s.close();
        Ok(done)
    })
}

pub fn short_hash(h: &Hash) -> String {
    hash_hex(h)[..8].to_string()
}
