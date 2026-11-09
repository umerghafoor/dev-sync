//! Endpoint description and the commands used to reach it.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Set once a shared (multiplexed) connection misbehaved in this process;
/// later sessions then connect directly.
static MUX_DISABLED: AtomicBool = AtomicBool::new(false);

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
            control_persist_secs: 60,
            connect_timeout_secs: 10,
            ssh_bin: std::env::var("DEVSYNC_SSH").unwrap_or_else(|_| "ssh".into()),
        }
    }

    fn control_dir() -> PathBuf {
        // Unix socket paths are limited to ~104 bytes; macOS $TMPDIR is too long.
        // SAFETY: getuid has no failure modes.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/devsync-{uid}"))
    }

    /// Whether sessions currently go through a shared connection.
    pub fn mux_active(&self) -> bool {
        self.multiplex && !MUX_DISABLED.load(Ordering::SeqCst)
    }

    /// A shared connection can break silently (network change, dead TCP,
    /// server session limit): every session through it then fails with exit
    /// 255 and no message. Shut it down and connect directly from now on.
    pub fn drop_shared_connection(&self) {
        if !self.mux_active() {
            return;
        }
        MUX_DISABLED.store(true, Ordering::SeqCst);
        tracing::info!("shared ssh connection failed; closing it and connecting directly");
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

    /// Base ssh invocation with keepalives, timeouts and no interactive prompts.
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.ssh_bin);
        c.arg("-o").arg("BatchMode=yes");
        c.arg("-o").arg("ServerAliveInterval=5");
        c.arg("-o").arg("ServerAliveCountMax=3");
        c.arg("-o").arg(format!("ConnectTimeout={}", self.connect_timeout_secs));
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
    pub fn shell_command(&self, rel: &str) -> Command {
        match &self.endpoint {
            Endpoint::Ssh(ssh) => {
                let root = remote_path_expr(&self.root);
                let dir = if rel.is_empty() { root.clone() } else { format!("{root}/{}", sh_quote(rel)) };
                let mut c = ssh.command();
                c.arg("-t")
                    .arg(&ssh.host)
                    .arg(format!("cd {dir} 2>/dev/null || cd {root}; exec \"${{SHELL:-sh}}\" -l"));
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
