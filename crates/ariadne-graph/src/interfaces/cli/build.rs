//! cmd_build, cmd_update, stamp_valid_from, run_dedup_on_graph.

use anyhow::Result;
use ariadne_graph::core::NodeKind;
use ariadne_graph::extract::{
    extract_directory, extract_file, resolve_call_placeholders, resolve_mentions,
};
use ariadne_graph::query::{deduplicate_nodes, DedupOptions};
use ariadne_graph::store::Store;
use ariadne_graph::Graph;
use std::collections::HashMap;
use std::path::Path;

use super::git::{collect_file_hashes, git_commit_hash};

/// Stamp every node and edge that lacks a `valid_from` with the given
/// commit SHA, marking them as introduced at that commit. Idempotent:
/// rows that already carry a `valid_from` are left untouched.
pub fn stamp_valid_from(graph: &mut Graph, commit: &str) {
    let node_ids: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| n.valid_from.is_none())
        .map(|(id, _)| id)
        .collect();
    for id in node_ids {
        if let Some(node) = graph.node_mut(id) {
            node.valid_from = Some(commit.to_string());
        }
    }
    let edge_ids: Vec<_> = graph
        .edges()
        .filter(|(_, _, _, e)| e.valid_from.is_none())
        .map(|(id, _, _, _)| id)
        .collect();
    for id in edge_ids {
        if let Some(edge) = graph.edge_mut(id) {
            edge.valid_from = Some(commit.to_string());
        }
    }
}

/// Build the graph from a directory of source files.
pub fn cmd_build(db: &Path, path: &Path) -> Result<()> {
    let mut graph = Graph::new();
    tracing::info!("extracting from {}", path.display());
    let n = extract_directory(path, &mut graph)?;
    tracing::info!(
        "extracted {} files: {} nodes, {} edges",
        n,
        graph.node_count(),
        graph.edge_count()
    );
    // Stamp active rows with HEAD so temporal diffs have a baseline.
    if let Some(head) = git_commit_hash("HEAD")? {
        stamp_valid_from(&mut graph, &head);
    }
    // Deduplicate semantically equivalent concept nodes.
    let dedup_result = run_dedup_on_graph(&mut graph, None);
    tracing::info!(
        dedup_candidates = dedup_result.candidates_examined,
        dedup_merges = dedup_result.merges,
        dedup_removed = dedup_result.nodes_removed,
        dedup_rewired = dedup_result.edges_rewired,
        "deduplication complete"
    );
    let mut store = Store::open(db)?;
    store.save(&graph)?;
    let hashes = collect_file_hashes(path)?;
    store.set_file_hashes(&hashes)?;
    println!(
        "graph built: {} nodes, {} edges -> {}",
        graph.node_count(),
        graph.edge_count(),
        db.display()
    );
    Ok(())
}

/// Incrementally update the graph from changed files.
pub fn cmd_update(db: &Path, path: &Path) -> Result<()> {
    let current = collect_file_hashes(path)?;
    let current_map: HashMap<String, String> = current.iter().cloned().collect();
    let mut store = Store::open(db)?;
    let previous = store.file_hashes()?;

    let changed: Vec<String> = current
        .iter()
        .filter(|(p, h)| previous.get(p) != Some(h))
        .map(|(p, _)| p.clone())
        .collect();
    let deleted: Vec<String> = previous
        .keys()
        .filter(|p| !current_map.contains_key(*p))
        .cloned()
        .collect();

    if changed.is_empty() && deleted.is_empty() {
        println!("graph already up to date");
        return Ok(());
    }

    let mut stale = changed.clone();
    stale.extend(deleted.iter().cloned());

    // Archive the rows about to be removed so a temporal diff can still
    // see their pre-change state: close them out at HEAD (valid_to). Also
    // remember each symbol's original birth commit so survivors keep it
    // rather than looking newly-introduced after re-extraction.
    let head = git_commit_hash("HEAD")?;
    let mut original_valid_from: HashMap<String, String> = HashMap::new();
    if let Some(head) = head.as_deref() {
        let old_nodes = store.active_nodes_for_sources(&stale)?;
        let old_edges = store.active_edges_for_sources(&stale)?;
        for row in &old_nodes {
            if let Some(vf) = &row.node.valid_from {
                original_valid_from.insert(row.node.qualified_name.clone(), vf.clone());
            }
        }
        store.archive_nodes(&old_nodes, head)?;
        store.archive_edges(&old_edges, head)?;
    }

    store.delete_sources(&stale)?;

    let mut graph = store.load()?;
    for source in &changed {
        let file = Path::new(source);
        if file.exists() {
            extract_file(file, &mut graph)?;
        }
    }
    resolve_call_placeholders(&mut graph);
    resolve_mentions(&mut graph);
    if let Some(head) = head.as_deref() {
        // Survivors keep their original birth commit; genuinely-new
        // symbols are stamped at HEAD.
        let carry: Vec<_> = graph
            .nodes()
            .filter(|(_, n)| n.valid_from.is_none())
            .filter_map(|(id, n)| {
                original_valid_from
                    .get(&n.qualified_name)
                    .map(|vf| (id, vf.clone()))
            })
            .collect();
        for (id, vf) in carry {
            if let Some(node) = graph.node_mut(id) {
                node.valid_from = Some(vf);
            }
        }
        stamp_valid_from(&mut graph, head);
    }
    // Deduplicate semantically equivalent concept nodes.
    let dedup_result = run_dedup_on_graph(&mut graph, None);
    tracing::info!(
        dedup_candidates = dedup_result.candidates_examined,
        dedup_merges = dedup_result.merges,
        dedup_removed = dedup_result.nodes_removed,
        dedup_rewired = dedup_result.edges_rewired,
        "deduplication complete"
    );
    store.save(&graph)?;
    store.set_file_hashes(&current)?;

    println!(
        "graph updated: {} changed, {} deleted, {} nodes, {} edges",
        changed.len(),
        deleted.len(),
        graph.node_count(),
        graph.edge_count()
    );
    Ok(())
}

/// Run dedup on the given graph using the specified community algorithm
/// (or Leiden by default). Returns a `DedupResult` summary.
pub fn run_dedup_on_graph(
    graph: &mut Graph,
    community_algo: Option<&str>,
) -> ariadne_graph::query::dedup::DedupResult {
    use std::collections::HashMap;

    // Dedup only touches Concept/Document/Section/Diagram/Image/Hyperedge.
    // Skip entirely when there are too few eligible nodes.
    let eligible_kinds = [
        NodeKind::Concept,
        NodeKind::Document,
        NodeKind::Section,
        NodeKind::Diagram,
        NodeKind::Image,
        NodeKind::Hyperedge,
    ];
    let eligible_count = graph
        .nodes()
        .filter(|(_, n)| eligible_kinds.contains(&n.kind))
        .count();
    if eligible_count < 2 {
        return ariadne_graph::query::dedup::DedupResult {
            candidates_examined: 0,
            merges: 0,
            nodes_removed: 0,
            edges_rewired: 0,
        };
    }

    // Compute communities for the boost pass.
    let communities: HashMap<ariadne_graph::core::NodeId, usize> = match community_algo {
        Some("louvain") => ariadne_graph::query::louvain(graph),
        Some("infomap") => ariadne_graph::query::infomap(graph),
        _ => ariadne_graph::query::leiden(graph),
    };

    let options = DedupOptions::default();
    deduplicate_nodes(graph, &communities, Some(options))
}
