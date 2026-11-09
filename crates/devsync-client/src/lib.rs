//! Client-side engine. The CLI and the VS Code extension (via the CLI's
//! `--json` mode) are thin layers over this crate.

pub mod config;
pub mod events;
pub mod integrations;
pub mod jobs;
pub mod project;
pub mod route;
pub mod selection;
pub mod sync;

pub use config::Config;
pub use events::{Direction, Event, Reporter, Stage, Transfer};
pub use project::Project;

use devsync_transport::{Transient, TransportError};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Fs(#[from] devsync_fs::FsError),
    #[error("config: {0}")]
    Config(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Refused for safety; needs explicit confirmation (e.g. `--force`).
    #[error("{0}")]
    Refused(String),
    /// The remote project is too large to sync whole; the user must choose
    /// what to sync first (`devsync tree`, `devsync unselect`, …).
    #[error("{0}")]
    NeedsSelection(String),
    #[error("interrupted")]
    Interrupted,
    #[error("{0}")]
    Other(String),
}

impl Transient for ClientError {
    fn is_transient(&self) -> bool {
        matches!(self, ClientError::Transport(e) if e.is_transient())
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// Set by the CLI's Ctrl-C handler; long-running loops check it.
pub static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn interrupted() -> bool {
    INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Sleeps in small steps so an interrupt is noticed promptly.
pub fn sleep_interruptible(d: std::time::Duration) -> Result<()> {
    let end = std::time::Instant::now() + d;
    while std::time::Instant::now() < end {
        if interrupted() {
            return Err(ClientError::Interrupted);
        }
        std::thread::sleep(std::time::Duration::from_millis(50).min(end - std::time::Instant::now()));
    }
    Ok(())
}
