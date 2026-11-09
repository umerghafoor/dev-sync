//! Walks a project tree applying devsync's ignore rules and produces a manifest.

use crate::{cache::HashCache, io_err, paths::to_rel, FsError};
use devsync_proto::{Entry, EntryKind, Hash, IgnoreSpec};
use ignore::{overrides::OverrideBuilder, WalkBuilder};
use std::fs::Metadata;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Always excluded. Dependency and VCS directories are rebuilt on the remote,
/// and devsync's own state never leaves its machine.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    crate::CLIENT_STATE_DIR,
    crate::AGENT_STATE_DIR,
    // the local connection settings; the remote has no use for them
    ".devsync.toml",
    "node_modules",
    "__pycache__",
    "*.pyc",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".DS_Store",
    ".idea",
    // devsync's own conflict copies and in-flight temp files
    "*.devsync-remote",
    "*.devsync-tmp",
    "*.devsync-dl",
    "*.devsync-ln",
];

/// Per-directory ignore file understood by devsync (gitignore syntax).
pub const IGNORE_FILE: &str = ".devsyncignore";

/// Normalises a selection path: `./src/` → `src`.
pub fn norm_rel(p: &str) -> String {
    let mut s = p.trim();
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    s.trim_start_matches('/').trim_end_matches('/').to_string()
}

/// Whether `rel` belongs to a sparse selection (`include`, already
/// normalised). Directories that merely *lead to* a selected path are
/// entered, but files in them are not part of the tree.
pub fn included(rel: &str, is_dir: bool, include: &[String]) -> bool {
    include.is_empty()
        || include.iter().any(|i| {
            i.is_empty()
                || rel == i
                || rel.starts_with(&format!("{i}/"))
                || (is_dir && i.starts_with(&format!("{rel}/")))
        })
}

/// Whether a file or directory name matches a built-in exclude.
pub fn builtin_excluded(name: &str) -> bool {
    DEFAULT_EXCLUDES.iter().any(|p| match p.strip_prefix('*') {
        Some(suffix) => name.ends_with(suffix),
        None => name == *p,
    })
}

fn walker(root: &Path, spec: &IgnoreSpec) -> Result<ignore::Walk, FsError> {
    let mut ob = OverrideBuilder::new(root);
    for p in DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).chain(spec.patterns.iter().cloned()) {
        let p = p.trim();
        if p.is_empty() || p.starts_with('#') {
            continue;
        }
        // In override syntax a leading `!` means "exclude".
        ob.add(&format!("!{}", p.trim_start_matches('!')))?;
    }
    let mut wb = WalkBuilder::new(root);
    wb.hidden(false)
        .parents(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(spec.use_gitignore)
        .git_exclude(spec.use_gitignore)
        .require_git(false)
        .follow_links(false)
        .add_custom_ignore_filename(IGNORE_FILE)
        .overrides(ob.build()?)
        .sort_by_file_path(|a, b| a.cmp(b));
    let include: Vec<String> = spec.include.iter().map(|p| norm_rel(p)).collect();
    if !include.is_empty() && !include.iter().any(|i| i.is_empty()) {
        // Prune unselected directories instead of walking them.
        let root = root.to_path_buf();
        wb.filter_entry(move |d| {
            if d.depth() == 0 {
                return true;
            }
            let Some(rel) = to_rel(&root, d.path()) else { return true };
            let is_dir = d.file_type().is_some_and(|t| t.is_dir());
            included(&rel, is_dir, &include)
        });
    }
    Ok(wb.build())
}

fn mtime_ns(md: &Metadata) -> i128 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i128).unwrap_or(0)
}

pub fn hash_file(path: &Path) -> std::io::Result<Hash> {
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(*h.finalize().as_bytes())
}

fn entry_for(path: &Path, rel: String, md: &Metadata, cache: &mut HashCache) -> Result<Option<Entry>, FsError> {
    let ft = md.file_type();
    let mtime = mtime_ns(md);
    if ft.is_symlink() {
        let target = std::fs::read_link(path).map_err(io_err(path))?;
        let target = target.to_string_lossy().into_owned();
        return Ok(Some(Entry {
            path: rel,
            kind: EntryKind::Symlink,
            size: target.len() as u64,
            mode: 0o777,
            mtime_ns: mtime,
            hash: *blake3::hash(target.as_bytes()).as_bytes(),
            link: Some(target),
        }));
    }
    if !ft.is_file() {
        return Ok(None); // sockets, fifos, devices are never synced
    }
    let size = md.len();
    let hash = match cache.get(&rel, size, mtime) {
        Some(h) => h,
        None => match hash_file(path) {
            Ok(h) => h,
            // File vanished between listing and hashing: just skip it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(path)(e)),
        },
    };
    cache.put(&rel, size, mtime, hash);
    Ok(Some(Entry {
        path: rel,
        kind: EntryKind::File,
        size,
        mode: md.permissions().mode() & 0o7777,
        mtime_ns: mtime,
        hash,
        link: None,
    }))
}

