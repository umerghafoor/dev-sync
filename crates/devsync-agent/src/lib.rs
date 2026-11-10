//! The remote agent. One process is started per (short-lived) session over
//! ssh stdio; it answers requests until the client hangs up. Long-running
//! commands are handed to a detached supervisor process (see [`jobs`]), so no
//! session ever has to stay open while a command runs.

pub mod files;
pub mod jobs;
mod run;

use devsync_proto::{self as proto, FrameError, Request, Response, PROTOCOL_VERSION};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

pub use jobs::run_supervisor;

pub struct Agent {
    root: PathBuf,
    state: PathBuf,
}

/// Expands `~` / `~/x` with `$HOME`; relative paths are taken from `$HOME`
/// (matching what a plain `ssh host cmd` would see).
pub fn expand_root(raw: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        home.join(raw)
    }
}

impl Agent {
    pub fn open(root_raw: &str) -> std::io::Result<Self> {
        let root = expand_root(root_raw);
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let state = root.join(devsync_fs::AGENT_STATE_DIR);
        devsync_fs::hide_from_git(&state)?;
        std::fs::create_dir_all(state.join("jobs"))?;
        std::fs::create_dir_all(state.join("partial"))?;
        Ok(Self { root, state })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Serves requests until the peer closes the stream.
    pub fn serve<R: Read, W: Write>(&self, r: R, w: W) -> Result<(), FrameError> {
        let mut r = BufReader::with_capacity(256 * 1024, r);
        let mut w = BufWriter::with_capacity(256 * 1024, w);
        w.write_all(proto::GREETING)?;
        w.flush()?;
        loop {
            let req: Request = match proto::recv(&mut r) {
                Ok(req) => req,
                Err(FrameError::Closed) => return Ok(()),
                Err(e) => return Err(e),
            };
            let resp = self.handle(req).unwrap_or_else(|message| Response::Error { message });
            proto::send(&mut w, &resp)?;
            w.flush()?;
        }
    }

    pub fn handle(&self, req: Request) -> Result<Response, String> {
        match req {
            Request::Hello { version } => {
                if version != PROTOCOL_VERSION {
                    return Err(format!("protocol mismatch: client {version}, agent {PROTOCOL_VERSION}"));
                }
                jobs::gc(&self.state);
                files::gc(&self.state);
                Ok(Response::Hello {
                    version: PROTOCOL_VERSION,
                    agent_version: env!("CARGO_PKG_VERSION").to_string(),
                    os: std::env::consts::OS.to_string(),
                    arch: std::env::consts::ARCH.to_string(),
                    root: self.root.display().to_string(),
                })
            }
            Request::Manifest { ignore } => files::manifest(&self.root, &self.state, &ignore),
            Request::PartialOffsets { files } => Ok(files::partial_offsets(&self.state, &files)),
            Request::PutChunk { hash, offset, data } => files::put_chunk(&self.state, &hash, offset, &data),
            Request::PutCommit { path, hash, mode, mtime_ns } => {
                files::put_commit(&self.root, &self.state, &path, &hash, mode, mtime_ns)
            }
            Request::PutSymlink { path, target } => files::put_symlink(&self.root, &path, &target),
            Request::Delete { paths } => files::delete(&self.root, &paths),
            Request::GetChunk { path, offset } => files::get_chunk(&self.root, &path, offset),
            Request::JobStart { spec } => jobs::start(&self.root, &self.state, &spec),
            Request::JobPoll { id, offset, max_bytes } => jobs::poll(&self.state, &id, offset, max_bytes),
            Request::JobKill { id } => jobs::kill(&self.state, &id),
            Request::JobList => Ok(Response::Jobs(jobs::list(&self.state))),
            Request::JobChanges { id, ignore } => jobs::changes(&self.root, &self.state, &id, &ignore),
            Request::Tree { path, budget_ms } => files::tree(&self.root, &path, budget_ms),
            Request::Run { cwd, argv, timeout_ms } => run::run(&self.root, &cwd, &argv, timeout_ms),
        }
    }
}
