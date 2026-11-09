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

/// Receives events; implemented by the CLI's renderers.
pub trait Reporter: Sync {
    fn event(&self, e: Event);
}

/// Discards all events.
pub struct Silent;

impl Reporter for Silent {
    fn event(&self, _: Event) {}
}
