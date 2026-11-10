//! Endpoint description and the commands used to reach it.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// While set (and in the future), sessions connect directly instead of
/// through the shared connection, which failed even after being restarted.
static MUX_DISABLED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// How long to avoid the shared connection after it failed twice.
const MUX_COOLDOWN: Duration = Duration::from_secs(300);
const LEARNED_TIMEOUT_TTL: Duration = Duration::from_secs(24 * 3600);
const MAX_CONNECT_TIMEOUT: u32 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshConfig {
    /// `user@host`, `host`, or an alias from `~/.ssh/config`.
    pub host: String,
    pub port: Option<u16>,
    pub identity: Option<String>,
    /// Extra `-o Key=Value` options (passed as `-o <item>`).
    pub options: Vec<String>,
    /// Reuse one TCP connection for consecutive short sessions
    /// (OpenSSH ControlMaster). Sessions themselves stay short either way.
    pub multiplex: bool,
    /// How long an idle multiplexed connection is kept (seconds).
    pub control_persist_secs: u32,
    pub connect_timeout_secs: u32,
    /// ssh binary (overridable with `DEVSYNC_SSH`, used by tests).
    pub ssh_bin: String,
}

impl SshConfig {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: None,
            identity: None,
            options: Vec::new(),
            multiplex: true,
            control_persist_secs: 600,
            connect_timeout_secs: 30,
            ssh_bin: std::env::var("DEVSYNC_SSH").unwrap_or_else(|_| "ssh".into()),
        }
    }

    /// Private per-user directory for devsync's ssh sockets and locks.
    pub(crate) fn lock_dir(&self) -> PathBuf {
        Self::control_dir()
    }

    fn control_dir() -> PathBuf {
        // Unix socket paths are limited to ~104 bytes; macOS $TMPDIR is too long.
        // SAFETY: getuid has no failure modes.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/devsync-{uid}"))
    }

    /// Whether sessions currently go through a shared connection.
    pub fn mux_active(&self) -> bool {
        self.multiplex && MUX_DISABLED_UNTIL.lock().map(|g| g.is_none_or(|t| Instant::now() >= t)).unwrap_or(true)
    }

    /// Shuts down the shared connection (if any) so the next session starts
    /// a fresh one. A shared connection can break silently (network change,
    /// dead TCP, server session limit): every session through it then fails
    /// with exit 255 and no message.
    pub fn close_shared_connection(&self) {
        if !self.multiplex {
            return;
        }
        tracing::info!("shared ssh connection failed; closing it");
        let mut c = Command::new(&self.ssh_bin);
        c.arg("-o").arg(format!("ControlPath={}/%C", Self::control_dir().display()));
        if let Some(p) = self.port {
            c.arg("-p").arg(p.to_string());
        }
        for o in &self.options {
            c.arg("-o").arg(o);
        }
        c.arg("-O").arg("exit").arg(&self.host);
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Ok(mut child) = c.spawn() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Connect directly (no shared connection) for a few minutes.
    pub fn avoid_shared_connection(&self) {
        tracing::info!("shared ssh connection keeps failing; connecting directly for a while");
        if let Ok(mut g) = MUX_DISABLED_UNTIL.lock() {
            *g = Some(Instant::now() + MUX_COOLDOWN);
        }
    }

    fn connect_timeout_file(&self) -> PathBuf {
        let host: String = self.host.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        Self::control_dir().join(format!("connect-timeout-{host}"))
    }

    /// The configured connect timeout, raised if connecting to this host
    /// recently timed out (VPNs such as Tailscale can need many seconds to
    /// find a route after idling). Remembered for a day across processes.
    pub fn effective_connect_timeout(&self) -> u32 {
        let learned = std::fs::metadata(self.connect_timeout_file())
            .ok()
            .filter(|m| m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < LEARNED_TIMEOUT_TTL))
            .and_then(|_| std::fs::read_to_string(self.connect_timeout_file()).ok())
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);
        self.connect_timeout_secs.max(learned).max(1)
    }

    /// Records that connecting timed out: the next attempts wait twice as long
    /// (up to 2 minutes) instead of failing the same way again.
    pub fn note_connect_timeout(&self) {
        let next = (self.effective_connect_timeout() * 2).min(MAX_CONNECT_TIMEOUT);
        tracing::info!("connecting to {} timed out; allowing {next}s from now on", self.host);
        let dir = Self::control_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(self.connect_timeout_file(), next.to_string());
    }

    /// Base ssh invocation with keepalives, timeouts and no interactive prompts.
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.ssh_bin);
        c.arg("-o").arg("BatchMode=yes");
        c.arg("-o").arg("ServerAliveInterval=5");
        c.arg("-o").arg("ServerAliveCountMax=3");
        c.arg("-o").arg(format!("ConnectTimeout={}", self.effective_connect_timeout()));
        c.arg("-o").arg("LogLevel=ERROR");
        if let Some(p) = self.port {
            c.arg("-p").arg(p.to_string());
        }
        if let Some(i) = &self.identity {
            c.arg("-i").arg(i);
        }
        if self.mux_active() {
            let dir = Self::control_dir();
            if std::fs::create_dir_all(&dir).is_ok() {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                c.arg("-o").arg("ControlMaster=auto");
                c.arg("-o").arg(format!("ControlPath={}/%C", dir.display()));
                c.arg("-o").arg(format!("ControlPersist={}s", self.control_persist_secs));
            }
        }
        for o in &self.options {
            c.arg("-o").arg(o);
        }
        c
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Ssh(SshConfig),
    /// Agent runs on this machine (tests, VMs with shared folders, containers).
    Local,
}

