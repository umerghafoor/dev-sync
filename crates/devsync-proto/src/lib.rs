//! Wire protocol between the devsync client and the remote agent.
//!
//! Transport is any byte stream (ssh stdio, a local pipe). Every message is a
//! frame: a 4-byte big-endian length followed by a bincode-encoded payload.
//! The client sends [`Request`]s; the agent answers each with exactly one
//! [`Response`], in order. Sessions are deliberately short-lived: a client
//! opens a session, performs one operation, and closes it, so nothing depends
//! on a connection surviving a flaky network.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Bumped on any incompatible change to the messages below.
pub const PROTOCOL_VERSION: u32 = 1;

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Hello { version: u32, agent_version: String, os: String, arch: String, root: String },
    Manifest(Vec<Entry>),
    Offsets(Vec<u64>),
    Ok,
    Chunk { data: Vec<u8>, eof: bool, size: u64 },
    JobStarted { id: String },
    JobOutput { state: JobState, data: Vec<u8>, next_offset: u64 },
    Jobs(Vec<JobInfo>),
    Changes(Vec<Entry>),
    Error { message: String },
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
    Codec(#[from] bincode::Error),
}

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    Ok(bincode::serialize(msg)?)
}

pub fn decode<T: for<'de> Deserialize<'de>>(buf: &[u8]) -> Result<T, FrameError> {
    Ok(bincode::deserialize(buf)?)
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
