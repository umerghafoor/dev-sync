//! Filesystem layer shared by client and agent: which files belong to the
//! synced tree, their content hashes (cached), how two trees differ, and
//! crash-safe writes.

pub mod cache;
pub mod diff;
pub mod paths;
pub mod scan;
pub mod write;

pub use cache::HashCache;
pub use diff::{plan, Plan};
pub use paths::{safe_join, validate_rel, PathError};
pub use scan::{hash_file, scan, scan_one, DEFAULT_EXCLUDES};

/// Directory (relative to the project root) holding client-side state.
pub const CLIENT_STATE_DIR: &str = ".devsync";
/// Directory (relative to the remote project root) holding agent state.
pub const AGENT_STATE_DIR: &str = ".devsync-agent";

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("{path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("ignore rules: {0}")]
    Rules(#[from] ignore::Error),
    #[error(transparent)]
    Path(#[from] PathError),
}

pub(crate) fn io_err(path: impl AsRef<std::path::Path>) -> impl FnOnce(std::io::Error) -> FsError {
    let path = path.as_ref().display().to_string();
    move |source| FsError::Io { path, source }
}
