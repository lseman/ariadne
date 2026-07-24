//! cmd_paths, cmd_callers, cmd_callees, cmd_impact, cmd_detect_changes, cmd_review_context, cmd_traverse, cmd_motifs.

use anyhow::Result;
use ariadne_graph::query::{analyze_impact, callees_of, callers_of, ImpactQuery};
use ariadne_graph::store::Store;
use std::path::Path;

pub fn cmd_motifs(db: &Path, built_in: &str, limit: usize) -> Result<()> {
    let report = super::response::motifs_json(db, built_in, limit)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn cmd_paths(
    db: &Path,
    from: &str,
    to: &str,
    max_hops: usize,
    top: usize,
    structural_only: bool,
) -> Result<()> {
    use super::helpers::resolve;
    use ariadne_graph::query::paths::PathQuery;

    let store = Store::open(db)?;
    let graph = store.load()?;
    let from_id = resolve(&graph, from)?;
    let to_id = resolve(&graph, to)?;
    let mut q = PathQuery::between(from_id, to_id, max_hops);
    if structural_only {
        q = q.with_min_confidence(1.0);
    }
    let paths = ariadne_graph::query::find_top_paths(&graph, &q, top);
    if paths.is_empty() {
        println!("no paths found");
        return Ok(());
    }
    println!("found {} ranked path(s):", paths.len());
    for (i, p) in paths.iter().enumerate() {
        println!("  path {} (cost {:.3}):", i + 1, p.cost);
        for n in &p.nodes {
            if let Some(node) = graph.node(*n) {
                println!("    -> {} ({:?})", node.qualified_name, node.kind);
            }
        }
    }
    Ok(())
}

pub fn cmd_callers(db: &Path, target: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let id = super::helpers::resolve(&graph, target)?;
    let callers = callers_of(&graph, id);
    println!("callers of {} ({} total):", target, callers.len());
    for c in callers {
        if let Some(n) = graph.node(c) {
            println!("  {}", n.qualified_name);
        }
    }
    Ok(())
}

pub fn cmd_callees(db: &Path, source: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let id = super::helpers::resolve(&graph, source)?;
    let callees = callees_of(&graph, id);
    println!("callees of {} ({} total):", source, callees.len());
    for c in callees {
        if let Some(n) = graph.node(c) {
            println!("  {}", n.qualified_name);
        }
    }
    Ok(())
}

pub fn cmd_impact(db: &Path, target: &str, max_hops: usize, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let seed = super::helpers::resolve(&graph, target)?;
    let hits = analyze_impact(
        &graph,
        ImpactQuery {
            seed,
            max_hops,
            limit: top,
        },
    );
    println!("impact of {} ({} result(s)):", target, hits.len());
    for hit in hits {
        if let Some(n) = graph.node(hit.id) {
            let via: Vec<_> = hit.via.iter().map(|kind| format!("{:?}", kind)).collect();
            println!(
                "  {:.4}  hops={}  {}  ({:?})  [{}]",
                hit.score,
                hit.distance,
                n.qualified_name,
                n.kind,
                via.join(" <- ")
            );
        }
    }
    Ok(())
}

pub fn cmd_detect_changes(db: &Path, base: &str, max_depth: usize, brief: bool) -> Result<()> {
    let analysis = super::response::detect_changes_json(db, base, max_depth)?;
    if brief {
        println!("{}", serde_json::to_string_pretty(&analysis)?);
        return Ok(());
    }
    println!(
        "change risk: {} ({:.2})",
        analysis["risk"], analysis["risk_score"]
    );
    println!("changed files:");
    for file in analysis["changed_files"].as_array().unwrap_or(&Vec::new()) {
        println!("  {}", file.as_str().unwrap_or(""));
    }
    if let Some(nodes) = analysis["changed_symbols"].as_array() {
        if !nodes.is_empty() {
            println!("changed symbols:");
            for node in nodes.iter().take(12) {
                let loc = match (
                    node["line_start"].as_u64(),
                    node["line_end"].as_u64(),
                    node["source_uri"].as_str(),
                ) {
                    (Some(start), Some(end), Some(src)) => format!("{}:{}-{}", src, start, end),
                    (_, _, Some(src)) => src.to_string(),
                    _ => String::new(),
                };
                println!(
                    "  {}  {}",
                    node["qualified_name"].as_str().unwrap_or(""),
                    loc
                );
            }
        }
    }
    println!("top impacted:");
    for hit in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "  {:.3}  {}",
            hit["score"].as_f64().unwrap_or_default(),
            hit["qualified_name"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

pub fn cmd_review_context(
    db: &Path,
    base: &str,
    max_lines_per_file: usize,
    token_budget: usize,
) -> Result<()> {
    let context = super::response::review_context_json(db, base, max_lines_per_file, token_budget)?;
    println!("{}", serde_json::to_string_pretty(&context)?);
    Ok(())
}

pub fn cmd_traverse(
    db: &Path,
    target: &str,
    direction: &str,
    max_depth: usize,
    token_budget: usize,
) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let seed = super::helpers::resolve(&graph, target)?;
    let out = super::response::traverse_json(&graph, seed, direction, max_depth, token_budget);
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
