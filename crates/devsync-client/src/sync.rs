//! Push (local -> remote), pull-back of files changed by remote commands,
//! and explicit pulls. Each attempt uses one short session; a failed attempt
//! is retried from scratch, which is cheap because hashes are cached and
//! partially uploaded files resume.

use crate::events::{Direction, Event, Reporter, Stage, Transfer};
use crate::project::Project;
use crate::{ClientError, Result};
use devsync_fs::{scan, scan_one, write, HashCache, Plan};
use devsync_proto::{hash_hex, Entry, EntryKind, Hash, Request, Response, CHUNK_SIZE};
use devsync_transport::Session;
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
    /// False until this machine has completed a sync with the remote.
    existed: bool,
    /// What the remote holds, as far as we know: the last remote scan,
    /// updated with every transfer since. Unlike `map` it is kept even when
    /// a sync doesn't finish, so "not on the remote yet" stays accurate
    /// before the first complete sync. `None` if never scanned.
    seen: Option<HashMap<String, Hash>>,
    seen_path: PathBuf,
}

fn load_map(path: &Path) -> Option<HashMap<String, Hash>> {
    fs::read(path).ok().and_then(|b| postcard::from_bytes(&b).ok())
}

fn save_map(path: &Path, map: &HashMap<String, Hash>) {
    if let Ok(b) = postcard::to_allocvec(map) {
        if let Some(d) = path.parent() {
            let _ = fs::create_dir_all(d);
        }
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, b).is_ok() {
            let _ = fs::rename(tmp, path);
        }
    }
}

impl Snapshot {
    pub fn load(p: &Project) -> Self {
        let path = p.state_dir().join("synced.v2.bin");
        let seen_path = p.state_dir().join("remote-seen.v1.bin");
        let saved = load_map(&path);
        Self { path, existed: saved.is_some(), map: saved.unwrap_or_default(), seen: load_map(&seen_path), seen_path }
    }

    /// Records a full remote scan (made with the project's sync excludes)
    /// right away, so it survives a sync that fails later.
    pub fn observe_remote(&mut self, remote: &[Entry]) {
        let seen: HashMap<String, Hash> = remote.iter().map(|e| (e.path.clone(), e.hash)).collect();
        save_map(&self.seen_path, &seen);
        self.seen = Some(seen);
    }

    /// The hash the remote is believed to hold for `rel`.
    pub fn remote(&self, rel: &str) -> Option<&Hash> {
        match &self.seen {
            Some(seen) => seen.get(rel),
            None => self.map.get(rel),
        }
    }

    fn remote_paths(&self) -> Vec<&String> {
        self.seen.as_ref().unwrap_or(&self.map).keys().collect()
    }

    /// Whether this machine has synced with the remote before.
    pub fn existed(&self) -> bool {
        self.existed
    }

    pub fn get(&self, rel: &str) -> Option<&Hash> {
        self.map.get(rel)
    }

    pub fn set_all(&mut self, entries: &[Entry]) {
        self.map = entries.iter().map(|e| (e.path.clone(), e.hash)).collect();
        self.seen = Some(self.map.clone());
    }

    pub fn set(&mut self, rel: &str, h: Hash) {
        self.map.insert(rel.to_string(), h);
        if let Some(seen) = &mut self.seen {
            seen.insert(rel.to_string(), h);
        }
    }

    pub fn save(&self) {
        save_map(&self.path, &self.map);
        self.save_seen();
    }

