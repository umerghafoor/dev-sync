//! A short-lived connection to an agent.

use crate::{deploy, Endpoint, SshConfig, Target, TransportError};
use devsync_proto::{self as proto, FrameError, Request, Response, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Max time to wait for any single response.
    pub timeout: Duration,
    /// Where to remember the remote's platform and deployed agent name.
    pub cache_file: Option<PathBuf>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(120), cache_file: None }
    }
}

/// What the agent reported at handshake.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteInfo {
    pub os: String,
    pub arch: String,
    pub root: String,
    pub agent_version: String,
    pub agent_name: Option<String>,
    /// Protocol of the cached agent; a cache from another protocol is ignored.
    #[serde(default)]
    pub protocol: u32,
}

pub struct Session {
    child: Child,
    writer: Option<BufWriter<ChildStdin>>,
    rx: Receiver<Result<Vec<u8>, FrameError>>,
    greeting: Receiver<Result<Vec<u8>, FrameError>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    timeout: Duration,
    pub info: RemoteInfo,
}

enum Handshake {
    AgentMissing,
    Failed(TransportError),
}

fn stderr_tail(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    let b = buf.lock().map(|b| b.clone()).unwrap_or_default();
    String::from_utf8_lossy(&b).trim().to_string()
}

/// Classifies an ssh failure from its exit code and stderr.
pub(crate) fn classify_ssh_failure(code: Option<i32>, stderr: &str) -> TransportError {
    let s = stderr.to_ascii_lowercase();
    if s.contains("permission denied") || s.contains("host key verification failed") || s.contains("bad configuration")
    {
        return TransportError::Fatal(format!(
            "ssh refused: {}. devsync needs non-interactive (key-based) ssh login; check `ssh {}`",
            stderr.lines().last().unwrap_or(stderr),
            "<host>"
        ));
    }
    let msg = if stderr.is_empty() {
        match code {
            Some(c) => format!("ssh failed with exit code {c} and no error message (network down?)"),
            None => "ssh was terminated".to_string(),
        }
    } else {
        stderr.to_string()
    };
    TransportError::Connect(msg)
}

