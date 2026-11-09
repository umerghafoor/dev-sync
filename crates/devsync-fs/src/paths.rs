//! Validation of relative paths received over the wire. Anything that could
//! escape the project root, or touch agent state, is rejected.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    #[error("empty path")]
    Empty,
    #[error("path escapes project root: {0}")]
    Escapes(String),
    #[error("path targets devsync internal state: {0}")]
    Reserved(String),
}

/// Ensures `rel` is a plain relative path (no root, no `..`, no `.`).
pub fn validate_rel(rel: &str) -> Result<(), PathError> {
    if rel.is_empty() {
        return Err(PathError::Empty);
    }
    if rel.contains('\0') || rel.contains('\\') {
        return Err(PathError::Escapes(rel.to_string()));
    }
    let mut first = true;
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(name) => {
                if first && (name == crate::AGENT_STATE_DIR || name == crate::CLIENT_STATE_DIR) {
                    return Err(PathError::Reserved(rel.to_string()));
                }
                first = false;
            }
            _ => return Err(PathError::Escapes(rel.to_string())),
        }
    }
    Ok(())
}

/// Joins a validated relative path onto `root`.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, PathError> {
    validate_rel(rel)?;
    Ok(root.join(rel))
}

/// Converts a path under `root` into the canonical `/`-separated relative form.
pub fn to_rel(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_paths() {
        for p in ["a", "a/b.txt", ".env", "src/.hidden/x", "a..b"] {
            assert!(validate_rel(p).is_ok(), "{p}");
        }
    }

    #[test]
    fn rejects_escapes_and_reserved() {
        for p in ["", "/etc/passwd", "../x", "a/../../x", "./a", "a\\b", ".devsync-agent/x", ".devsync/cache"] {
            assert!(validate_rel(p).is_err(), "{p}");
        }
        // Reserved names are only reserved at the root.
        assert!(validate_rel("sub/.devsync/x").is_ok());
    }
}