    /// Saves only what we know about the remote (not "a sync completed").
    pub fn save_seen(&self) {
        if let Some(seen) = &self.seen {
            save_map(&self.seen_path, seen);
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
            match snap.remote(&e.path) {
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
            snap.remote_paths()
                .into_iter()
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

fn upload_file(pipe: &mut Pipe, root: &Path, e: &Entry, offset: u64, prog: &mut Transfer) -> Result<()> {
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
            prog.bytes(n as u64);
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

/// What a pull would do (or did).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PullStats {
    /// Written to their own path (new on the remote, or changed only there).
    pub updated: Vec<String>,
    /// Edited locally and changed remotely: local kept, remote saved alongside.
    pub conflicts: Vec<(String, String)>,
    /// Changed on the remote while downloading: not taken this time.
    pub skipped: Vec<String>,
}

/// How a remote file relates to the local copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteStatus {
    /// Only on the remote.
    New,
    /// Changed on the remote; the local copy is untouched since the last sync.
    Changed,
    /// Differs, and the local copy was edited too (or was never synced).
    Edited,
    /// Identical on both sides.
    Same,
}

fn remote_status(local: Option<&Entry>, remote: &Entry, snap: &Snapshot) -> RemoteStatus {
    match local {
        None => RemoteStatus::New,
        Some(l) if devsync_fs::diff::same(l, remote) => RemoteStatus::Same,
        Some(l) if snap.get(&remote.path) == Some(&l.hash) => RemoteStatus::Changed,
        Some(_) => RemoteStatus::Edited,
    }
}

struct PullOpts<'a> {
    dry_run: bool,
    /// Overwrite local edits instead of saving the remote copy alongside.
    force: bool,
    /// Prefix for the progress message.
    what: &'a str,
}

/// Brings remote files into the local tree without losing local work:
/// - missing locally, or locally unchanged since the last sync → take the remote file;
/// - edited locally (differs from the last-synced version) → keep it and save
///   the remote version as `<file>.devsync-remote` (unless `force`).
///
/// With no sync history (first sync), every differing local file counts as
/// edited, so nothing local is ever overwritten.
fn pull_remote(
    s: &mut Session,
    p: &Project,
    rep: &dyn Reporter,
    local: &[Entry],
    remote: &[Entry],
    snap: &mut Snapshot,
    opts: &PullOpts,
) -> Result<PullStats> {
    let local_map: HashMap<&str, &Entry> = local.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut take = Vec::new();
    let mut keep_both = Vec::new();
    for e in remote {
        match remote_status(local_map.get(e.path.as_str()).copied(), e, snap) {
            RemoteStatus::Same => snap.set(&e.path, e.hash),
            RemoteStatus::Edited if !opts.force => keep_both.push(e),
            _ => take.push(e),
        }
    }
    let mut stats = PullStats::default();
    let already = remote.len() - take.len() - keep_both.len();
    // Record what is already identical on both sides right away.
    snap.save_seen();
    if take.is_empty() && keep_both.is_empty() {
        return Ok(stats);
    }
    let both = if keep_both.is_empty() {
        String::new()
    } else {
        format!("; {} file(s) you edited locally are kept (remote versions saved as *.devsync-remote)", keep_both.len())
    };
    rep.event(Event::Info {
        message: format!(
            "{}: {} {} file(s){both}{}",
            opts.what,
            if opts.dry_run { "would update" } else { "updating" },
            take.len(),
            if already > 0 { format!(" ({already} already here)") } else { String::new() }
        ),
    });
    let total_bytes: u64 = take.iter().chain(keep_both.iter()).map(|e| e.size).sum();
    rep.event(Event::PullPlanned {
        download: take.len() + keep_both.len(),
        download_bytes: total_bytes,
        conflicts: keep_both.len(),
        already,
    });
    let mut prog = Transfer::new(rep, Direction::Down, take.len() + keep_both.len(), total_bytes);
    if !opts.dry_run {
        rep.event(Event::Stage { stage: Stage::Download, message: "downloading".into() });
    }
    let mut checkpoint = Instant::now();
    for e in &take {
        if !opts.dry_run {
            prog.file(&e.path, e.size, 0);
            let ok = download(s, &p.root, e, &p.root.join(&e.path), &mut |n| prog.bytes(n))?;
            prog.file_done();
            if !ok {
                changed_while_reading(rep, &e.path);
                stats.skipped.push(e.path.clone());
                continue;
            }
            snap.set(&e.path, e.hash);
            // Remember what already arrived, in case the link drops.
            if checkpoint.elapsed() > std::time::Duration::from_secs(5) {
                snap.save_seen();
                checkpoint = Instant::now();
            }
        }
        rep.event(Event::Pulled { path: e.path.clone(), bytes: e.size });
        stats.updated.push(e.path.clone());
    }
    for e in &keep_both {
        let alt = format!("{}.devsync-remote", e.path);
        if !opts.dry_run {
            prog.file(&e.path, e.size, 0);
            let ok = download(s, &p.root, e, &p.root.join(&alt), &mut |n| prog.bytes(n))?;
            prog.file_done();
            if !ok {
                changed_while_reading(rep, &e.path);
                stats.skipped.push(e.path.clone());
                continue;
            }
        }
        rep.event(Event::Conflict { path: e.path.clone(), saved_as: alt.clone() });
        stats.conflicts.push((e.path.clone(), alt));
    }
    if !opts.dry_run {
        prog.finish();
    }
    Ok(stats)
}

/// Requests that make the agent scan the whole project. A first scan of a
/// large tree (hashing gigabytes) can take long; liveness is still guarded by
/// ssh keepalives, so allow up to an hour instead of the normal timeout.
fn scan_call(s: &mut Session, req: &Request) -> Result<Response> {
    let normal = s.timeout();
    s.set_timeout(normal.max(std::time::Duration::from_secs(3600)));
    let r = s.call(req);
    s.set_timeout(normal);
    Ok(r?)
}

fn stage(rep: &dyn Reporter, stage: Stage, message: &str) {
    rep.event(Event::Stage { stage, message: message.into() });
}

fn remote_manifest(s: &mut Session, ignore: devsync_proto::IgnoreSpec) -> Result<Vec<Entry>> {
    match scan_call(s, &Request::Manifest { ignore })? {
        Response::Manifest(m) => Ok(m),
        other => Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
    }
}

/// Normalises user-supplied remote paths: `./src/` → `src`, `.` → "" (everything).
fn normalize_paths(paths: &[String]) -> Result<Vec<String>> {
    paths
        .iter()
        .map(|raw| {
            let mut s = raw.trim().to_string();
            while let Some(rest) = s.strip_prefix("./") {
                s = rest.to_string();
            }
            let s = s.trim_end_matches('/').to_string();
            if s == "." || s.is_empty() {
                return Ok(String::new());
            }
            devsync_fs::validate_rel(&s).map_err(|e| ClientError::Other(e.to_string()))?;
            Ok(s)
        })
        .collect()
}

fn under(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| p.is_empty() || path == p || path.starts_with(&format!("{p}/")))
}

/// `devsync pull` without a path: bring the whole project's remote changes
/// down safely (sync excludes apply; local edits are never overwritten).
pub fn pull_project(p: &Project, rep: &dyn Reporter, dry_run: bool) -> Result<PullStats> {
    let _lock = p.sync_lock(rep)?;
    p.with_retry(rep, || {
        stage(rep, Stage::ScanLocal, "checking local files");
        let local = local_manifest(p)?;
        rep.event(Event::Connecting { host: p.host_label() });
        let mut s = p.connect()?;
        large_remote_guard(&mut s, p)?;
        stage(rep, Stage::ScanRemote, "checking remote files");
        let remote = remote_manifest(&mut s, p.ignore_spec())?;
        let mut snap = Snapshot::load(p);
        snap.observe_remote(&remote);
        let opts = PullOpts { dry_run, force: false, what: "pulling from remote" };
        let stats = pull_remote(&mut s, p, rep, &local, &remote, &mut snap, &opts)?;
        s.close();
        if !dry_run {
            snap.save();
        }
        Ok(stats)
    })
}

/// `devsync pull <paths…>`: like [`pull_project`], limited to the given
/// remote files/directories. Because the paths are named explicitly,
/// `sync.ignore` and .gitignore don't apply; built-in excludes and
/// .devsyncignore still do (both sides scan with them).
/// `force` overwrites local edits instead of saving `*.devsync-remote`.
pub fn pull_paths(p: &Project, rep: &dyn Reporter, paths: &[String], force: bool, dry_run: bool) -> Result<PullStats> {
    let wanted = normalize_paths(paths)?;
    let _lock = p.sync_lock(rep)?;
    p.with_retry(rep, || {
        rep.event(Event::Connecting { host: p.host_label() });
        let mut s = p.connect()?;
        stage(rep, Stage::ScanRemote, "checking remote files");
        let all = remote_manifest(&mut s, devsync_proto::IgnoreSpec::default())?;
        let remote: Vec<Entry> = all.into_iter().filter(|e| under(&e.path, &wanted)).collect();
        if remote.is_empty() {
            return Err(ClientError::Other(format!("nothing on the remote at {}", paths.join(", "))));
        }
        // Local state of exactly these files, whatever the excludes say.
        let mut cache = HashCache::load(p.state_dir().join("hash-cache.v2.bin"));
        let mut local = Vec::new();
        for e in &remote {
            if let Some(l) = scan_one(&p.root, &e.path, &mut cache)? {
                local.push(l);
            }
        }
        let mut snap = Snapshot::load(p);
        let opts = PullOpts { dry_run, force, what: "pulling from remote" };
        let stats = pull_remote(&mut s, p, rep, &local, &remote, &mut snap, &opts)?;
        s.close();
        if !dry_run {
            snap.save();
        }
        Ok(stats)
    })
}

/// One remote file and how it compares with the local copy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RemoteFile {
    pub path: String,
    pub size: u64,
    pub status: RemoteStatus,
}

