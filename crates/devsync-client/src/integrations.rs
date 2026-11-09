//! Coding-agent integration: a Claude Code PreToolUse hook that transparently
//! sends code-executing shell commands to the remote, plus instruction blocks
//! for CLAUDE.md / AGENTS.md.

use crate::config::{Config, CONFIG_FILE};
use crate::project::find_root;
use crate::route::{classify, Route};
use devsync_transport::endpoint::sh_quote;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

const BEGIN: &str = "<!-- devsync:start -->";
const END: &str = "<!-- devsync:end -->";

pub fn instructions() -> String {
    format!(
        "{BEGIN}
## Remote execution (devsync)

This project is edited on this machine but **runs on a remote machine** (see `.devsync.toml`).

- Shell commands that execute code (tests, builds, package installs, scripts, servers,
  `ps`/`kill`) are routed to the remote automatically by a hook: local changes are synced
  first, the command runs in the matching remote directory, output is shown, the exit code
  is preserved, and files the command created or changed (lockfiles, generated code) are
  copied back here. A copy that would overwrite a local edit is saved as `<file>.devsync-remote`.
- Reading, searching and editing files, and git, stay local.
- Force a command to run locally by ending it with `# local`; force remote with `# remote`.
- Every remote run is recorded in `.devsync/jobs/<id>/` (`output.log`, `status.json`).
- Long-running processes (dev servers, watchers, training): start them with
  `devsync submit \"<cmd>\"`, read progress with `devsync logs <id>` or by reading
  `.devsync/jobs/<id>/output.log` after `devsync collect`, stop with `devsync kill <id>`.
  Never run a server in the foreground.
- The network is unreliable. devsync retries and reconnects by itself and remote commands keep
  running through disconnects. Do not re-run a command just because of a retry message.
{END}"
    )
}

/// Inserts or replaces the devsync block in a markdown file.
pub fn upsert_block(path: &Path) -> std::io::Result<()> {
    let block = instructions();
    let text = fs::read_to_string(path).unwrap_or_default();
    let new = match (text.find(BEGIN), text.find(END)) {
        (Some(a), Some(b)) if b > a => format!("{}{}{}", &text[..a], block, &text[b + END.len()..]),
        _ if text.trim().is_empty() => format!("{block}\n"),
        _ => format!("{}\n\n{block}\n", text.trim_end()),
    };
    fs::write(path, new)
}

/// Installs the Claude Code hook into `<root>/.claude/settings.json`
/// (merging with existing settings) and the CLAUDE.md block.
pub fn setup_claude(root: &Path, exe: &Path) -> Result<(), String> {
    let dir = root.join(".claude");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("settings.json");
    let mut settings: Value = match fs::read_to_string(&path) {
        Ok(t) if !t.trim().is_empty() => {
            serde_json::from_str(&t).map_err(|e| format!("{}: invalid JSON ({e}); not modifying it", path.display()))?
        }
        _ => json!({}),
    };
    let command = format!("{} hook claude", sh_quote(&exe.display().to_string()));
    let obj = settings.as_object_mut().ok_or("settings.json is not a JSON object")?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let pre =
        hooks.as_object_mut().ok_or("\"hooks\" is not an object")?.entry("PreToolUse").or_insert_with(|| json!([]));
    let arr = pre.as_array_mut().ok_or("\"hooks.PreToolUse\" is not an array")?;
    // Drop earlier devsync hooks (e.g. from a different install path).
    arr.retain(|m| {
        !m["hooks"].as_array().is_some_and(|hs| {
            hs.iter().any(|h| h["command"].as_str().is_some_and(|c| c.contains("devsync") && c.contains(" hook")))
        })
    });
    arr.push(json!({
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": command, "timeout": 15 }]
    }));
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())? + "\n";
    fs::write(&path, text).map_err(|e| e.to_string())?;
    upsert_block(&root.join("CLAUDE.md")).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn setup_agents_md(root: &Path) -> Result<(), String> {
    upsert_block(&root.join("AGENTS.md")).map_err(|e| e.to_string())
}

