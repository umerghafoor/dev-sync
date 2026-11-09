//! Detached jobs. `start` writes the job description to
//! `.devsync-agent/jobs/<id>/` and launches a supervisor (`devsync agent
//! run-job <dir>`) in its own session. The supervisor runs the command with
//! output appended to `output.log` and records `exit_code` when it ends.
//! Clients read output by byte offset at their own pace, from any number of
//! short sessions, so network drops never affect the command.
//!
//! Job directory layout:
//!   meta.json   id, command, cwd, start time
//!   env.json    extra environment
//!   cmd.sh      prelude + command
//!   output.log  combined stdout/stderr
//!   sup.pid     supervisor pid
//!   child.pid   command pid (also its process group id)
//!   exit_code   written atomically when the command ends

use devsync_fs::{validate_rel, HashCache};
use devsync_proto::{IgnoreSpec, JobInfo, JobSpec, JobState, Response};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const JOB_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const KEEP_JOBS: usize = 200;
/// A job without a live supervisor this long after start is considered lost.
const START_GRACE_NS: i128 = 10_000_000_000;

#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    id: String,
    command: String,
    cwd: String,
    started_unix: i64,
    started_ns: i128,
}

fn now_ns() -> i128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0)
}

fn job_dir(state: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!("invalid job id: {id:?}"));
    }
    let d = state.join("jobs").join(id);
    if d.is_dir() {
        Ok(d)
    } else {
        Err(format!("no such job: {id}"))
    }
}

fn read_meta(dir: &Path) -> Option<Meta> {
    serde_json::from_slice(&fs::read(dir.join("meta.json")).ok()?).ok()
}

fn read_i32(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(tmp, path)
}

fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 only checks for existence/permission.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn new_id() -> String {
    let ns = now_ns();
    let secs = ns / 1_000_000_000;
    let salt = (ns as u64 ^ (std::process::id() as u64).rotate_left(17)) & 0xffff;
    format!("{secs}-{salt:04x}")
}

pub fn start(root: &Path, state: &Path, spec: &JobSpec) -> Result<Response, String> {
    let cwd = if spec.cwd.is_empty() {
        root.to_path_buf()
    } else {
        validate_rel(&spec.cwd).map_err(|e| e.to_string())?;
        root.join(&spec.cwd)
    };
    fs::create_dir_all(&cwd).map_err(|e| format!("{}: {e}", cwd.display()))?;

    let id = new_id();
    let dir = state.join("jobs").join(&id);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let meta = Meta {
        id: id.clone(),
        command: spec.command.clone(),
        cwd: spec.cwd.clone(),
        started_unix: (now_ns() / 1_000_000_000) as i64,
        started_ns: now_ns(),
    };
    let script = if spec.prelude.trim().is_empty() {
        format!("{}\n", spec.command)
    } else {
        format!("{}\n{}\n", spec.prelude, spec.command)
    };
    let io = |r: std::io::Result<()>| r.map_err(|e| e.to_string());
    io(fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta).unwrap_or_default()))?;
    io(fs::write(dir.join("env.json"), serde_json::to_vec(&spec.env).unwrap_or_default()))?;
    io(fs::write(dir.join("cmd.sh"), script))?;
    io(fs::write(dir.join("output.log"), b""))?;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let child = Command::new(exe)
        .arg("agent")
        .arg("run-job")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // No process_group here: the supervisor calls setsid(), which fails
        // for a process-group leader.
        .spawn()
        .map_err(|e| format!("failed to launch job supervisor: {e}"))?;
    io(fs::write(dir.join("sup.pid"), child.id().to_string()))?;
    // Reap the supervisor's exit status in the background so it never lingers
    // as a zombie while this agent session is alive.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(Response::JobStarted { id })
}

/// Entry point of the supervisor process. Never returns an error to a peer:
/// every failure ends up in `output.log` and `exit_code`.
pub fn run_supervisor(dir: &Path) -> i32 {
    // SAFETY: detaching from the ssh session; plain libc calls without
    // memory-safety implications.
    unsafe {
        libc::setsid();
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let finish = |code: i32| {
        let _ = write_atomic(&dir.join("exit_code"), code.to_string().as_bytes());
        code
    };
    let log = match fs::OpenOptions::new().append(true).create(true).open(dir.join("output.log")) {
        Ok(f) => f,
        Err(_) => return finish(127),
    };
    let Some(meta) = read_meta(dir) else { return finish(127) };
    let env: Vec<(String, String)> = fs::read(dir.join("env.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    // jobs/<id> -> .devsync-agent -> project root
    let root = dir.ancestors().nth(3).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let cwd = if meta.cwd.is_empty() { root } else { root.join(&meta.cwd) };

    let (shell, login) = if Path::new("/bin/bash").exists() || Path::new("/usr/bin/bash").exists() {
        ("bash", "-l")
    } else {
        ("sh", "-l")
    };
    let stderr = match log.try_clone() {
        Ok(f) => f,
        Err(_) => return finish(127),
    };
    let mut cmd = Command::new(shell);
    cmd.arg(login)
        .arg(dir.join("cmd.sh"))
        .current_dir(&cwd)
        .envs(env)
        .env("DEVSYNC_JOB", &meta.id)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(stderr)
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            use std::io::Write;
            if let Ok(mut f) = fs::OpenOptions::new().append(true).open(dir.join("output.log")) {
                let _ = writeln!(f, "devsync: failed to start {shell}: {e}");
            }
            return finish(127);
        }
    };
    let _ = fs::write(dir.join("child.pid"), child.id().to_string());
    let code = match child.wait() {
        Ok(st) => {
            use std::os::unix::process::ExitStatusExt;
            st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0))
        }
        Err(_) => 1,
    };
    finish(code)
}

