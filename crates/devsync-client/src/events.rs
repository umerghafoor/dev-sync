//! Progress events. The CLI renders them for humans or as NDJSON
//! (`--json`), which is what the VS Code extension consumes.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Connecting {
        host: String,
    },
    Connected {
        os: String,
        arch: String,
        root: String,
        agent_version: String,
    },
    Retrying {
        attempt: u32,
        error: String,
        delay_ms: u64,
    },
    Planned {
        upload: usize,
        upload_bytes: u64,
        delete: usize,
    },
    Uploaded {
        path: String,
        bytes: u64,
    },
    Deleted {
        path: String,
    },
    Synced {
        files: usize,
        bytes: u64,
        deleted: usize,
        millis: u64,
    },
    UpToDate,
    Pulled {
        path: String,
        bytes: u64,
    },
    /// What a sync is doing before files move (scanning can take a while
    /// on big trees).
    Stage {
        stage: Stage,
        message: String,
    },
    /// What a pull will download.
    PullPlanned {
        download: usize,
        download_bytes: u64,
        /// Of those, files edited locally: saved as `*.devsync-remote`.
        conflicts: usize,
        /// Remote files already identical locally (not downloaded again).
        already: usize,
    },
    /// Transfer progress (throttled to a few updates per second).
    Progress {
        direction: Direction,
        /// File being transferred, and how far along it is.
        path: String,
        file_done: u64,
        file_size: u64,
        /// Files of this transfer completed / planned.
        files_done: usize,
        files_total: usize,
        /// Files that were already in sync before it started (not counted
        /// above), so "here / in project" = (already + done) / (already + total).
        files_already: usize,
        bytes_done: u64,
        bytes_total: u64,
    },
    Conflict {
        path: String,
        saved_as: String,
    },
    JobStarted {
        id: String,
        command: String,
        cwd: String,
    },
    JobFinished {
        id: String,
        state: String,
        exit_code: Option<i32>,
    },
    JobUpdated {
        id: String,
        state: String,
        new_bytes: u64,
    },
    Watching {
        root: String,
    },
    Changed {
        paths: usize,
    },
    Offline {
        error: String,
        /// When the next attempt is made (0 = unknown).
        retry_in_ms: u64,
    },
    /// The remote is too large to sync whole; waiting for a selection.
    NeedsSelection {
        message: String,
    },
    Warning {
        message: String,
    },
    Info {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    ScanLocal,
    ScanRemote,
    Upload,
    Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Up,
    Down,
}

/// Turns per-chunk byte counts into throttled [`Event::Progress`] events.
pub struct Transfer<'a> {
    rep: &'a dyn Reporter,
    direction: Direction,
    files_total: usize,
    bytes_total: u64,
    files_done: usize,
    files_already: usize,
    bytes_done: u64,
    path: String,
    file_base: u64,
    file_size: u64,
    last: Option<std::time::Instant>,
}

impl<'a> Transfer<'a> {
    const EVERY: std::time::Duration = std::time::Duration::from_millis(100);

    pub fn new(rep: &'a dyn Reporter, direction: Direction, files_total: usize, bytes_total: u64) -> Self {
        Self {
            rep,
            direction,
            files_total,
            bytes_total,
            files_done: 0,
            files_already: 0,
            bytes_done: 0,
            path: String::new(),
            file_base: 0,
            file_size: 0,
            last: None,
        }
    }

    /// Files already in sync before this transfer (shown as done).
    pub fn already(mut self, n: usize) -> Self {
        self.files_already = n;
        self
    }

    /// Starts a file of `size` bytes, of which `already` were sent earlier
    /// (resumed upload).
    pub fn file(&mut self, path: &str, size: u64, already: u64) {
        self.path = path.to_string();
        self.file_base = self.bytes_done;
        self.file_size = size;
        self.bytes_done += already.min(size);
        self.emit(false);
    }

    pub fn bytes(&mut self, n: u64) {
        self.bytes_done = (self.bytes_done + n).min(self.file_base + self.file_size);
        self.emit(false);
    }

    pub fn file_done(&mut self) {
        self.files_done += 1;
        self.bytes_done = self.file_base + self.file_size;
        self.emit(false);
    }

    /// Always reports the final state.
    pub fn finish(&mut self) {
        self.emit(true);
    }

    fn emit(&mut self, force: bool) {
        let now = std::time::Instant::now();
        if !force && self.last.is_some_and(|t| now.duration_since(t) < Self::EVERY) {
            return;
        }
        self.last = Some(now);
        self.rep.event(Event::Progress {
            direction: self.direction,
            path: self.path.clone(),
            file_done: self.bytes_done - self.file_base,
            file_size: self.file_size,
            files_done: self.files_done,
            files_total: self.files_total,
            files_already: self.files_already,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
        });
    }
}

/// Receives events; implemented by the CLI's renderers.
pub trait Reporter: Sync {
    fn event(&self, e: Event);
}

/// Discards all events.
pub struct Silent;

impl Reporter for Silent {
    fn event(&self, _: Event) {}
}
