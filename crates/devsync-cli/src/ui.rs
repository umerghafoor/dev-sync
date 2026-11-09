//! Rendering of progress events: human-readable (stderr) or NDJSON.

use devsync_client::{Direction, Event, Reporter};
use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Per-file lines are printed above the bar for transfers up to this many
/// files (always with -v); bigger ones show only the bar and a summary.
const LIST_FILES_UP_TO: usize = 200;

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
    /// stderr is a terminal: draw a live progress line.
    live: bool,
}

/// One live line per process, shared by every renderer so any message
/// (from any reporter) clears it first.
static BAR: Mutex<Bar> = Mutex::new(Bar { shown: false, planned: 0, rate_from: None });

fn bar() -> std::sync::MutexGuard<'static, Bar> {
    BAR.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clears the live line; call before exiting.
pub fn finish() {
    let mut b = bar();
    if b.shown {
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
        b.shown = false;
    }
}

/// State of the live line at the bottom of the terminal.
struct Bar {
    /// A live line is on screen and must be cleared before printing.
    shown: bool,
    /// Files in the current transfer (to decide whether to list them).
    planned: usize,
    /// Rate measurement: (start time, bytes at start, direction).
    rate_from: Option<(Instant, u64, Direction)>,
}

impl Human {
    pub fn new(verbosity: Verbosity, terse: bool) -> Self {
        let tty = std::io::stderr().is_terminal();
        let color = tty && std::env::var_os("NO_COLOR").is_none();
        let live = tty && verbosity != Verbosity::Quiet && std::env::var_os("DEVSYNC_NO_PROGRESS").is_none();
        Self { verbosity, terse, color, live }
    }

    /// Replaces the live line (terminal only).
    fn live_line(&self, s: &str) {
        if !self.live {
            return;
        }
        let mut b = bar();
        let text = fit(s, term_width().saturating_sub(1));
        eprint!("\r\x1b[2K{text}");
        let _ = std::io::stderr().flush();
        b.shown = true;
    }

