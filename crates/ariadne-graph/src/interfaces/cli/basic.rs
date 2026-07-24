//! cmd_status, cmd_rebuild_fts, cmd_embed, cmd_tui, cmd_graph_diff.

use anyhow::Result;
use ariadne_graph::store::Store;
use std::path::Path;

pub fn cmd_status(db: &Path) -> Result<()> {
    let store = Store::open(db)?;
    let (n, e) = store.stats()?;
    println!("ariadne db: {}", db.display());
    println!("  nodes: {}", n);
    println!("  edges: {}", e);
    Ok(())
}

pub fn cmd_rebuild_fts(db: &Path) -> Result<()> {
    let mut store = Store::open(db)?;
    let indexed = store.rebuild_fts_index()?;
    println!("rebuilt FTS5 index: {} nodes", indexed);
    Ok(())
}

pub fn cmd_embed(db: &Path, model: &str) -> Result<()> {
    let mut store = Store::open(db)?;
    let count = store.rebuild_embeddings(model)?;
    println!("built {} embeddings with model {}", count, model);
    Ok(())
}

pub fn cmd_tui(db: &Path) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    ariadne_graph::tui::run(&store, &graph)
}

pub fn cmd_graph_diff(db: &Path, base: &str, head: &str, top: usize) -> Result<()> {
    let report = super::response::graph_diff_json(db, base, head, top)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