/// Handles one Claude Code PreToolUse hook invocation. Returns the JSON to
/// print, or `None` to let the command run unchanged.
pub fn claude_hook(input: &str, exe: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(input).ok()?;
    if v["tool_name"].as_str()? != "Bash" {
        return None;
    }
    let tool_input = v["tool_input"].as_object()?;
    let cmd = tool_input.get("command")?.as_str()?;
    let cwd = v["cwd"].as_str().map(Path::new).map(Path::to_path_buf).or_else(|| std::env::current_dir().ok())?;
    let root = find_root(&cwd)?;
    let cfg = Config::load(&root.join(CONFIG_FILE)).unwrap_or_default();
    if classify(cmd, &cfg.exec.local_commands, &cfg.exec.remote_commands) == Route::Local {
        return None;
    }
    let mut updated = tool_input.clone();
    updated.insert(
        "command".into(),
        Value::String(format!("{} exec -- {}", sh_quote(&exe.display().to_string()), sh_quote(cmd))),
    );
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": updated
            }
        })
        .to_string(),
    )
}

/// Makes sure `.devsync/` is ignored by git in `root`. Only touches projects
/// that use git: an existing `.gitignore`, or a git repository at or above
/// `root`. A plain folder is left untouched.
pub fn gitignore_state_dir(root: &Path) {
    let gi = root.join(".gitignore");
    let in_git = root.ancestors().any(|d| d.join(".git").exists());
    if !gi.exists() && !in_git {
        return;
    }
    let text = fs::read_to_string(&gi).unwrap_or_default();
    if text.lines().any(|l| matches!(l.trim(), ".devsync/" | ".devsync" | "/.devsync/" | "/.devsync")) {
        return;
    }
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    let _ = fs::write(gi, format!("{text}{sep}.devsync/\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(CONFIG_FILE), Config::template("me@box", "~/p")).unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        d
    }

    fn hook(cmd: &str, cwd: &Path) -> Option<Value> {
        let input = json!({"tool_name": "Bash", "cwd": cwd, "tool_input": {"command": cmd, "description": "d"}});
        claude_hook(&input.to_string(), Path::new("/opt/dev sync/devsync")).map(|s| serde_json::from_str(&s).unwrap())
    }

    #[test]
    fn hook_rewrites_only_remote_commands() {
        let d = project();
        assert!(hook("git status", d.path()).is_none());
        let out = hook("pytest -x", &d.path().join("src")).unwrap();
        let u = &out["hookSpecificOutput"]["updatedInput"];
        assert_eq!(u["command"], "'/opt/dev sync/devsync' exec -- 'pytest -x'");
        assert_eq!(u["description"], "d");
        assert!(out["hookSpecificOutput"]["permissionDecision"].is_null());
        // Outside a devsync project nothing changes.
        assert!(hook("pytest", Path::new("/")).is_none());
    }

    #[test]
    fn setup_is_idempotent_and_merges() {
        let d = project();
        fs::create_dir_all(d.path().join(".claude")).unwrap();
        fs::write(d.path().join(".claude/settings.json"), r#"{"permissions":{"allow":["Bash(ls *)"]}}"#).unwrap();
        fs::write(d.path().join("CLAUDE.md"), "# Project\n\nNotes.\n").unwrap();
        setup_claude(d.path(), Path::new("/usr/local/bin/devsync")).unwrap();
        setup_claude(d.path(), Path::new("/usr/local/bin/devsync")).unwrap();
        let s: Value =
            serde_json::from_str(&fs::read_to_string(d.path().join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(s["permissions"]["allow"][0], "Bash(ls *)");
        assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        let md = fs::read_to_string(d.path().join("CLAUDE.md")).unwrap();
        assert!(md.starts_with("# Project"));
        assert_eq!(md.matches(BEGIN).count(), 1);
    }

    #[test]
    fn gitignore_left_alone_outside_git() {
        let d = tempfile::tempdir().unwrap();
        gitignore_state_dir(d.path());
        assert!(!d.path().join(".gitignore").exists(), "plain folders get no .gitignore");
        fs::create_dir(d.path().join(".git")).unwrap();
        gitignore_state_dir(d.path());
        assert_eq!(fs::read_to_string(d.path().join(".gitignore")).unwrap(), ".devsync/\n");
    }

    #[test]
    fn gitignore_entry_added_once() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(".gitignore"), "target").unwrap();
        gitignore_state_dir(d.path());
        gitignore_state_dir(d.path());
        assert_eq!(fs::read_to_string(d.path().join(".gitignore")).unwrap(), "target\n.devsync/\n");
    }
}
