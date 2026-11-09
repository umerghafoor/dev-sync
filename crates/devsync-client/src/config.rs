//! Project configuration (`.devsync.toml` at the project root).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const CONFIG_FILE: &str = ".devsync.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub remote: RemoteConfig,
    pub ssh: SshSection,
    pub sync: SyncConfig,
    pub exec: ExecConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteConfig {
    /// `user@host`, an ssh alias, or `local` (agent on this machine).
    pub host: String,
    /// Project directory on the remote (`~` allowed).
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SshSection {
    pub port: Option<u16>,
    pub identity: Option<String>,
    /// Extra ssh `-o` options, e.g. `["ProxyJump=bastion"]`.
    pub options: Vec<String>,
    /// Reuse one TCP connection across the short sessions devsync opens.
    pub multiplex: bool,
    pub control_persist_secs: u32,
    pub connect_timeout_secs: u32,
    /// Seconds to wait for any single agent response.
    pub response_timeout_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    /// Also skip files matched by .gitignore (off: .env etc. are synced).
    pub use_gitignore: bool,
    /// Extra exclude patterns (gitignore syntax).
    pub ignore: Vec<String>,
    /// Remove remote files that were deleted locally.
    pub delete: bool,
    /// Refuse to delete more than this many files without `--force`.
    pub max_delete: usize,
    /// Attempts for network operations (0 = until it works).
    pub retries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecConfig {
    /// Bring back files that remote commands create or change.
    pub pull_back: bool,
    /// Shell snippet run before every remote command.
    pub prelude: String,
    pub env: BTreeMap<String, String>,
    /// Ports `devsync forward` exposes on localhost.
    pub ports: Vec<u16>,
    /// Commands the agent hook should keep local / always send remote,
    /// in addition to the built-in lists.
    pub local_commands: Vec<String>,
    pub remote_commands: Vec<String>,
    /// Delay between output polls while a job runs (adaptive up to 4x).
    pub poll_interval_ms: u64,
}

impl Default for SshSection {
    fn default() -> Self {
        Self {
            port: None,
            identity: None,
            options: Vec::new(),
            multiplex: true,
            control_persist_secs: 60,
            connect_timeout_secs: 10,
            response_timeout_secs: 120,
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self { use_gitignore: false, ignore: Vec::new(), delete: false, max_delete: 200, retries: 0 }
    }
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            pull_back: true,
            prelude: String::new(),
            env: BTreeMap::new(),
            ports: Vec::new(),
            local_commands: Vec::new(),
            remote_commands: Vec::new(),
            poll_interval_ms: 400,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.validate().map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.remote.host.trim().is_empty() {
            return Err("remote.host is required".into());
        }
        if self.remote.path.trim().is_empty() {
            return Err("remote.path is required".into());
        }
        if self.remote.host.starts_with('-') {
            return Err("remote.host must not start with '-'".into());
        }
        Ok(())
    }

    /// Commented template written by `devsync init`.
    pub fn template(host: &str, path: &str) -> String {
        format!(
            r#"# devsync project configuration (reference: docs/CONFIG.md)

[remote]
host = {host:?}          # user@host, ~/.ssh/config alias, or "local"
path = {path:?}          # project directory on the remote ("~" = remote home)

[ssh]
# port = 22
# identity = "~/.ssh/id_ed25519"
# options = ["ProxyJump=bastion"]
multiplex = true             # reuse one TCP connection for devsync's short sessions
control_persist_secs = 60    # idle time before that connection is closed
connect_timeout_secs = 10
response_timeout_secs = 120

[sync]
use_gitignore = false        # false: .gitignore'd files (.env, data, configs) are synced too
ignore = []                  # extra excludes, e.g. ["dist/", "*.log"]; also see .devsyncignore
delete = false              # remove remote files deleted locally
max_delete = 200             # safety cap for deletions without --force
retries = 0                  # 0 = keep retrying until the network is back

[exec]
pull_back = true             # copy files changed by remote commands back here
prelude = ""                 # e.g. "source ~/.nvm/nvm.sh" or "conda activate myenv"
ports = []                   # forwarded by `devsync forward`, e.g. [3000, 8000]
local_commands = []          # extra commands the agent hook keeps local
remote_commands = []         # extra commands the agent hook always runs remotely
poll_interval_ms = 400

[exec.env]
# PYTHONUNBUFFERED = "1"
"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_and_validates() {
        let cfg: Config = toml::from_str(&Config::template("me@box", "~/proj")).unwrap();
        assert_eq!(cfg.remote.host, "me@box");
        assert_eq!(cfg.remote.path, "~/proj");
        assert!(cfg.ssh.multiplex);
        assert!(!cfg.sync.use_gitignore);
        cfg.validate().unwrap();
    }

    #[test]
    fn rejects_unknown_keys_and_missing_host() {
        assert!(toml::from_str::<Config>("[remote]\nhots = \"x\"").is_err());
        let cfg: Config = toml::from_str("[remote]\npath = \"/x\"").unwrap();
        assert!(cfg.validate().is_err());
    }
}
