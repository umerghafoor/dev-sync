//! Decides whether a shell command line should run locally or on the remote.
//!
//! Used by the coding-agent hook. File inspection/editing and version control
//! stay local (fast, and the files are local); anything that executes project
//! code goes to the remote. The decision looks at the first word of every
//! pipeline segment, with a small shell lexer that understands quotes,
//! operators, comments and heredocs.

/// Commands that never need the remote environment.
pub const LOCAL_COMMANDS: &[&str] = &[
    "git",
    "ls",
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "find",
    "fd",
    "sed",
    "awk",
    "wc",
    "diff",
    "cmp",
    "echo",
    "printf",
    "pwd",
    "cd",
    "pushd",
    "popd",
    "mkdir",
    "rmdir",
    "mv",
    "cp",
    "rm",
    "touch",
    "chmod",
    "chown",
    "ln",
    "tree",
    "file",
    "stat",
    "du",
    "df",
    "jq",
    "yq",
    "sort",
    "uniq",
    "cut",
    "tr",
    "tee",
    "xargs",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "true",
    "false",
    "test",
    "[",
    "sleep",
    "date",
    "which",
    "type",
    "command",
    "export",
    "unset",
    "source",
    ".",
    "alias",
    "devsync",
    "open",
    "code",
    "cursor",
    "gh",
    "curl",
    "wget",
    "ssh",
    "scp",
    "rsync",
    "tar",
    "zip",
    "unzip",
    "gzip",
    "gunzip",
    "base64",
    "shasum",
    "sha256sum",
    "md5",
    "md5sum",
    "clear",
    "history",
    "patch",
    "column",
    "nl",
    "fold",
    "paste",
    "comm",
    "join",
    "split",
    "xxd",
    "hexdump",
    "od",
    "strings",
    "whoami",
    "hostname",
    "uname",
];

/// Prefixes that wrap another command; the wrapped command decides.
const WRAPPERS: &[&str] = &["sudo", "time", "nohup", "nice", "env", "exec", "builtin", "caffeinate"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Local,
    Remote,
}

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Sep,
}

#[allow(unused_assignments)] // the flush! macro resets state also at the very end
fn lex(input: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = input.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let mut cur = String::new();
    let mut in_word = false;
    let mut heredocs: Vec<String> = Vec::new();
    let mut pending_heredoc = false;

    macro_rules! flush {
        () => {
            if in_word {
                let w = std::mem::take(&mut cur);
                if pending_heredoc {
                    heredocs.push(w.clone());
                    pending_heredoc = false;
                }
                toks.push(Tok::Word(w));
                in_word = false;
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    cur.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return None;
                }
            }
            '"' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return None;
                }
            }
            '\\' => {
                in_word = true;
                if i + 1 < chars.len() && chars[i + 1] != '\n' {
                    cur.push(chars[i + 1]);
                }
                i += 1;
            }
            '#' if !in_word => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '\n' => {
                flush!();
                toks.push(Tok::Sep);
                // Skip heredoc bodies that start on the next line.
                for delim in std::mem::take(&mut heredocs) {
                    loop {
                        let start = i + 1;
                        if start > chars.len() {
                            break;
                        }
                        let end = chars[start..].iter().position(|&c| c == '\n').map_or(chars.len(), |p| start + p);
                        let line: String = chars[start..end].iter().collect();
                        i = end;
                        if line.trim() == delim || end >= chars.len() {
                            break;
                        }
                    }
                }
            }
            ' ' | '\t' => flush!(),
            ';' | '&' | '|' | '(' | ')' | '{' | '}' => {
                flush!();
                toks.push(Tok::Sep);
                while i + 1 < chars.len() && matches!(chars[i + 1], '&' | '|' | ';') {
                    i += 1;
                }
            }
            '<' if !in_word || cur.is_empty() => {
                flush!();
                if i + 1 < chars.len() && chars[i + 1] == '<' {
                    if i + 2 < chars.len() && chars[i + 2] == '<' {
                        i += 2; // here-string
                    } else {
                        i += 1;
                        if i + 1 < chars.len() && chars[i + 1] == '-' {
                            i += 1;
                        }
                        pending_heredoc = true;
                    }
                }
            }
            '>' => flush!(),
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
        i += 1;
    }
    flush!();
    Some(toks)
}

/// First command word of each pipeline segment.
pub fn command_words(input: &str) -> Option<Vec<String>> {
    let toks = lex(input)?;
    let mut out = Vec::new();
    let mut at_start = true;
    for t in toks {
        match t {
            Tok::Sep => at_start = true,
            Tok::Word(w) if at_start => {
                let is_assign = w
                    .split_once('=')
                    .is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
                if is_assign || WRAPPERS.contains(&w.as_str()) || w.starts_with('-') {
                    continue;
                }
                let base = w.rsplit('/').next().unwrap_or(&w).to_string();
                out.push(base);
                at_start = false;
            }
            Tok::Word(_) => {}
        }
    }
    Some(out)
}

fn trailing_marker(cmd: &str) -> Option<Route> {
    let t = cmd.trim_end();
    let (_, comment) = t.rsplit_once('#')?;
    match comment.trim() {
        "local" | "devsync:local" => Some(Route::Local),
        "remote" | "devsync:remote" => Some(Route::Remote),
        _ => None,
    }
}

/// Routes `cmd`. `extra_local` / `extra_remote` come from the project config.
pub fn classify(cmd: &str, extra_local: &[String], extra_remote: &[String]) -> Route {
    if let Some(r) = trailing_marker(cmd) {
        return r;
    }
    let Some(words) = command_words(cmd) else { return Route::Remote };
    if words.is_empty() {
        return Route::Local;
    }
    if words.iter().any(|w| extra_remote.iter().any(|r| r == w)) {
        return Route::Remote;
    }
    let local = |w: &String| LOCAL_COMMANDS.contains(&w.as_str()) || extra_local.iter().any(|l| l == w);
    if words.iter().all(local) {
        Route::Local
    } else {
        Route::Remote
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(c: &str) -> Route {
        classify(c, &[], &[])
    }

    #[test]
    fn local_commands() {
        for c in [
            "git status",
            "ls -la src && cat a.py | grep print",
            r#"grep -rn "foo|bar" src"#,
            "cd src; ls",
            "cat > x.py <<'EOF'\nimport os\nprint(os)\nEOF",
            "cat <<EOF > f\npytest\nnpm install\nEOF\necho done",
            "FOO=1 git log --oneline | head -5",
            "python3 lint.py # local",
            "",
            "# just a comment",
            "/usr/bin/git diff",
            "find . -name '*.py' | xargs wc -l",
        ] {
            assert_eq!(r(c), Route::Local, "{c:?}");
        }
    }

    #[test]
    fn remote_commands() {
        for c in [
            "pytest -x tests/",
            "cd src && python3 a.py",
            "npm install lodash",
            "FOO=1 npm test",
            "ps aux | grep node",
            "ls # remote",
            "sudo systemctl restart app",
            "(cd web && npm run build)",
            "echo hi && make",
            "cat <<EOF > f\nhello\nEOF\npython3 f",
            "echo 'unterminated",
        ] {
            assert_eq!(r(c), Route::Remote, "{c:?}");
        }
    }

    #[test]
    fn config_overrides() {
        assert_eq!(classify("make fmt", &["make".into()], &[]), Route::Local);
        assert_eq!(classify("git push", &[], &["git".into()]), Route::Remote);
    }
}
