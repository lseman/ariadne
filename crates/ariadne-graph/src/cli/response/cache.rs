//! Process-lifetime graph cache for long-lived servers (mcp-server, serve).
//!
//! Not used by the one-shot `ariadne tool` CLI path — see `tool_response_cached`
//! vs `tool_response` in mod.rs.

use ariadne_graph::store::Store;
use ariadne_graph::Graph;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// Fingerprint of the on-disk DB state a cached graph was loaded from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DbFingerprint {
    main_mtime: Option<SystemTime>,
    main_len: u64,
    wal_mtime: Option<SystemTime>,
    wal_len: u64,
}

impl DbFingerprint {
    fn capture(db_path: &Path) -> Self {
        let main = std::fs::metadata(db_path).ok();
        let wal_path = wal_sidecar_path(db_path);
        let wal = std::fs::metadata(&wal_path).ok();
        Self {
            main_mtime: main.as_ref().and_then(|m| m.modified().ok()),
            main_len: main.map(|m| m.len()).unwrap_or(0),
            wal_mtime: wal.as_ref().and_then(|m| m.modified().ok()),
            wal_len: wal.map(|m| m.len()).unwrap_or(0),
        }
    }
}

fn wal_sidecar_path(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_owned();
    os.push("-wal");
    PathBuf::from(os)
}

struct CachedGraph {
    db_path: PathBuf,
    fingerprint: DbFingerprint,
    graph: Graph,
}

static CACHE: OnceLock<Mutex<Option<CachedGraph>>> = OnceLock::new();

fn cache_slot() -> &'static Mutex<Option<CachedGraph>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Load the graph for `db_path`, reusing the process-lifetime cache when the
/// on-disk DB is unchanged since it was last cached. Opens a fresh `Store`
/// every call (cheap: just a SQLite connection open) but skips `Store::load`
/// (the expensive full table scan) when the fingerprint matches.
pub fn load_cached(db_path: &Path, store: &Store) -> Result<Graph> {
    let fingerprint = DbFingerprint::capture(db_path);
    let mut slot = cache_slot().lock().unwrap();

    if let Some(cached) = slot.as_ref() {
        if cached.db_path == db_path && cached.fingerprint == fingerprint {
            return Ok(cached.graph.clone());
        }
    }

    let graph = store.load()?;
    *slot = Some(CachedGraph {
        db_path: db_path.to_path_buf(),
        fingerprint,
        graph: graph.clone(),
    });
    Ok(graph)
}
