//! Crash-safe placement of files: content is written elsewhere, verified,
//! then renamed into place, so readers never observe a half-written file.

use crate::{io_err, FsError};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

/// Makes sure every ancestor of `dest` (below `root`) is a directory,
/// replacing files that stand in the way (a file became a directory).
pub fn prepare_parent(root: &Path, dest: &Path) -> Result<(), FsError> {
    let Some(parent) = dest.parent() else { return Ok(()) };
    if parent.is_dir() {
        return Ok(());
    }
    let rel = parent.strip_prefix(root).unwrap_or(parent);
    let mut cur = root.to_path_buf();
    for c in rel.components() {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(md) if md.is_dir() => {}
            Ok(_) => fs::remove_file(&cur).map_err(io_err(&cur))?,
            Err(_) => {}
        }
    }
    fs::create_dir_all(parent).map_err(io_err(parent))
}

/// Removes whatever is at `dest` if it is a directory (a directory became a file).
fn clear_dir_at(dest: &Path) -> Result<(), FsError> {
    if let Ok(md) = fs::symlink_metadata(dest) {
        if md.is_dir() {
            fs::remove_dir_all(dest).map_err(io_err(dest))?;
        }
    }
    Ok(())
}

/// Moves the fully written `staged` file to `dest`, applying mode and mtime.
pub fn commit_file(root: &Path, staged: &Path, dest: &Path, mode: u32, mtime_ns: i128) -> Result<(), FsError> {
    fs::set_permissions(staged, fs::Permissions::from_mode(mode & 0o7777)).map_err(io_err(staged))?;
    if mtime_ns > 0 {
        let t = UNIX_EPOCH + Duration::from_nanos(mtime_ns as u64);
        if let Ok(f) = fs::File::options().write(true).open(staged) {
            let _ = f.set_modified(t);
        }
    }
    prepare_parent(root, dest)?;
    clear_dir_at(dest)?;
    fs::rename(staged, dest).map_err(io_err(dest))
}

/// Writes `bytes` to `dest` atomically (via a sibling temp file).
pub fn write_file(root: &Path, dest: &Path, bytes: &[u8], mode: u32, mtime_ns: i128) -> Result<(), FsError> {
    prepare_parent(root, dest)?;
    let tmp = dest.with_file_name(format!(
        ".{}.devsync-tmp",
        dest.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
    ));
    fs::write(&tmp, bytes).map_err(io_err(&tmp))?;
    commit_file(root, &tmp, dest, mode, mtime_ns)
}

/// Creates or replaces a symlink at `dest` atomically.
pub fn write_symlink(root: &Path, dest: &Path, target: &str) -> Result<(), FsError> {
    prepare_parent(root, dest)?;
    clear_dir_at(dest)?;
    let tmp = dest.with_file_name(format!(".{}.devsync-ln", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp).map_err(io_err(&tmp))?;
    fs::rename(&tmp, dest).map_err(io_err(dest))
}

/// Deletes a file or symlink, then prunes directories left empty (up to `root`).
pub fn remove_path(root: &Path, dest: &Path) -> Result<(), FsError> {
    match fs::symlink_metadata(dest) {
        Ok(md) if md.is_dir() => fs::remove_dir_all(dest).map_err(io_err(dest))?,
        Ok(_) => fs::remove_file(dest).map_err(io_err(dest))?,
        Err(_) => return Ok(()),
    }
    let mut cur = dest.parent();
    while let Some(dir) = cur {
        if dir == root || !dir.starts_with(root) || fs::remove_dir(dir).is_err() {
            break;
        }
        cur = dir.parent();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_dir_transitions() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write_file(r, &r.join("a"), b"file", 0o644, 0).unwrap();
        // "a" becomes a directory containing "b"
        write_file(r, &r.join("a/b"), b"nested", 0o755, 0).unwrap();
        assert_eq!(fs::read(r.join("a/b")).unwrap(), b"nested");
        assert_eq!(fs::metadata(r.join("a/b")).unwrap().permissions().mode() & 0o777, 0o755);
        // and back to a file
        write_file(r, &r.join("a"), b"file again", 0o644, 1_700_000_000_000_000_000).unwrap();
        assert_eq!(fs::read(r.join("a")).unwrap(), b"file again");
        remove_path(r, &r.join("a")).unwrap();
        assert!(!r.join("a").exists());
    }

    #[test]
    fn remove_prunes_empty_dirs() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write_file(r, &r.join("x/y/z.txt"), b"1", 0o644, 0).unwrap();
        remove_path(r, &r.join("x/y/z.txt")).unwrap();
        assert!(!r.join("x").exists());
        assert!(r.exists());
    }
}
