//! Computes what must change on the destination to mirror the source.

use devsync_proto::{Entry, EntryKind};
use std::collections::HashMap;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Source entries that are missing or different on the destination.
    pub upload: Vec<Entry>,
    /// Destination paths absent from the source (only when deletion is enabled).
    pub delete: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.upload.is_empty() && self.delete.is_empty()
    }

    pub fn upload_bytes(&self) -> u64 {
        self.upload.iter().map(|e| e.size).sum()
    }
}

fn exec_bit(mode: u32) -> bool {
    mode & 0o111 != 0
}

/// True when `dst` already has the same content as `src`.
pub fn same(src: &Entry, dst: &Entry) -> bool {
    src.kind == dst.kind
        && src.hash == dst.hash
        && (src.kind == EntryKind::Symlink || exec_bit(src.mode) == exec_bit(dst.mode))
}

pub fn plan(src: &[Entry], dst: &[Entry], delete: bool) -> Plan {
    let dst_map: HashMap<&str, &Entry> = dst.iter().map(|e| (e.path.as_str(), e)).collect();
    let upload = src.iter().filter(|s| dst_map.get(s.path.as_str()).is_none_or(|d| !same(s, d))).cloned().collect();
    let delete = if delete {
        let src_set: std::collections::HashSet<&str> = src.iter().map(|e| e.path.as_str()).collect();
        dst.iter().filter(|d| !src_set.contains(d.path.as_str())).map(|d| d.path.clone()).collect()
    } else {
        Vec::new()
    };
    Plan { upload, delete }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(path: &str, h: u8, mode: u32) -> Entry {
        Entry { path: path.into(), kind: EntryKind::File, size: 1, mode, mtime_ns: 0, hash: [h; 32], link: None }
    }

    #[test]
    fn plans_uploads_and_deletes() {
        let src = vec![e("a", 1, 0o644), e("b", 2, 0o644), e("c", 3, 0o755)];
        let dst = vec![e("a", 1, 0o600), e("b", 9, 0o644), e("c", 3, 0o644), e("old", 4, 0o644)];
        let p = plan(&src, &dst, true);
        let up: Vec<_> = p.upload.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(up, vec!["b", "c"]); // a: only non-exec bits differ
        assert_eq!(p.delete, vec!["old".to_string()]);
        assert!(plan(&src, &dst, false).delete.is_empty());
    }
}
