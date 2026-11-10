//! Short synchronous commands (e.g. `git status`), answered in one reply.

use devsync_fs::safe_join;
use devsync_proto::Response;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Output beyond this is cut off (a short command shouldn't print more).
const MAX_OUTPUT: u64 = 32 * 1024 * 1024;

pub fn run(root: &std::path::Path, cwd: &str, argv: &[String], timeout_ms: u32) -> Result<Response, String> {
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let dir = if cwd.is_empty() { root.to_path_buf() } else { safe_join(root, cwd).map_err(|e| e.to_string())? };
    let mut child = Command::new(prog)
        .args(args)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Never wait for a password or an editor nobody can see.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .spawn()
        .map_err(|e| format!("cannot run {prog}: {e}"))?;
    let read = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(r) = r {
                let _ = r.take(MAX_OUTPUT).read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = read(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1000) as u64);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let mut stderr = err.join().unwrap_or_default();
    if status.is_none() {
        stderr.extend_from_slice(format!("\n{prog}: timed out after {} s\n", timeout_ms / 1000).as_bytes());
    }
    Ok(Response::RunOutput { code: status.and_then(|s| s.code()), stdout, stderr })
}
