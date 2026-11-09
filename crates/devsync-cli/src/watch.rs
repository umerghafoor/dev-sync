//! `devsync watch`: push on every save and collect job results.
//!
//! Network failures never stop the watcher: changes stay pending and are
//! sent with backoff once the link is back.

use crate::commands::{project, reporter};
use crate::Global;
use anyhow::{Context, Result};
use devsync_client::{jobs, sync, ClientError, Event};
use devsync_transport::Transient;
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Paths whose changes never trigger a push (our own state, VCS internals).
fn relevant(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else { return false };
    let mut comps = rel.components().map(|c| c.as_os_str().to_string_lossy());
    match comps.next().as_deref() {
        None => false,
        Some(".devsync") | Some(".git") => false,
        Some(_) => !rel.components().any(|c| c.as_os_str() == "node_modules" || c.as_os_str() == ".git"),
    }
}

pub fn run(g: &Global) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, false, false);
    let (tx, rx) = channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(300), tx).context("cannot start file watcher")?;
    debouncer
        .watcher()
        .watch(&p.root, RecursiveMode::Recursive)
        .with_context(|| format!("cannot watch {}", p.root.display()))?;
    rep.event(Event::Watching { root: p.root.display().to_string() });

    let mut dirty = true; // initial sync
    let mut backoff = Duration::from_secs(1);
    let mut next_try = Instant::now();
    let mut next_collect = Instant::now();
    let opts = sync::PushOptions::default();

    loop {
        if devsync_client::interrupted() {
            return Ok(0);
        }
        // Wait for file events, but wake regularly for retries and job collection.
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(events)) => {
                let n = events.iter().filter(|e| relevant(&p.root, &e.path)).count();
                if n > 0 {
                    rep.event(Event::Changed { paths: n });
                    dirty = true;
                }
            }
            Ok(Err(e)) => rep.event(Event::Warning { message: format!("file watcher: {e:?}") }),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("file watcher stopped"),
        }

        let now = Instant::now();
        if dirty && now >= next_try {
            // Single attempt per round: we own the retry loop here so file
            // events keep being processed while offline.
            let once = devsync_transport::RetryPolicy { max_attempts: 1, ..Default::default() };
            match sync::push_with(&p, rep.as_ref(), opts, once) {
                Ok(_) => {
                    dirty = false;
                    backoff = Duration::from_secs(1);
                }
                Err(e) if e.is_transient() => {
                    rep.event(Event::Offline { error: e.to_string() });
                    next_try = now + backoff;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
                Err(ClientError::Interrupted) => return Ok(0),
                Err(e) => {
                    rep.event(Event::Warning { message: format!("sync failed: {e}") });
                    next_try = now + Duration::from_secs(10);
                }
            }
        }

        if now >= next_collect {
            let has_open = jobs::local_jobs(&p).iter().any(|j| !j.finished() || !j.pulled_back);
            if has_open && !dirty {
                if let Err(e) = jobs::collect(&p, rep.as_ref()) {
                    if !e.is_transient() {
                        rep.event(Event::Warning { message: format!("collecting job output: {e}") });
                    }
                }
            }
            next_collect = now + Duration::from_secs(2);
        }
    }
}
