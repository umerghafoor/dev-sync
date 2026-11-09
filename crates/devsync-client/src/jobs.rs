//! Remote jobs from the client's side.
//!
//! A job is submitted in one short session and then *collected*: every poll
//! is its own short session that fetches new output and appends it to the
//! local results folder `.devsync/jobs/<id>/`:
//!
//!   command      the command line
//!   output.log   combined stdout/stderr, as far as fetched
//!   status.json  state, exit code, fetched offset, pull-back status
//!
//! No connection is held while a command runs; tools (and coding agents) can
//! simply read these files.

use crate::events::{Event, Reporter};
use crate::project::Project;
use crate::{ClientError, Result};
use devsync_proto::{JobInfo, JobSpec, JobState, Request, Response};
use devsync_transport::Session;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// Output fetched per request.
const POLL_BYTES: u32 = 512 * 1024;
/// Output drained per session before yielding.
const SESSION_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalJob {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub host: String,
    pub started_unix: i64,
    /// "running", "exited" or "lost".
    pub state: String,
    pub exit_code: Option<i32>,
    /// Bytes of output fetched so far.
    pub offset: u64,
    pub pulled_back: bool,
}

impl LocalJob {
    pub fn finished(&self) -> bool {
        self.state != "running"
    }

    fn apply(&mut self, st: JobState) {
        match st {
            JobState::Running => self.state = "running".into(),
            JobState::Exited(c) => {
                self.state = "exited".into();
                self.exit_code = Some(c);
            }
            JobState::Lost => self.state = "lost".into(),
        }
    }
}

pub fn jobs_dir(p: &Project) -> PathBuf {
    p.state_dir().join("jobs")
}

pub fn job_dir(p: &Project, id: &str) -> PathBuf {
    jobs_dir(p).join(id)
}

pub fn output_path(p: &Project, id: &str) -> PathBuf {
    job_dir(p, id).join("output.log")
}

