//! Wire protocol between the devsync client and the remote agent.
//!
//! Transport is any byte stream (ssh stdio, a local pipe). Every message is a
//! frame: a 4-byte big-endian length followed by a postcard-encoded payload.
//! The client sends [`Request`]s; the agent answers each with exactly one
//! [`Response`], in order. Sessions are deliberately short-lived: a client
//! opens a session, performs one operation, and closes it, so nothing depends
//! on a connection surviving a flaky network.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Bumped on any incompatible change to the messages below.
pub const PROTOCOL_VERSION: u32 = 5;

/// Sent by the agent before anything else. The client discards whatever
/// precedes it (output from the remote user's shell startup files) and sends
/// nothing until it arrives, so startup files can neither corrupt nor consume
/// protocol data.
pub const GREETING: &[u8] = b"\x00\x7fdevsync-agent-ready\x00\n";

/// Most bytes of shell noise tolerated before the greeting.
pub const MAX_PREAMBLE: usize = 1024 * 1024;

/// Upper bound for a single frame; protects both sides from garbage input.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Raw (uncompressed) size of a file chunk on the wire.
pub const CHUNK_SIZE: usize = 1024 * 1024;

/// BLAKE3 digest of a file's content (or a symlink's target).
pub type Hash = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    Symlink,
}

/// One file in a manifest. Paths are relative, `/`-separated, never contain `..`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix permission bits (only the executable bit is significant for sync).
    pub mode: u32,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    pub hash: Hash,
    /// Symlink target when `kind == Symlink`.
    pub link: Option<String>,
}

/// Which files are part of the synced tree. Both sides must scan with the
/// same spec so their manifests are comparable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IgnoreSpec {
    /// Honour `.gitignore` files (off by default: gitignored files such as
    /// `.env` are usually needed to run the code).
    pub use_gitignore: bool,
    /// Extra gitignore-style exclude patterns from the config file.
    pub patterns: Vec<String>,
    /// If non-empty, only these paths (files or directories, relative to the
    /// root) are part of the tree; everything else is skipped without being
    /// descended into ("sparse sync").
    pub include: Vec<String>,
}

/// One entry of a directory listing (`Request::Tree`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeNode {
    /// Path relative to the project root.
    pub path: String,
    pub dir: bool,
    /// Total size in bytes (for directories: as far as counted).
    pub size: u64,
    /// Number of files (for directories: as far as counted).
    pub files: u64,
    /// False when counting stopped at the time budget (sizes are lower bounds).
    pub complete: bool,
    /// Matches a built-in exclude (never synced).
    pub excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    /// Working directory relative to the project root ("" = root).
    pub cwd: String,
    pub command: String,
    /// Shell snippet run before the command (e.g. `source ~/.nvm/nvm.sh`).
    pub prelude: String,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Running,
    Exited(i32),
    /// The supervisor disappeared without recording an exit code
    /// (e.g. the remote machine rebooted).
    Lost,
}

