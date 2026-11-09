//! Endpoint description and the commands used to reach it.

use std::path::PathBuf;
use std::process::Command;

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
        if self.multiplex {
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
                let dir = if rel.is_empty() {
                    root.clone()
                } else {
                    format!("{root}/{}", sh_quote(rel))
                };
                let mut c = ssh.command();
                c.arg("-t").arg(&ssh.host).arg(format!(
                    "cd {dir} 2>/dev/null || cd {root}; exec \"${{SHELL:-sh}}\" -l"
                ));
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
