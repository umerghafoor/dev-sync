//! Choosing which parts of a project are synced ("sparse sync").
//!
//! The selection lives in `.devsync.toml`:
//! - `sync.include`: if non-empty, only these paths are synced;
//! - `sync.ignore`: anchored entries like `/data/` exclude a path inside an
//!   included one.
//!
//! Unselected paths are skipped without being scanned, on both sides.
//! Edits keep the rest of the file (comments, formatting) intact.

use crate::config::{Config, SyncConfig, CONFIG_FILE};
use crate::{ClientError, Result};
use devsync_fs::norm_rel;
use std::path::Path;
use toml_edit::{Array, DocumentMut, Item, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Synced.
    Selected,
    /// Not synced itself, but something inside it is (or the reverse).
    Partial,
    NotSelected,
}

fn is_glob(p: &str) -> bool {
    p.contains(['*', '?', '['])
}

/// Plain-path ignore entries, normalised (`/data/` → `data`).
fn ignored_paths(cfg: &SyncConfig) -> Vec<String> {
    cfg.ignore.iter().filter(|p| !is_glob(p) && !p.trim().is_empty()).map(|p| norm_rel(p)).collect()
}

fn covers(base: &str, rel: &str) -> bool {
    base.is_empty() || rel == base || rel.starts_with(&format!("{base}/"))
}

pub fn state(cfg: &SyncConfig, rel: &str) -> State {
    let rel = norm_rel(rel);
    let include: Vec<String> = cfg.include.iter().map(|p| norm_rel(p)).collect();
    let ignored = ignored_paths(cfg);
    let in_include = include.is_empty() || include.iter().any(|i| covers(i, &rel));
    let is_ignored = ignored.iter().any(|g| covers(g, &rel));
    if in_include && !is_ignored {
        if ignored.iter().any(|g| covers(&rel, g) && g != &rel) {
            State::Partial
        } else {
            State::Selected
        }
    } else if !is_ignored && include.iter().any(|i| covers(&rel, i)) {
        State::Partial
    } else {
        State::NotSelected
    }
}

fn load_doc(root: &Path) -> Result<DocumentMut> {
    let text = std::fs::read_to_string(root.join(CONFIG_FILE))?;
    text.parse::<DocumentMut>().map_err(|e| ClientError::Config(e.to_string()))
}

