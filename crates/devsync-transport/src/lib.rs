//! How the client reaches an agent.
//!
//! Every operation opens a fresh [`Session`]: a child process (`ssh host
//! devsync agent --stdio`, or the agent itself for the local endpoint) whose
//! stdio carries protocol frames. Sessions are closed as soon as the
//! operation completes; nothing relies on a connection staying up.

pub mod deploy;
pub mod endpoint;
pub mod retry;
pub mod session;

pub use endpoint::{Endpoint, SshConfig, Target};
pub use retry::{retry, RetryPolicy, Transient};
pub use session::{connect, ConnectOptions, RemoteInfo, Session};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Could not establish the connection (network down, host unreachable…).
    #[error("cannot connect: {0}")]
    Connect(String),
    /// The connection dropped mid-session.
    #[error("connection lost: {0}")]
    Disconnected(String),
    #[error("timed out waiting for the remote after {0:?}")]
    Timeout(std::time::Duration),
    /// The agent processed the request and reported an error.
    #[error("remote: {0}")]
    Remote(String),
    /// Authentication or configuration problem that retrying will not fix.
    #[error("{0}")]
    Fatal(String),
    #[error("agent deployment failed: {0}")]
    Deploy(String),
}

impl TransportError {
    /// Whether retrying the operation (with a new session) can help.
    pub fn is_transient(&self) -> bool {
        match self {
            TransportError::Connect(_) | TransportError::Disconnected(_) | TransportError::Timeout(_) => true,
            // Hash mismatches after an interrupted upload heal on retry.
            TransportError::Remote(m) => m.contains("will retry"),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, TransportError>;
