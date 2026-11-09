//! Rendering of progress events: human-readable (stderr) or NDJSON.

use devsync_client::{Event, Reporter};
use std::io::{IsTerminal, Write};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

pub struct Human {
    pub verbosity: Verbosity,
    /// Hide routine sync chatter (used while running a command).
    pub terse: bool,
    color: bool,
}

impl Human {
    pub fn new(verbosity: Verbosity, terse: bool) -> Self {
        let color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self { verbosity, terse, color }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn line(&self, s: &str) {
        eprintln!("{} {s}", self.paint("2", "[devsync]"));
    }

    pub fn ok(&self, s: &str) {
        self.line(&self.paint("32", s));
    }

    pub fn warn(&self, s: &str) {
        self.line(&self.paint("33", s));
    }

    pub fn error(&self, s: &str) {
        self.line(&self.paint("31", s));
    }
}

pub fn bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

impl Reporter for Human {
    fn event(&self, e: Event) {
        let quiet = self.verbosity == Verbosity::Quiet;
        let verbose = self.verbosity == Verbosity::Verbose;
        match e {
            Event::Connecting { host } if verbose => self.line(&format!("connecting to {host}")),
            Event::Connected { os, arch, root, agent_version } if verbose => {
                self.line(&format!("agent {agent_version} on {os}/{arch}, root {root}"))
            }
            Event::Retrying { attempt, error, delay_ms } => {
                self.warn(&format!("network problem ({error}) — retry #{attempt} in {:.1}s", delay_ms as f64 / 1000.0))
            }
            Event::Planned { upload, upload_bytes, delete } if verbose && (upload > 0 || delete > 0) => {
                self.line(&format!("{upload} file(s) to upload ({}), {delete} to delete", bytes(upload_bytes)))
            }
            Event::Uploaded { path, bytes: b } if verbose => self.line(&format!("  ↑ {path} ({})", bytes(b))),
            Event::Deleted { path } if verbose => self.line(&format!("  ✗ {path}")),
            Event::Synced { files, bytes: b, deleted, millis } if !quiet && (!self.terse || verbose) => {
                let del = if deleted > 0 { format!(", {deleted} deleted") } else { String::new() };
                self.ok(&format!("synced {files} file(s), {}{del} in {:.1}s", bytes(b), millis as f64 / 1000.0))
            }
            Event::UpToDate if !quiet && !self.terse => self.ok("remote is up to date"),
            Event::Pulled { path } if !quiet => self.line(&format!("  ↓ {path}")),
            Event::Conflict { path, saved_as } => self.warn(&format!(
                "{path} was changed both locally and by the remote command; remote version saved as {saved_as}"
            )),
            Event::JobStarted { id, command, cwd } if verbose => {
                self.line(&format!("job {id} started in ./{cwd}: {command}"))
            }
            Event::JobFinished { id, state, exit_code } if verbose || (state == "lost" && !quiet) => {
                self.line(&format!("job {id} {state}{}", exit_code.map(|c| format!(" ({c})")).unwrap_or_default()))
            }
            Event::JobUpdated { .. } => {}
            Event::Watching { root } if !quiet => self.ok(&format!("watching {root} (Ctrl-C to stop)")),
            Event::Changed { paths } if verbose => self.line(&format!("{paths} change(s) detected")),
            Event::Offline { error } => {
                self.warn(&format!("offline: {error} — changes will be sent when the link is back"))
            }
            Event::Warning { message } => self.warn(&message),
            Event::Info { message } if !quiet => self.line(&message),
            _ => {}
        }
    }
}

/// NDJSON events, one per line, on stdout (or stderr when stdout carries
/// command output).
pub struct Json {
    pub to_stderr: bool,
}

impl Reporter for Json {
    fn event(&self, e: Event) {
        let line = serde_json::to_string(&e).unwrap_or_default();
        if self.to_stderr {
            eprintln!("{line}");
        } else {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}
