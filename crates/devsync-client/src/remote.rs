//! One-off remote reads and short commands (no sync, no job).

use crate::events::Reporter;
use crate::project::Project;
use crate::{ClientError, Result};
use devsync_proto::{Request, Response};
use devsync_transport::TransportError;
use std::time::Duration;

/// Output of a short remote command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunOutput {
    pub code: Option<i32>,
    #[serde(serialize_with = "lossy")]
    pub stdout: Vec<u8>,
    #[serde(serialize_with = "lossy")]
    pub stderr: Vec<u8>,
}

fn lossy<S: serde::Serializer>(b: &[u8], s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&String::from_utf8_lossy(b))
}

/// Contents of a remote file (project-relative), or `None` if it doesn't
/// exist there. Read-only, so retried on network errors.
pub fn cat(p: &Project, rep: &dyn Reporter, rel: &str) -> Result<Option<Vec<u8>>> {
    let rel = devsync_fs::norm_rel(rel);
    devsync_fs::validate_rel(&rel).map_err(|e| ClientError::Other(e.to_string()))?;
    p.with_retry(rep, || {
        let mut s = p.connect()?;
        let mut out = Vec::new();
        loop {
            match s.call(&Request::GetChunk { path: rel.clone(), offset: out.len() as u64 }) {
                Ok(Response::Chunk { data, eof, .. }) => {
                    let raw = lz4_flex::decompress_size_prepended(&data)
                        .map_err(|e| ClientError::Other(format!("corrupt chunk: {e}")))?;
                    let empty = raw.is_empty();
                    out.extend_from_slice(&raw);
                    if eof || empty {
                        break;
                    }
                }
                Err(TransportError::Remote(m)) if m.contains("os error 2") || m.contains("No such file") => {
                    s.close();
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
                Ok(other) => return Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
            }
        }
        s.close();
        Ok(Some(out))
    })
}

/// Runs `argv` on the remote in the invocation directory and waits for it.
/// Not retried: the command may not be safe to run twice.
pub fn run(p: &Project, argv: &[String], timeout: Duration) -> Result<RunOutput> {
    let mut s = p.connect()?;
    s.set_timeout(timeout + Duration::from_secs(30));
    let r = s.call(&Request::Run {
        cwd: p.rel_cwd.clone(),
        argv: argv.to_vec(),
        timeout_ms: timeout.as_millis().min(u32::MAX as u128) as u32,
    })?;
    s.close();
    match r {
        Response::RunOutput { code, stdout, stderr } => Ok(RunOutput { code, stdout, stderr }),
        other => Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
    }
}