/// Where the project lives on the remote side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub endpoint: Endpoint,
    /// Remote project root as written in the config (may start with `~`).
    pub root: String,
}

/// Quotes `s` for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+%".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Shell expression for a remote path, letting the remote shell expand `~`.
pub fn remote_path_expr(path: &str) -> String {
    if path == "~" {
        "~".into()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("~/{}", sh_quote(rest))
    } else {
        sh_quote(path)
    }
}

impl Target {
    /// Interactive login shell in `<root>/<rel>` (falls back to the root).
    /// `persistent`: run it in a tmux session (when the remote has tmux) that
    /// outlives the connection; reconnecting attaches to the same session.
    pub fn shell_command(&self, rel: &str, persistent: bool) -> Command {
        match &self.endpoint {
            Endpoint::Ssh(ssh) => {
                let root = remote_path_expr(&self.root);
                let dir = if rel.is_empty() { root.clone() } else { format!("{root}/{}", sh_quote(rel)) };
                let login = "exec \"${SHELL:-sh}\" -l";
                let start = if persistent {
                    // One session per project folder; status bar off and mouse
                    // scrolling on, so it feels like a plain terminal.
                    let name: String = format!("devsync-{}-{}", self.root, rel)
                        .chars()
                        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
                        .collect();
                    format!(
                        "if command -v tmux >/dev/null 2>&1; then exec tmux new-session -A -s {name} \\; \
                         set-option status off \\; set-option mouse on; else echo 'devsync: tmux is not \
                         installed on the remote; using a plain shell' >&2; {login}; fi"
                    )
                } else {
                    login.to_string()
                };
                let mut c = ssh.command();
                c.arg("-t").arg(&ssh.host).arg(format!("cd {dir} 2>/dev/null || cd {root}; {start}"));
                c
            }
            Endpoint::Local => {
                let mut c = Command::new(std::env::var("SHELL").unwrap_or_else(|_| "sh".into()));
                c.arg("-l");
                c
            }
        }
    }

    /// `ssh -N` forwarding the given remote ports to the same local ports.
    pub fn forward_command(&self, ports: &[u16]) -> Option<Command> {
        let Endpoint::Ssh(ssh) = &self.endpoint else { return None };
        let mut c = ssh.command();
        c.arg("-N").arg("-o").arg("ExitOnForwardFailure=yes");
        for p in ports {
            c.arg("-L").arg(format!("{p}:localhost:{p}"));
        }
        c.arg(&ssh.host);
        Some(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("abc/d.e"), "abc/d.e");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(remote_path_expr("~/my proj"), "~/'my proj'");
        assert_eq!(remote_path_expr("/srv/x"), "/srv/x");
    }
}