/// Lists remote files (optionally under `paths`) with their status relative
/// to the local tree. `all` includes paths the sync excludes would skip.
pub fn list_remote(p: &Project, rep: &dyn Reporter, paths: &[String], all: bool) -> Result<Vec<RemoteFile>> {
    let wanted = normalize_paths(paths)?;
    p.with_retry(rep, || {
        let mut s = p.connect()?;
        let spec = if all { devsync_proto::IgnoreSpec::default() } else { p.ignore_spec() };
        let remote = remote_manifest(&mut s, spec)?;
        s.close();
        let snap = Snapshot::load(p);
        let mut cache = HashCache::load(p.state_dir().join("hash-cache.v2.bin"));
        let mut out = Vec::new();
        for e in remote.iter().filter(|e| wanted.is_empty() || under(&e.path, &wanted)) {
            let local = scan_one(&p.root, &e.path, &mut cache)?;
            out.push(RemoteFile {
                path: e.path.clone(),
                size: e.size,
                status: remote_status(local.as_ref(), e, &snap),
            });
        }
        Ok(out)
    })
}

/// Whether the user has chosen what to sync (any include, or a path excluded).
fn has_selection(p: &Project) -> bool {
    let s = &p.config.sync;
    !s.include.is_empty() || !s.ignore.is_empty()
}