/// Scans `root` and returns the manifest, sorted by path. Unreadable entries
/// are logged and skipped rather than failing the whole scan.
pub fn scan(root: &Path, spec: &IgnoreSpec, cache: &mut HashCache) -> Result<Vec<Entry>, FsError> {
    let mut out = Vec::new();
    let mut last_checkpoint = std::time::Instant::now();
    for item in walker(root, spec)? {
        if last_checkpoint.elapsed() > std::time::Duration::from_secs(10) {
            cache.checkpoint();
            last_checkpoint = std::time::Instant::now();
        }
        let dent = match item {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("skipping: {e}");
                continue;
            }
        };
        if dent.depth() == 0 {
            continue;
        }
        let Some(ft) = dent.file_type() else { continue };
        if ft.is_dir() {
            continue;
        }
        let Some(rel) = to_rel(root, dent.path()) else { continue };
        let md = match std::fs::symlink_metadata(dent.path()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("skipping {rel}: {e}");
                continue;
            }
        };
        if let Some(e) = entry_for(dent.path(), rel, &md, cache)? {
            out.push(e);
        }
    }
    cache.save();
    Ok(out)
}

/// Builds the entry for a single path (no ignore-rule evaluation).
/// Returns `None` if it does not exist or is not a file/symlink.
pub fn scan_one(root: &Path, rel: &str, cache: &mut HashCache) -> Result<Option<Entry>, FsError> {
    let path = crate::safe_join(root, rel)?;
    match std::fs::symlink_metadata(&path) {
        Ok(md) => entry_for(&path, rel.to_string(), &md, cache),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(&path)(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.path.as_str()).collect()
    }

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        for (p, c) in [
            ("src/main.py", "print(1)"),
            (".env", "SECRET=1"),
            ("build/out.bin", "x"),
            ("node_modules/x/index.js", "x"),
            (".git/HEAD", "ref"),
            ("pkg/__pycache__/m.pyc", "x"),
            ("data/big.csv", "1,2"),
            ("logs/a.log", "x"),
            ("src/main.py.devsync-remote", "conflict copy"),
            (".devsync.toml", "[remote]"),
        ] {
            fs::create_dir_all(r.join(p).parent().unwrap()).unwrap();
            fs::write(r.join(p), c).unwrap();
        }
        fs::write(r.join(".gitignore"), ".env\nbuild/\n").unwrap();
        fs::write(r.join(IGNORE_FILE), "logs/\n").unwrap();
        d
    }

    #[test]
    fn default_rules_keep_gitignored_files() {
        let d = tree();
        let m = scan(d.path(), &IgnoreSpec::default(), &mut HashCache::ephemeral()).unwrap();
        assert_eq!(
            names(&m),
            vec![".devsyncignore", ".env", ".gitignore", "build/out.bin", "data/big.csv", "src/main.py"]
        );
    }

    #[test]
    fn gitignore_and_patterns_when_enabled() {
        let d = tree();
        let spec =
            IgnoreSpec { use_gitignore: true, patterns: vec!["data/".into(), "# comment".into()], include: vec![] };
        let m = scan(d.path(), &spec, &mut HashCache::ephemeral()).unwrap();
        assert_eq!(names(&m), vec![".devsyncignore", ".gitignore", "src/main.py"]);
    }

    #[test]
    fn symlinks_and_exec_bit() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("run.sh"), "#!/bin/sh").unwrap();
        fs::set_permissions(d.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("run.sh", d.path().join("link")).unwrap();
        let m = scan(d.path(), &IgnoreSpec::default(), &mut HashCache::ephemeral()).unwrap();
        assert_eq!(m[0].kind, EntryKind::Symlink);
        assert_eq!(m[0].link.as_deref(), Some("run.sh"));
        assert_eq!(m[1].mode & 0o111, 0o111);
    }

    #[test]
    fn include_selects_paths_and_prunes_the_rest() {
        let d = tree();
        let spec = IgnoreSpec { include: vec!["./src/".into(), "data/big.csv".into()], ..Default::default() };
        let m = scan(d.path(), &spec, &mut HashCache::ephemeral()).unwrap();
        assert_eq!(names(&m), vec!["data/big.csv", "src/main.py"]);
        assert!(included("data", true, &["data/big.csv".to_string()]));
        assert!(!included("data/other.csv", false, &["data/big.csv".to_string()]));
        assert!(builtin_excluded("node_modules") && builtin_excluded("x.pyc") && !builtin_excluded("src"));
    }

    #[test]
    fn checkpoint_keeps_progress() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("cache.bin");
        let mut c = HashCache::load(&file);
        c.put("a", 1, 1, [1; 32]);
        c.checkpoint();
        // An interrupted scan never calls save(); the checkpoint survives.
        drop(c);
        let c = HashCache::load(&file);
        assert_eq!(c.get("a", 1, 1), Some([1; 32]));
    }

    #[test]
    fn cache_detects_content_change() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.txt");
        fs::write(&f, "one").unwrap();
        let cache_file = d.path().join(".devsync/cache.bin");
        let h1 = scan(d.path(), &IgnoreSpec::default(), &mut HashCache::load(&cache_file)).unwrap()[0].hash;
        fs::write(&f, "two").unwrap(); // same size, possibly same mtime second
        let h2 = scan(d.path(), &IgnoreSpec::default(), &mut HashCache::load(&cache_file)).unwrap()[0].hash;
        assert_ne!(h1, h2);
        assert!(cache_file.exists());
    }
}