fn state_of(dir: &Path, meta: &Meta) -> JobState {
    if let Some(code) = read_i32(&dir.join("exit_code")) {
        return JobState::Exited(code);
    }
    let sup = read_i32(&dir.join("sup.pid")).unwrap_or(0);
    if pid_alive(sup) || now_ns() - meta.started_ns < START_GRACE_NS {
        JobState::Running
    } else if let Some(code) = read_i32(&dir.join("exit_code")) {
        JobState::Exited(code) // finished between the two checks
    } else {
        JobState::Lost
    }
}

pub fn poll(state: &Path, id: &str, offset: u64, max_bytes: u32) -> Result<Response, String> {
    let dir = job_dir(state, id)?;
    let meta = read_meta(&dir).ok_or("job metadata unreadable")?;
    // Determine state *before* reading output so a job that finishes in
    // between is reported as running and its remaining output is not missed.
    let st = state_of(&dir, &meta);
    let mut f = fs::File::open(dir.join("output.log")).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let start = offset.min(len);
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    f.take(max_bytes as u64).read_to_end(&mut data).map_err(|e| e.to_string())?;
    let next = start + data.len() as u64;
    let state = if st.is_finished() && next < len { JobState::Running } else { st };
    Ok(Response::JobOutput { state, data, next_offset: next })
}

pub fn kill(state: &Path, id: &str) -> Result<Response, String> {
    let dir = job_dir(state, id)?;
    if dir.join("exit_code").exists() {
        return Ok(Response::Ok);
    }
    let target = read_i32(&dir.join("child.pid")).or_else(|| read_i32(&dir.join("sup.pid")));
    let Some(pid) = target else { return Ok(Response::Ok) };
    // SAFETY: signalling the job's process group (its pid == pgid).
    unsafe {
        if libc::kill(-pid, libc::SIGTERM) != 0 {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    for _ in 0..30 {
        if dir.join("exit_code").exists() {
            return Ok(Response::Ok);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    Ok(Response::Ok)
}

pub fn list(state: &Path) -> Vec<JobInfo> {
    let Ok(rd) = fs::read_dir(state.join("jobs")) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.iter()
        .rev()
        .take(50)
        .filter_map(|d| {
            let meta = read_meta(d)?;
            Some(JobInfo {
                state: state_of(d, &meta),
                output_len: fs::metadata(d.join("output.log")).map(|m| m.len()).unwrap_or(0),
                id: meta.id,
                command: meta.command,
                cwd: meta.cwd,
                started_unix: meta.started_unix,
            })
        })
        .collect()
}

pub fn changes(root: &Path, state: &Path, id: &str, ignore: &IgnoreSpec) -> Result<Response, String> {
    let dir = job_dir(state, id)?;
    let meta = read_meta(&dir).ok_or("job metadata unreadable")?;
    let mut cache = HashCache::load(state.join("hash-cache.bin"));
    let all = devsync_fs::scan(root, ignore, &mut cache).map_err(|e| e.to_string())?;
    let since = meta.started_ns - 1_000_000_000;
    Ok(Response::Changes(all.into_iter().filter(|e| e.mtime_ns >= since).collect()))
}

/// Removes old finished jobs, keeping at most `KEEP_JOBS`.
pub fn gc(state: &Path) {
    let Ok(rd) = fs::read_dir(state.join("jobs")) else { return };
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    let now = now_ns();
    let excess = dirs.len().saturating_sub(KEEP_JOBS);
    for (i, d) in dirs.iter().enumerate() {
        let Some(meta) = read_meta(d) else { continue };
        let finished = state_of(d, &meta).is_finished();
        let expired = now - meta.started_ns > JOB_TTL.as_nanos() as i128;
        if finished && (expired || i < excess) {
            let _ = fs::remove_dir_all(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_ids() {
        let d = tempfile::tempdir().unwrap();
        for id in ["", "../x", "a/b", "x y"] {
            assert!(job_dir(d.path(), id).is_err());
        }
    }

    #[test]
    fn poll_reports_running_until_output_drained() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path();
        let dir = state.join("jobs").join("1-abcd");
        fs::create_dir_all(&dir).unwrap();
        let meta = Meta { id: "1-abcd".into(), command: "x".into(), cwd: String::new(), started_unix: 1, started_ns: 1 };
        fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        fs::write(dir.join("output.log"), b"hello world").unwrap();
        fs::write(dir.join("exit_code"), b"3").unwrap();
        match poll(state, "1-abcd", 0, 5).unwrap() {
            Response::JobOutput { state, data, next_offset } => {
                assert_eq!(state, JobState::Running);
                assert_eq!(data, b"hello");
                assert_eq!(next_offset, 5);
            }
            r => panic!("{r:?}"),
        }
        match poll(state, "1-abcd", 5, 100).unwrap() {
            Response::JobOutput { state, data, .. } => {
                assert_eq!(state, JobState::Exited(3));
                assert_eq!(data, b" world");
            }
            r => panic!("{r:?}"),
        }
    }
}