/// Guard before syncing a whole remote project that nobody has chosen
/// from: look at the top level cheaply (no hashing, time-limited sizing)
/// and refuse if it is large, listing the biggest folders.
fn large_remote_guard(s: &mut Session, p: &Project) -> Result<()> {
    let limit = p.config.sync.large_remote_limit_mb.saturating_mul(1024 * 1024);
    if limit == 0 || has_selection(p) {
        return Ok(());
    }
    let nodes = match s.call(&Request::Tree { path: String::new(), budget_ms: 3000 })? {
        Response::Tree(n) => n,
        other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
    };
    let mut nodes: Vec<_> = nodes.into_iter().filter(|n| !n.excluded).collect();
    let total: u64 = nodes.iter().map(|n| n.size).sum();
    let incomplete = nodes.iter().any(|n| !n.complete);
    if total <= limit && !incomplete {
        return Ok(());
    }
    nodes.sort_by_key(|n| std::cmp::Reverse(n.size));
    let fmt = |b: u64| {
        let mb = b as f64 / 1_048_576.0;
        if mb >= 1024.0 {
            format!("{:.1} GB", mb / 1024.0)
        } else {
            format!("{mb:.1} MB")
        }
    };
    let biggest: Vec<String> = nodes
        .iter()
        .filter(|n| n.dir)
        .take(5)
        .map(|n| format!("{}/ {}{}", n.path, if n.complete { "" } else { "≥ " }, fmt(n.size)))
        .collect();
    Err(ClientError::NeedsSelection(format!(
        "the remote project is large ({}{}); choose what to sync before the first sync. \
         Biggest folders: {}. Browse with `devsync tree`, then e.g. `devsync unselect {}` \
         (or in VS Code: devsync sidebar → Remote Files). To sync everything anyway, set \
         sync.large_remote_limit_mb = 0",
        if incomplete { "at least " } else { "" },
        fmt(total),
        biggest.join(", "),
        nodes.iter().find(|n| n.dir).map(|n| n.path.as_str()).unwrap_or("data")
    )))
}

