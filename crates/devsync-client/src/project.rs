//! A devsync project: its root, config, local state and remote target.

use crate::config::{Config, CONFIG_FILE};
use crate::events::{Event, Reporter};
use crate::{ClientError, Result};
use devsync_proto::IgnoreSpec;
use devsync_transport::{connect, ConnectOptions, Endpoint, RetryPolicy, Session, SshConfig, Target};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Project {
    pub root: PathBuf,
    pub config: Config,
    /// Invocation directory relative to the root ("" at the root).
    pub rel_cwd: String,
}

/// Finds the directory containing `.devsync.toml`, walking up from `start`.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|d| d.join(CONFIG_FILE).is_file()).map(Path::to_path_buf)
}

impl Project {
    pub fn discover(start: &Path) -> Result<Self> {
        let start = start.canonicalize()?;
        let root = find_root(&start).ok_or_else(|| {
            ClientError::Config(format!(
                "no {CONFIG_FILE} found in {} or any parent. Run `devsync init user@host:~/path` first",
                start.display()
            ))
        })?;
        let config = Config::load(&root.join(CONFIG_FILE)).map_err(ClientError::Config)?;
        let rel_cwd = devsync_fs::paths::to_rel(&root, &start).unwrap_or_default();
        Ok(Self { root, config, rel_cwd })
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join(devsync_fs::CLIENT_STATE_DIR)
    }

    pub fn ignore_spec(&self) -> IgnoreSpec {
        IgnoreSpec { use_gitignore: self.config.sync.use_gitignore, patterns: self.config.sync.ignore.clone() }
    }

    pub fn target(&self) -> Target {
        let r = &self.config.remote;
        let endpoint = if r.host == "local" {
            Endpoint::Local
        } else {
            let s = &self.config.ssh;
            let mut ssh = SshConfig::new(&r.host);
            ssh.port = s.port;
            ssh.identity = s.identity.clone();
            ssh.options = s.options.clone();
            ssh.multiplex = s.multiplex;
            ssh.control_persist_secs = s.control_persist_secs;
            ssh.connect_timeout_secs = s.connect_timeout_secs;
            Endpoint::Ssh(ssh)
        };
        Target { endpoint, root: r.path.clone() }
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy { max_attempts: self.config.sync.retries, ..RetryPolicy::default() }
    }

    /// Opens a short-lived session to the agent.
    pub fn connect(&self) -> Result<Session> {
        let opts = ConnectOptions {
            timeout: Duration::from_secs(self.config.ssh.response_timeout_secs.max(5) as u64),
            cache_file: Some(self.state_dir().join("remote.json")),
        };
        Ok(connect(&self.target(), &opts)?)
    }

    /// Runs `op` with the project's retry policy, reporting retries.
    pub fn with_retry<T>(&self, rep: &dyn Reporter, op: impl FnMut() -> Result<T>) -> Result<T> {
        self.with_retry_policy(self.retry_policy(), rep, op)
    }

    pub fn with_retry_policy<T>(
        &self,
        policy: RetryPolicy,
        rep: &dyn Reporter,
        mut op: impl FnMut() -> Result<T>,
    ) -> Result<T> {
        devsync_transport::retry(
            &policy,
            |attempt, e: &ClientError, delay| {
                rep.event(Event::Retrying { attempt, error: e.to_string(), delay_ms: delay.as_millis() as u64 })
            },
            || {
                if crate::interrupted() {
                    return Err(ClientError::Interrupted);
                }
                op()
            },
        )
    }

    pub fn host_label(&self) -> String {
        self.config.remote.host.clone()
    }
}
