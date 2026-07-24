//! cmd_watch, watch_event_driven, WATCH_DEBOUNCE, Never.

use anyhow::{bail, Result};
use ariadne_graph::extract::{ignore_set, is_relevant_source, IgnoreSet};
use notify::{RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use super::build;
use std::path::Path;

/// Quiet period after the first file event before running an update, so
/// editor save bursts and branch switches collapse into one rebuild.
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);

/// Never-returning success type: the event loop only exits by error.
pub enum Never {}

/// Watch a path and incrementally update when supported files change.
///
/// Uses OS file events (inotify / FSEvents / ReadDirectoryChangesW) with
/// a short debounce. Falls back to polling every `interval` seconds when
/// a watcher cannot be created (e.g. network filesystems).
pub fn cmd_watch(db: &Path, path: &Path, interval: u64) -> Result<()> {
    let err = match watch_event_driven(db, &[path.to_path_buf()]) {
        Err(e) => e,
        Ok(never) => match never {},
    };
    tracing::warn!(
        "file watcher unavailable ({}); falling back to polling",
        err
    );
    println!(
        "watching {} for Ariadne graph updates every {}s (polling)",
        path.display(),
        interval
    );
    loop {
        build::cmd_update(db, path)?;
        std::thread::sleep(Duration::from_secs(interval.max(1)));
    }
}

/// Watch every root with one OS watcher and run `cmd_update` on the
/// roots whose relevant files changed, after a debounce window.
pub fn watch_event_driven(db: &Path, roots: &[PathBuf]) -> Result<Never> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx)?;
    let mut watched: Vec<(PathBuf, IgnoreSet)> = Vec::new();
    for root in roots {
        let root = super::git::absolute_path(root)?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        let ignore = ignore_set(&root);
        watched.push((root, ignore));
    }
    println!(
        "watching {} path(s) for file events (debounce {}ms)",
        watched.len(),
        WATCH_DEBOUNCE.as_millis()
    );

    // Catch up on anything that changed while no watcher was running.
    for (root, _) in &watched {
        if let Err(e) = build::cmd_update(db, root) {
            tracing::warn!("initial update failed for {}: {}", root.display(), e);
        }
    }

    let mark_dirty = |event: &notify::Result<notify::Event>,
                      dirty: &mut std::collections::HashSet<usize>| {
        let Ok(event) = event else { return };
        for path in &event.paths {
            for (idx, (root, ignore)) in watched.iter().enumerate() {
                if path.starts_with(root) && is_relevant_source(root, path, ignore) {
                    dirty.insert(idx);
                }
            }
        }
    };

    loop {
        // Block until something happens, then drain events until the
        // filesystem has been quiet for a full debounce window.
        let first = rx
            .recv()
            .map_err(|_| anyhow::anyhow!("watch channel closed"))?;
        let mut dirty = std::collections::HashSet::new();
        mark_dirty(&first, &mut dirty);
        loop {
            match rx.recv_timeout(WATCH_DEBOUNCE) {
                Ok(event) => mark_dirty(&event, &mut dirty),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("watch channel closed")
                }
            }
        }
        for idx in dirty {
            let (root, _) = &watched[idx];
            if let Err(e) = build::cmd_update(db, root) {
                tracing::warn!("update failed for {}: {}", root.display(), e);
            }
        }
    }
}