impl Session {
    fn spawn(mut cmd: Command, timeout: Duration) -> Result<Self, TransportError> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Own process group: Ctrl-C in the terminal must not kill in-flight
        // sessions (the client handles interruption itself).
        cmd.process_group(0);
        let mut child =
            cmd.spawn().map_err(|e| TransportError::Fatal(format!("cannot run {:?}: {e}", cmd.get_program())))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let mut err_pipe = child.stderr.take().expect("piped stderr");

        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err_pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut s) = sink.lock() {
                    s.extend_from_slice(&buf[..n]);
                    let excess = s.len().saturating_sub(16 * 1024);
                    s.drain(..excess);
                }
            }
        });

        let (tx, rx) = mpsc::channel();
        let (greet_tx, greeting) = mpsc::channel();
        std::thread::spawn(move || {
            let mut r = BufReader::with_capacity(256 * 1024, stdout);
            // Skip anything the remote shell printed before the agent started.
            let greeted = proto::read_greeting(&mut r);
            let ok = greeted.is_ok();
            let _ = greet_tx.send(greeted);
            if !ok {
                return;
            }
            loop {
                let item = proto::read_frame(&mut r);
                let stop = item.is_err();
                if tx.send(item).is_err() || stop {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            writer: Some(BufWriter::with_capacity(256 * 1024, stdin)),
            rx,
            greeting,
            stderr,
            timeout,
            info: RemoteInfo::default(),
        })
    }

    fn exit_code_soon(&mut self) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(st)) => return st.code(),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return None,
            }
        }
        None
    }

    fn lost(&mut self, what: &str) -> TransportError {
        let code = self.exit_code_soon();
        let err = stderr_tail(&self.stderr);
        match code {
            Some(255) => classify_ssh_failure(code, &err),
            _ => TransportError::Disconnected(if err.is_empty() { what.to_string() } else { format!("{what}: {err}") }),
        }
    }

    /// Waits for the agent's greeting before sending anything, so the remote
    /// shell's startup files cannot swallow or corrupt protocol data.
    fn await_greeting(&mut self) -> Result<(), Handshake> {
        let wait = self.timeout.min(Duration::from_secs(60));
        match self.greeting.recv_timeout(wait) {
            Ok(Ok(noise)) => {
                if !noise.is_empty() {
                    let text = String::from_utf8_lossy(&noise);
                    tracing::warn!(
                        "remote shell printed {} byte(s) before devsync started (ignored): {:?}",
                        noise.len(),
                        text.chars().take(200).collect::<String>()
                    );
                }
                Ok(())
            }
            Ok(Err(FrameError::TooLarge(n))) => {
                let _ = self.child.kill();
                Err(Handshake::Failed(TransportError::Fatal(format!(
                    "the remote printed {n} bytes without starting the devsync agent; check that your \
                     shell startup files (~/.bashrc, ~/.profile) don't run programs in non-interactive shells"
                ))))
            }
            Ok(Err(e)) => Err(self.diagnose(TransportError::Disconnected(e.to_string()))),
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                Err(Handshake::Failed(TransportError::Timeout(wait)))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(self.diagnose(TransportError::Disconnected("connection closed".into())))
            }
        }
    }

    fn handshake(mut self) -> Result<Self, Handshake> {
        self.await_greeting()?;
        if let Err(e) = self.send(&Request::Hello { version: PROTOCOL_VERSION }) {
            return Err(self.diagnose(e));
        }
        match self.recv() {
            Ok(Response::Hello { agent_version, os, arch, root, .. }) => {
                self.info = RemoteInfo { os, arch, root, agent_version, agent_name: None, protocol: PROTOCOL_VERSION };
                Ok(self)
            }
            Ok(other) => {
                Err(Handshake::Failed(TransportError::Fatal(format!("unexpected handshake reply: {other:?}"))))
            }
            Err(TransportError::Remote(m)) if m.contains("protocol mismatch") => Err(Handshake::AgentMissing),
            Err(e) => Err(self.diagnose(e)),
        }
    }

    fn diagnose(&mut self, e: TransportError) -> Handshake {
        let code = self.exit_code_soon();
        let err = stderr_tail(&self.stderr);
        let low = err.to_ascii_lowercase();
        if code == Some(127) || code == Some(126) || low.contains("no such file") || low.contains("not found") {
            return Handshake::AgentMissing;
        }
        if code == Some(255) {
            return Handshake::Failed(classify_ssh_failure(code, &err));
        }
        Handshake::Failed(match e {
            TransportError::Disconnected(_) if !err.is_empty() => TransportError::Connect(err),
            other => other,
        })
    }

    pub fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
    }

    pub fn send(&mut self, req: &Request) -> Result<(), TransportError> {
        let w = self.writer.as_mut().ok_or_else(|| TransportError::Disconnected("session closed".into()))?;
        let res = proto::send(w, req).and_then(|_| w.flush().map_err(FrameError::from));
        res.map_err(|e| self.lost(&e.to_string()))
    }

    pub fn recv(&mut self) -> Result<Response, TransportError> {
        match self.rx.recv_timeout(self.timeout) {
            Ok(Ok(buf)) => match proto::decode::<Response>(&buf) {
                Ok(Response::Error { message }) => Err(TransportError::Remote(message)),
                Ok(r) => Ok(r),
                Err(e) => Err(TransportError::Fatal(format!("bad response from agent: {e}"))),
            },
            Ok(Err(e)) => Err(self.lost(&e.to_string())),
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                Err(TransportError::Timeout(self.timeout))
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.lost("connection closed")),
        }
    }

    /// Sends a request and waits for its response.
    pub fn call(&mut self, req: &Request) -> Result<Response, TransportError> {
        self.send(req)?;
        self.recv()
    }

    /// Closes stdin so the agent exits, then reaps the process.
    pub fn close(mut self) {
        self.writer.take();
        if self.exit_code_soon().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.writer.take();
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn local_agent_command(root: &str) -> Command {
    let exe = std::env::var_os("DEVSYNC_AGENT_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("devsync"));
    let mut c = Command::new(exe);
    c.arg("agent").arg("--stdio").arg("--root").arg(root);
    c
}

fn ssh_agent_command(ssh: &SshConfig, agent_name: &str, root: &str) -> Command {
    let mut c = ssh.command();
    c.arg("-T").arg(&ssh.host).arg(format!(
        "exec ~/{}/{} agent --stdio --root {}",
        deploy::REMOTE_BIN_DIR,
        agent_name,
        crate::endpoint::sh_quote(root)
    ));
    c
}

fn load_cache(path: Option<&Path>) -> Option<RemoteInfo> {
    let info: RemoteInfo = serde_json::from_slice(&std::fs::read(path?).ok()?).ok()?;
    // An agent from another protocol version must be replaced, not tried.
    (info.protocol == PROTOCOL_VERSION).then_some(info)
}

fn save_cache(path: Option<&Path>, info: &RemoteInfo) {
    if let (Some(p), Ok(bytes)) = (path, serde_json::to_vec_pretty(info)) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, bytes);
    }
}

/// Opens a session, deploying the agent to the remote first if needed.
pub fn connect(target: &Target, opts: &ConnectOptions) -> Result<Session, TransportError> {
    match &target.endpoint {
        Endpoint::Local => {
            let s = Session::spawn(local_agent_command(&target.root), opts.timeout)?;
            match s.handshake() {
                Ok(s) => Ok(s),
                Err(Handshake::AgentMissing) => Err(TransportError::Fatal("local agent failed to start".into())),
                Err(Handshake::Failed(e)) => Err(e),
            }
        }
        Endpoint::Ssh(ssh) => {
            let cache = opts.cache_file.as_deref();
            if let Some(name) = load_cache(cache).and_then(|c| c.agent_name) {
                let s = Session::spawn(ssh_agent_command(ssh, &name, &target.root), opts.timeout)?;
                match s.handshake() {
                    Ok(mut s) => {
                        s.info.agent_name = Some(name);
                        return Ok(s);
                    }
                    Err(Handshake::Failed(e)) => return Err(e),
                    Err(Handshake::AgentMissing) => {
                        tracing::info!("cached agent {name} missing on remote; redeploying")
                    }
                }
            }
            let name = deploy::ensure_agent(ssh)?;
            let s = Session::spawn(ssh_agent_command(ssh, &name, &target.root), opts.timeout)?;
            match s.handshake() {
                Ok(mut s) => {
                    s.info.agent_name = Some(name);
                    save_cache(cache, &s.info);
                    Ok(s)
                }
                Err(Handshake::AgentMissing) => Err(TransportError::Deploy(
                    "agent was uploaded but cannot be executed on the remote (noexec home? wrong architecture?)".into(),
                )),
                Err(Handshake::Failed(e)) => Err(e),
            }
        }
    }
}