pub fn save(p: &Project, j: &LocalJob) -> Result<()> {
    let dir = job_dir(p, &j.id);
    fs::create_dir_all(&dir)?;
    let tmp = dir.join("status.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(j).map_err(|e| ClientError::Other(e.to_string()))?)?;
    fs::rename(tmp, dir.join("status.json"))?;
    Ok(())
}

pub fn load(p: &Project, id: &str) -> Option<LocalJob> {
    serde_json::from_slice(&fs::read(job_dir(p, id).join("status.json")).ok()?).ok()
}

/// Locally known jobs, newest first.
pub fn local_jobs(p: &Project) -> Vec<LocalJob> {
    let Ok(rd) = fs::read_dir(jobs_dir(p)) else { return Vec::new() };
    let mut v: Vec<LocalJob> = rd.flatten().filter_map(|e| load(p, &e.file_name().to_string_lossy())).collect();
    v.sort_by(|a, b| b.id.cmp(&a.id));
    v
}

pub fn last_job_id(p: &Project) -> Option<String> {
    fs::read_to_string(p.state_dir().join("last-job")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Resolves an optional user-supplied id (or unique prefix) to a job id.
pub fn resolve_id(p: &Project, id: Option<&str>) -> Result<String> {
    match id {
        None => last_job_id(p).ok_or_else(|| ClientError::Other("no jobs yet".into())),
        Some(want) => {
            let matches: Vec<String> =
                local_jobs(p).into_iter().map(|j| j.id).filter(|i| i.starts_with(want)).collect();
            match matches.len() {
                1 => Ok(matches[0].clone()),
                0 => Ok(want.to_string()), // maybe only known remotely
                _ => Err(ClientError::Other(format!("job id {want:?} is ambiguous"))),
            }
        }
    }
}

/// Starts `command` on the remote (in the directory matching the local cwd).
pub fn submit(p: &Project, command: &str, rep: &dyn Reporter) -> Result<LocalJob> {
    let spec = JobSpec {
        cwd: p.rel_cwd.clone(),
        command: command.to_string(),
        prelude: p.config.exec.prelude.clone(),
        env: p.config.exec.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    };
    // Submission is not idempotent: retry only failures to *connect*, never a
    // request that may have reached the agent.
    let id = p.with_retry(rep, || {
        let mut s = p.connect()?;
        s.send(&Request::JobStart { spec: spec.clone() })?;
        match s.recv() {
            Ok(Response::JobStarted { id }) => {
                s.close();
                Ok(id)
            }
            Ok(other) => Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
            Err(e) => Err(ClientError::Other(format!(
                "connection lost while starting the command; it may or may not be running \
                 (check `devsync jobs`): {e}"
            ))),
        }
    })?;
    let job = LocalJob {
        id: id.clone(),
        command: command.to_string(),
        cwd: p.rel_cwd.clone(),
        host: p.host_label(),
        started_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        state: "running".into(),
        exit_code: None,
        offset: 0,
        pulled_back: false,
    };
    fs::create_dir_all(job_dir(p, &id))?;
    fs::write(job_dir(p, &id).join("command"), format!("{command}\n"))?;
    fs::write(output_path(p, &id), b"")?;
    save(p, &job)?;
    fs::write(p.state_dir().join("last-job"), &id)?;
    rep.event(Event::JobStarted { id, command: command.into(), cwd: p.rel_cwd.clone() });
    Ok(job)
}

/// Fetches new output for `job` within session `s`, appending it to the
/// local log (and `out`). Returns the number of new bytes.
pub fn poll_session(s: &mut Session, p: &Project, job: &mut LocalJob, mut out: Option<&mut dyn Write>) -> Result<u64> {
    let mut total = 0u64;
    loop {
        let resp = s.call(&Request::JobPoll { id: job.id.clone(), offset: job.offset, max_bytes: POLL_BYTES })?;
        let Response::JobOutput { state, data, next_offset } = resp else {
            return Err(ClientError::Other("unexpected agent reply to poll".into()));
        };
        if !data.is_empty() {
            let mut log = fs::OpenOptions::new().create(true).append(true).open(output_path(p, &job.id))?;
            log.write_all(&data)?;
            if let Some(o) = out.as_deref_mut() {
                o.write_all(&data)?;
                o.flush()?;
            }
        }
        total += data.len() as u64;
        job.offset = next_offset;
        job.apply(state);
        save(p, job)?;
        if data.len() < POLL_BYTES as usize || job.finished() || total >= SESSION_BYTES {
            return Ok(total);
        }
    }
}

fn state_label(j: &LocalJob) -> String {
    j.state.clone()
}

/// Collects output until the job ends, writing it to `out` as it arrives.
/// Each poll is a separate short session; disconnects only delay output.
pub fn follow(p: &Project, id: &str, out: &mut dyn Write, rep: &dyn Reporter) -> Result<LocalJob> {
    let mut job = load(p, id).ok_or_else(|| ClientError::Other(format!("unknown job {id}")))?;
    let base = Duration::from_millis(p.config.exec.poll_interval_ms.max(50));
    let mut interval = base;
    loop {
        let n = p.with_retry(rep, || {
            let mut s = p.connect()?;
            let n = poll_session(&mut s, p, &mut job, Some(&mut *out))?;
            s.close();
            Ok(n)
        })?;
        if job.finished() {
            rep.event(Event::JobFinished { id: job.id.clone(), state: state_label(&job), exit_code: job.exit_code });
            return Ok(job);
        }
        interval = if n > 0 { base } else { (interval * 3 / 2).min(base * 5) };
        crate::sleep_interruptible(interval)?;
    }
}

/// Prints already-fetched output for `id` to `out`.
pub fn print_local(p: &Project, id: &str, out: &mut dyn Write) -> Result<()> {
    if let Ok(b) = fs::read(output_path(p, id)) {
        out.write_all(&b)?;
    }
    Ok(())
}

pub fn kill(p: &Project, id: &str, rep: &dyn Reporter) -> Result<()> {
    p.with_retry(rep, || {
        let mut s = p.connect()?;
        s.call(&Request::JobKill { id: id.to_string() })?;
        s.close();
        Ok(())
    })
}

pub fn remote_list(p: &Project, rep: &dyn Reporter) -> Result<Vec<JobInfo>> {
    p.with_retry(rep, || {
        let mut s = p.connect()?;
        let r = s.call(&Request::JobList)?;
        s.close();
        match r {
            Response::Jobs(j) => Ok(j),
            other => Err(ClientError::Other(format!("unexpected agent reply: {other:?}"))),
        }
    })
}

/// One collection pass over all unfinished local jobs (single session):
/// fetch new output, and pull back files of jobs that just finished.
/// Returns the number of jobs still running.
pub fn collect(p: &Project, rep: &dyn Reporter) -> Result<usize> {
    let mut pending: Vec<LocalJob> =
        local_jobs(p).into_iter().filter(|j| !j.finished() || (!j.pulled_back && p.config.exec.pull_back)).collect();
    if pending.is_empty() {
        return Ok(0);
    }
    let mut s = p.connect()?;
    let mut running = 0;
    let mut to_pull = Vec::new();
    for j in pending.iter_mut() {
        if !j.finished() {
            let n = match poll_session(&mut s, p, j, None) {
                Ok(n) => n,
                Err(ClientError::Transport(devsync_transport::TransportError::Remote(m))) if m.contains("no such job") => {
                    j.state = "lost".into();
                    save(p, j)?;
                    0
                }
                Err(e) => return Err(e),
            };
            if n > 0 || j.finished() {
                rep.event(Event::JobUpdated { id: j.id.clone(), state: j.state.clone(), new_bytes: n });
            }
            if j.finished() {
                rep.event(Event::JobFinished { id: j.id.clone(), state: j.state.clone(), exit_code: j.exit_code });
            }
        }
        if j.finished() && !j.pulled_back {
            to_pull.push(j.clone());
        } else if !j.finished() {
            running += 1;
        }
    }
    s.close();
    for mut j in to_pull {
        if p.config.exec.pull_back && j.state == "exited" {
            crate::sync::pull_back(p, rep, &j.id)?;
        }
        j.pulled_back = true;
        save(p, &j)?;
    }
    Ok(running)
}