fn push_once(p: &Project, rep: &dyn Reporter, opts: PushOptions) -> Result<PushStats> {
    let started = Instant::now();
    stage(rep, Stage::ScanLocal, "checking local files");
    let mut local = local_manifest(p)?;
    rep.event(Event::Connecting { host: p.host_label() });
    let mut s = p.connect()?;
    if !Snapshot::load(p).existed() {
        large_remote_guard(&mut s, p)?;
    }
    stage(rep, Stage::ScanRemote, "checking remote files");
    let remote = match scan_call(&mut s, &Request::Manifest { ignore: p.ignore_spec() })? {
        Response::Manifest(m) => m,
        other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
    };
    // First sync from this machine: start from the remote's code, so an empty
    // or new folder never overwrites (or, with delete on, wipes) the project.
    let mut snap = Snapshot::load(p);
    snap.observe_remote(&remote);
    let mut skipped = Vec::new();
    if p.config.sync.initial_pull && !remote.is_empty() && !snap.existed() {
        let first = PullOpts { dry_run: opts.dry_run, force: false, what: "first sync with this remote" };
        let pulled = pull_remote(&mut s, p, rep, &local, &remote, &mut snap, &first)?;
        if !opts.dry_run && !(pulled.updated.is_empty() && pulled.conflicts.is_empty()) {
            local = local_manifest(p)?;
        }
        skipped = pulled.skipped;
    }
    let mut plan = devsync_fs::plan(&local, &remote, p.config.sync.delete);
    // Never push an old local copy over a remote file we could not fetch.
    plan.upload.retain(|e| !skipped.contains(&e.path));
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

    stage(rep, Stage::Upload, "uploading");
    let mut prog = Transfer::new(rep, Direction::Up, plan.upload.len(), plan.upload_bytes());
    let mut pipe = Pipe { s: &mut s, inflight: 0 };
    for e in &plan.upload {
        let resumed = offset_of.get(e.path.as_str()).copied().unwrap_or(0);
        prog.file(&e.path, e.size, resumed);
        match e.kind {
            EntryKind::File => upload_file(&mut pipe, &p.root, e, resumed, &mut prog)?,
            EntryKind::Symlink => {
                pipe.send(&Request::PutSymlink { path: e.path.clone(), target: e.link.clone().unwrap_or_default() })?
            }
        }
        prog.file_done();
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
    prog.finish();
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
    let _lock = p.sync_lock(rep)?;
    p.with_retry_policy(policy, rep, || push_once(p, rep, opts))
}

/// Downloads remote `e` to `dest` (verified, atomic). An interrupted
/// download resumes where it stopped on the next attempt. Returns
/// `Ok(false)` if the remote file changed while it was being read (nothing
/// is written; the next sync gets the new version).
/// `on_bytes` is called with each chunk's size, for progress.
fn download(s: &mut Session, dest_root: &Path, e: &Entry, dest: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<bool> {
    if e.kind == EntryKind::Symlink {
        write::write_symlink(dest_root, dest, e.link.as_deref().unwrap_or_default())?;
        return Ok(true);
    }
    write::prepare_parent(dest_root, dest)?;
    let tmp = dest.with_file_name(format!(
        ".{}.devsync-dl",
        dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    // A partial file from an interrupted attempt: continue after it.
    let mut resumed = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if resumed > e.size {
        let _ = fs::remove_file(&tmp);
        resumed = 0;
    }
    loop {
        // On failure the partial file is kept to resume from; the hash
        // check below rejects it if it turns out to be stale.
        fetch_into(s, e, &tmp, resumed, on_bytes)?;
        if devsync_fs::hash_file(&tmp)? == e.hash {
            write::commit_file(dest_root, &tmp, dest, e.mode, e.mtime_ns)?;
            return Ok(true);
        }
        let _ = fs::remove_file(&tmp);
        if resumed > 0 {
            // The kept part may be from an older version: start over once.
            resumed = 0;
            continue;
        }
        return Ok(false);
    }
}

fn fetch_into(s: &mut Session, e: &Entry, tmp: &Path, from: u64, on_bytes: &mut dyn FnMut(u64)) -> Result<()> {
    let mut f = fs::OpenOptions::new().create(true).append(from > 0).write(true).truncate(from == 0).open(tmp)?;
    if from > 0 {
        on_bytes(from);
    }
    let mut offset = from;
    loop {
        if offset >= e.size && e.size > 0 {
            break;
        }
        match s.call(&Request::GetChunk { path: e.path.clone(), offset })? {
            Response::Chunk { data, eof, .. } => {
                let raw = lz4_flex::decompress_size_prepended(&data)
                    .map_err(|err| ClientError::Other(format!("corrupt chunk: {err}")))?;
                f.write_all(&raw)?;
                offset += raw.len() as u64;
                on_bytes(raw.len() as u64);
                if eof || raw.is_empty() {
                    break;
                }
            }
            other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
        }
    }
    f.sync_all()?;
    Ok(())
}

fn changed_while_reading(rep: &dyn Reporter, path: &str) {
    rep.event(Event::Warning {
        message: format!("{path} changed on the remote while it was downloading; skipped, the next sync gets it"),
    });
}

#[derive(Debug, Default, Clone)]
pub struct PullBackStats {
    pub pulled: Vec<String>,
    pub conflicts: Vec<(String, String)>,
}

fn pull_back_once(p: &Project, rep: &dyn Reporter, job_id: &str) -> Result<PullBackStats> {
    let mut s = p.connect()?;
    let changed = match scan_call(&mut s, &Request::JobChanges { id: job_id.to_string(), ignore: p.ignore_spec() })? {
        Response::Changes(c) => c,
        other => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
    };
    let mut snap = Snapshot::load(p);
    let mut cache = HashCache::load(p.state_dir().join("hash-cache.v2.bin"));
    let mut stats = PullBackStats::default();
    let mut prog = Transfer::new(rep, Direction::Down, changed.len(), changed.iter().map(|e| e.size).sum());
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
        prog.file(&e.path, e.size, 0);
        if safe {
            if !download(&mut s, &p.root, &e, &p.root.join(&e.path), &mut |n| prog.bytes(n))? {
                prog.file_done();
                changed_while_reading(rep, &e.path);
                continue;
            }
            prog.file_done();
            snap.set(&e.path, e.hash);
            rep.event(Event::Pulled { path: e.path.clone(), bytes: e.size });
            stats.pulled.push(e.path);
        } else {
            let alt = format!("{}.devsync-remote", e.path);
            if !download(&mut s, &p.root, &e, &p.root.join(&alt), &mut |n| prog.bytes(n))? {
                prog.file_done();
                changed_while_reading(rep, &e.path);
                continue;
            }
            prog.file_done();
            rep.event(Event::Conflict { path: e.path.clone(), saved_as: alt.clone() });
            stats.conflicts.push((e.path, alt));
        }
    }
    if prog_used(&stats) {
        prog.finish();
    }
    s.close();
    snap.save();
    Ok(stats)
}

fn prog_used(s: &PullBackStats) -> bool {
    !s.pulled.is_empty() || !s.conflicts.is_empty()
}

/// Brings back files that remote job `job_id` created or modified.
pub fn pull_back(p: &Project, rep: &dyn Reporter, job_id: &str) -> Result<PullBackStats> {
    let _lock = p.sync_lock(rep)?;
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
        let spec = devsync_proto::IgnoreSpec::default();
        let all = match scan_call(&mut s, &Request::Manifest { ignore: spec })? {
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
        let mut prog = Transfer::new(rep, Direction::Down, wanted.len(), wanted.iter().map(|e| e.size).sum());
        for e in &wanted {
            prog.file(&e.path, e.size, 0);
            let ok = download(&mut s, &dest_root, e, &dest_root.join(&e.path), &mut |n| prog.bytes(n))?;
            prog.file_done();
            if !ok {
                changed_while_reading(rep, &e.path);
                continue;
            }
            rep.event(Event::Pulled { path: e.path.clone(), bytes: e.size });
            done.push(e.path.clone());
        }
        prog.finish();
        s.close();
        Ok(done)
    })
}

pub fn short_hash(h: &Hash) -> String {
    hash_hex(h)[..8].to_string()
}

/// Direct children of a remote directory ("" = project root) with sizes.
/// Nothing is hashed; sizing of large directories stops at a time budget.
pub fn remote_tree(p: &Project, rep: &dyn Reporter, rel: &str) -> Result<Vec<devsync_proto::TreeNode>> {
    let rel = devsync_fs::norm_rel(rel);
    p.with_retry(rep, || {
        let mut s = p.connect()?;
        let r = s.call(&Request::Tree { path: rel.clone(), budget_ms: 3000 })?;
        s.close();
        match r {
            Response::Tree(nodes) => Ok(nodes),
            other => Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
        }
    })
}