impl JobState {
    pub fn is_finished(self) -> bool {
        !matches!(self, JobState::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub started_unix: i64,
    pub state: JobState,
    pub output_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Hello {
        version: u32,
    },
    /// Full manifest of the remote tree.
    Manifest {
        ignore: IgnoreSpec,
    },
    /// For each (path, hash) about to be uploaded: bytes already received in
    /// an earlier, interrupted session (resume point).
    PartialOffsets {
        files: Vec<Hash>,
    },
    /// Append lz4-compressed data to the partial upload for `hash`.
    PutChunk {
        hash: Hash,
        offset: u64,
        data: Vec<u8>,
    },
    /// Verify the partial upload and atomically move it into place.
    PutCommit {
        path: String,
        hash: Hash,
        mode: u32,
        mtime_ns: i128,
    },
    PutSymlink {
        path: String,
        target: String,
    },
    Delete {
        paths: Vec<String>,
    },
    /// Read a chunk of a remote file (lz4-compressed in the response).
    GetChunk {
        path: String,
        offset: u64,
    },
    JobStart {
        spec: JobSpec,
    },
    /// Fetch job output from `offset` (at most `max_bytes`) plus its state.
    JobPoll {
        id: String,
        offset: u64,
        max_bytes: u32,
    },
    JobKill {
        id: String,
    },
    JobList,
    /// Files whose content changed since the job started.
    JobChanges {
        id: String,
        ignore: IgnoreSpec,
    },
    /// List the direct children of `path` ("" = root) with sizes, counting
    /// for at most `budget_ms` overall. Nothing is read or hashed.
    Tree {
        path: String,
        budget_ms: u32,
    },
    /// Run a short command (e.g. `git status`) synchronously in `cwd`
    /// (relative to the root) and return its output. No shell is involved.
    /// Long-running work belongs in jobs, which survive disconnects.
    Run {
        cwd: String,
        argv: Vec<String>,
        timeout_ms: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Hello {
        version: u32,
        agent_version: String,
        os: String,
        arch: String,
        root: String,
    },
    Manifest(Vec<Entry>),
    Offsets(Vec<u64>),
    Ok,
    Chunk {
        data: Vec<u8>,
        eof: bool,
        size: u64,
    },
    JobStarted {
        id: String,
    },
    JobOutput {
        state: JobState,
        data: Vec<u8>,
        next_offset: u64,
    },
    Jobs(Vec<JobInfo>),
    Changes(Vec<Entry>),
    Error {
        message: String,
    },
    Tree(Vec<TreeNode>),
    /// Result of [`Request::Run`]; `code` is `None` if killed (timeout/signal).
    RunOutput {
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("connection closed")]
    Closed,
    #[error("frame too large ({0} bytes)")]
    TooLarge(usize),
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("decode: {0}")]
    Codec(#[from] postcard::Error),
}

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    Ok(postcard::to_allocvec(msg)?)
}

pub fn decode<T: for<'de> Deserialize<'de>>(buf: &[u8]) -> Result<T, FrameError> {
    Ok(postcard::from_bytes(buf)?)
}

pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    if payload.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(payload.len()));
    }
    w.write_all(&(payload.len() as u32).to_be_bytes())?;
    w.write_all(payload)?;
    Ok(())
}

/// Reads one frame. Returns [`FrameError::Closed`] on a clean EOF at a frame boundary.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn send<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), FrameError> {
    write_frame(w, &encode(msg)?)
}

pub fn recv<R: Read, T: for<'de> Deserialize<'de>>(r: &mut R) -> Result<T, FrameError> {
    decode(&read_frame(r)?)
}

/// Reads until [`GREETING`] and returns whatever came before it.
pub fn read_greeting<R: Read>(r: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut seen = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => return Err(FrameError::Closed),
            Ok(_) => seen.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
        if seen.ends_with(GREETING) {
            seen.truncate(seen.len() - GREETING.len());
            return Ok(seen);
        }
        if seen.len() > MAX_PREAMBLE {
            return Err(FrameError::TooLarge(seen.len()));
        }
    }
}

pub fn hash_hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let req = Request::JobStart {
            spec: JobSpec {
                cwd: "src".into(),
                command: "echo 'hi' && exit 3".into(),
                prelude: String::new(),
                env: vec![("A".into(), "b".into())],
            },
        };
        let mut buf = Vec::new();
        send(&mut buf, &req).unwrap();
        send(&mut buf, &Request::JobList).unwrap();
        let mut r = &buf[..];
        assert_eq!(recv::<_, Request>(&mut r).unwrap(), req);
        assert_eq!(recv::<_, Request>(&mut r).unwrap(), Request::JobList);
        assert!(matches!(recv::<_, Request>(&mut r), Err(FrameError::Closed)));
    }

    #[test]
    fn greeting_skips_shell_noise() {
        let mut stream = b"Welcome to box!\nconda activated\n".to_vec();
        stream.extend_from_slice(GREETING);
        send(&mut stream, &Request::JobList).unwrap();
        let mut r = &stream[..];
        assert_eq!(read_greeting(&mut r).unwrap(), b"Welcome to box!\nconda activated\n");
        assert_eq!(recv::<_, Request>(&mut r).unwrap(), Request::JobList);
        let mut clean = GREETING.to_vec();
        assert!(read_greeting(&mut &clean[..]).unwrap().is_empty());
        clean.truncate(5);
        assert!(matches!(read_greeting(&mut &clean[..]), Err(FrameError::Closed)));
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
        assert!(matches!(read_frame(&mut &buf[..]), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn truncated_frame_is_io_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&10u32.to_be_bytes());
        buf.extend_from_slice(b"abc");
        assert!(matches!(read_frame(&mut &buf[..]), Err(FrameError::Io(_))));
    }
}