    fn progress(&self, dir: Direction, path: &str, files: (usize, usize), done: u64, total: u64) {
        if !self.live || self.terse {
            return;
        }
        let (rate, eta) = {
            let mut b = bar();
            let from = match b.rate_from {
                Some(f) if f.2 == dir && f.1 <= done => f,
                _ => {
                    let f = (Instant::now(), done, dir);
                    b.rate_from = Some(f);
                    f
                }
            };
            let secs = from.0.elapsed().as_secs_f64();
            if secs > 0.5 {
                let r = (done - from.1) as f64 / secs;
                let eta =
                    if r > 0.0 { Some(Duration::from_secs_f64(total.saturating_sub(done) as f64 / r)) } else { None };
                (Some(r), eta)
            } else {
                (None, None)
            }
        };
        let pct = if total == 0 {
            if files.1 == 0 {
                100.0
            } else {
                files.0 as f64 * 100.0 / files.1 as f64
            }
        } else {
            done.min(total) as f64 * 100.0 / total as f64
        };
        let width = 24;
        let filled = ((pct / 100.0) * width as f64).round() as usize;
        let meter = format!("{}{}", "█".repeat(filled.min(width)), "░".repeat(width - filled.min(width)));
        let (arrow, verb) = match dir {
            Direction::Up => ("↑", "pushing"),
            Direction::Down => ("↓", "pulling"),
        };
        let mut line = format!(
            "{} {} {pct:>3.0}% {}/{} files  {} / {}",
            self.paint("36", &format!("{arrow} {verb}")),
            self.paint("36", &meter),
            files.0,
            files.1,
            bytes(done.min(total)),
            bytes(total),
        );
        if let Some(r) = rate {
            line.push_str(&format!("  {}/s", bytes(r as u64)));
        }
        if let Some(eta) = eta {
            line.push_str(&format!("  {} left", duration(eta)));
        }
        if !path.is_empty() {
            line.push_str(&format!("  {}", self.paint("2", path)));
        }
        self.live_line(&line);
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn line(&self, s: &str) {
        finish();
        eprintln!("{} {s}", self.paint("2", "[devsync]"));
    }

    /// Removes the live line, if any.
    pub fn done(&self) {
        finish();
        bar().rate_from = None;
    }

    fn list_files(&self) -> bool {
        self.verbosity == Verbosity::Verbose
            || (!self.terse && self.verbosity == Verbosity::Normal && bar().planned <= LIST_FILES_UP_TO)
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

fn duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Terminal width of stderr (80 if unknown).
fn term_width() -> usize {
    if let Some(c) = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()) {
        return c;
    }
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ fills a winsize struct we own.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    80
}

/// Cuts `s` to `width` visible characters, ignoring ANSI color codes.
fn fit(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut visible = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            for d in chars.by_ref() {
                out.push(d);
                if d == 'm' {
                    break;
                }
            }
            continue;
        }
        if visible >= width {
            out.push_str("\x1b[0m");
            break;
        }
        out.push(c);
        visible += 1;
    }
    out
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
            Event::Connecting { host } => {
                if verbose {
                    self.line(&format!("connecting to {host}"))
                } else if !self.terse {
                    self.live_line(&format!("{} connecting to {host}…", self.paint("2", "[devsync]")))
                }
            }
            Event::Connected { os, arch, root, agent_version } if verbose => {
                self.line(&format!("agent {agent_version} on {os}/{arch}, root {root}"))
            }
            Event::Stage { message, .. } => {
                if verbose {
                    self.line(&format!("{message}…"))
                } else if !self.terse {
                    self.live_line(&format!("{} {message}…", self.paint("2", "[devsync]")))
                }
            }
            Event::Retrying { attempt, error, delay_ms } => {
                self.warn(&format!("network problem ({error}) — retry #{attempt} in {:.1}s", delay_ms as f64 / 1000.0))
            }
            Event::Planned { upload, upload_bytes, delete } => {
                bar().planned = upload;
                if (upload > 0 || delete > 0) && !quiet && (!self.terse || verbose) {
                    let del = if delete > 0 { format!(", {delete} to delete") } else { String::new() };
                    self.line(&format!("pushing {upload} file(s), {}{del}", bytes(upload_bytes)))
                }
            }
            Event::PullPlanned { download, download_bytes, .. } => {
                bar().planned = download;
                if download > 0 && verbose {
                    self.line(&format!("pulling {download} file(s), {}", bytes(download_bytes)))
                }
            }
            Event::Progress { direction, path, files_done, files_total, bytes_done, bytes_total } => {
                self.progress(direction, &path, (files_done, files_total), bytes_done, bytes_total)
            }
            Event::Uploaded { path, bytes: b } if self.list_files() => {
                self.line(&format!("  {} {path} {}", self.paint("32", "↑"), self.paint("2", &bytes(b))))
            }
            Event::Deleted { path } if self.list_files() => self.line(&format!("  {} {path}", self.paint("31", "✗"))),
            Event::Synced { files, bytes: b, deleted, millis } => {
                self.done();
                if !quiet && (!self.terse || verbose) {
                    let del = if deleted > 0 { format!(", {deleted} deleted") } else { String::new() };
                    self.ok(&format!("synced {files} file(s), {}{del} in {:.1}s", bytes(b), millis as f64 / 1000.0))
                }
            }
            Event::UpToDate => {
                self.done();
                if !quiet && !self.terse {
                    self.ok("remote is up to date")
                }
            }
            Event::Pulled { path, bytes: b } if !quiet && (self.list_files() || !self.live) => {
                self.line(&format!("  {} {path} {}", self.paint("34", "↓"), self.paint("2", &bytes(b))))
            }
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
            Event::NeedsSelection { message } => self.warn(&message),
            Event::Offline { error, retry_in_ms } => {
                self.done();
                let when = if retry_in_ms > 0 {
                    format!("retrying in {}s (press Enter to retry now)", retry_in_ms.div_ceil(1000))
                } else {
                    "changes will be sent when the link is back".into()
                };
                self.warn(&format!("offline: {error} — {when}"))
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
