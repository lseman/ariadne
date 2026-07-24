//! cmd_search.

use anyhow::Result;
use ariadne_graph::store::Store;
use std::path::Path;

pub fn cmd_search(db: &Path, query: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let results = ariadne_graph::query::ranked_search(&graph, query, 50);
    println!("found {} result(s):", results.len());
    for hit in results.iter().take(50) {
        if let Some(n) = graph.node(hit.id) {
            println!(
                "  {:.2}  {}  ({:?})  {}  [{}]",
                hit.score,
                n.qualified_name,
                n.kind,
                n.source_uri.as_deref().unwrap_or(""),
                hit.signals.join(",")
            );
        }
    }
    Ok(())
}