fn strings(doc: &DocumentMut, key: &str) -> Vec<String> {
    doc.get("sync")
        .and_then(|s| s.get(key))
        .and_then(Item::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn set_strings(doc: &mut DocumentMut, key: &str, values: &[String]) {
    if doc.get("sync").is_none() {
        doc["sync"] = toml_edit::table();
    }
    let mut arr = Array::new();
    for v in values {
        arr.push(v.as_str());
    }
    doc["sync"][key] = Item::Value(Value::Array(arr));
}

fn save(root: &Path, doc: &DocumentMut) -> Result<()> {
    let text = doc.to_string();
    // Refuse to write something the loader would reject.
    let cfg: Config = toml::from_str(&text).map_err(|e| ClientError::Config(e.to_string()))?;
    cfg.validate().map_err(ClientError::Config)?;
    std::fs::write(root.join(CONFIG_FILE), text)?;
    Ok(())
}

/// Adds `rel` to the selection.
pub fn select(root: &Path, rel: &str) -> Result<()> {
    let rel = norm_rel(rel);
    let mut doc = load_doc(root)?;
    let mut include = strings(&doc, "include");
    let mut ignore = strings(&doc, "ignore");
    // Stop excluding it (and anything inside it).
    ignore.retain(|g| is_glob(g) || !covers(&rel, &norm_rel(g)));
    if let Some(parent) = ignore.iter().find(|g| !is_glob(g) && covers(&norm_rel(g), &rel)) {
        return Err(ClientError::Other(format!(
            "{rel} is inside {}, which is not synced; select {} instead",
            norm_rel(parent),
            norm_rel(parent)
        )));
    }
    let norm_inc: Vec<String> = include.iter().map(|i| norm_rel(i)).collect();
    if !norm_inc.is_empty() && !norm_inc.iter().any(|i| covers(i, &rel)) {
        include.retain(|i| !covers(&rel, &norm_rel(i)));
        include.push(rel);
    }
    set_strings(&mut doc, "include", &include);
    set_strings(&mut doc, "ignore", &ignore);
    save(root, &doc)
}

/// Removes `rel` from the selection.
pub fn unselect(root: &Path, rel: &str) -> Result<()> {
    let rel = norm_rel(rel);
    if rel.is_empty() {
        return Err(ClientError::Other("cannot unselect the whole project".into()));
    }
    let mut doc = load_doc(root)?;
    let mut include = strings(&doc, "include");
    let mut ignore = strings(&doc, "ignore");
    let before = include.len();
    include.retain(|i| !covers(&rel, &norm_rel(i)));
    let removed = include.len() != before;
    let still_covered = include.is_empty() && !removed || include.iter().any(|i| covers(&norm_rel(i), &rel));
    if still_covered || (removed && include.is_empty()) {
        // Inside a selected folder (or the last selected path): exclude it explicitly,
        // since an empty include list means "everything".
        let entry = format!("/{rel}");
        if !ignore.iter().any(|g| !is_glob(g) && norm_rel(g) == rel) {
            ignore.push(entry);
        }
        if removed && include.is_empty() {
            include.push(rel.clone());
        }
    }
    set_strings(&mut doc, "include", &include);
    set_strings(&mut doc, "ignore", &ignore);
    save(root, &doc)
}

/// Back to syncing everything (plain-path excludes made by `unselect` are
/// removed; glob patterns are kept).
pub fn reset(root: &Path) -> Result<()> {
    let mut doc = load_doc(root)?;
    let ignore: Vec<String> = strings(&doc, "ignore").into_iter().filter(|g| is_glob(g)).collect();
    set_strings(&mut doc, "include", &[]);
    set_strings(&mut doc, "ignore", &ignore);
    save(root, &doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(CONFIG_FILE), Config::template("me@box", "~/p")).unwrap();
        d
    }

    fn cfg(d: &Path) -> SyncConfig {
        Config::load(&d.join(CONFIG_FILE)).unwrap().sync
    }

    #[test]
    fn select_and_unselect_round_trip() {
        let d = project();
        let r = d.path();
        assert_eq!(state(&cfg(r), "data"), State::Selected, "everything by default");

        // Exclude a big folder from "everything".
        unselect(r, "data").unwrap();
        assert_eq!(cfg(r).ignore, vec!["/data"]);
        assert_eq!(state(&cfg(r), "data"), State::NotSelected);
        assert_eq!(state(&cfg(r), "data/x.csv"), State::NotSelected);
        assert_eq!(state(&cfg(r), "src"), State::Selected);
        assert_eq!(state(&cfg(r), ""), State::Partial);
        select(r, "data").unwrap();
        assert!(cfg(r).ignore.is_empty());

        // Sparse: only some folders.
        std::fs::write(
            r.join(CONFIG_FILE),
            std::fs::read_to_string(r.join(CONFIG_FILE)).unwrap().replace("include = []", "include = [\"src/\"]"),
        )
        .unwrap();
        select(r, "./tests/").unwrap();
        assert_eq!(cfg(r).include, vec!["src/", "tests"]);
        assert_eq!(state(&cfg(r), "data"), State::NotSelected);
        select(r, "src/api").unwrap();
        assert_eq!(cfg(r).include, vec!["src/", "tests"], "already covered");
        unselect(r, "src/api/big").unwrap();
        assert_eq!(state(&cfg(r), "src/api/big"), State::NotSelected);
        assert_eq!(state(&cfg(r), "src"), State::Partial);
        unselect(r, "tests").unwrap();
        unselect(r, "src").unwrap();
        // Removing the last included path must not mean "everything".
        assert_eq!(state(&cfg(r), "src"), State::NotSelected);
        assert_eq!(state(&cfg(r), "data"), State::NotSelected);

        // Comments survive edits.
        assert!(std::fs::read_to_string(r.join(CONFIG_FILE)).unwrap().contains("# devsync project configuration"));
        reset(r).unwrap();
        assert_eq!(state(&cfg(r), "data"), State::Selected);
    }
}
