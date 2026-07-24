//! cmd_god_nodes, cmd_communities, cmd_dedup, cmd_flows, cmd_affected_flows,
//! cmd_blast_radius, cmd_test_coverage.

use anyhow::{bail, Result};
use ariadne_graph::query::{deduplicate_nodes, pagerank, personalized_pagerank, DedupOptions};
use ariadne_graph::query::{infomap, leiden, louvain};
use ariadne_graph::store::Store;
use ariadne_graph::{EdgeKind, NodeKind};
use serde_json::json;
use std::path::Path;

pub fn cmd_god_nodes(db: &Path, top: usize, seed: Option<&str>) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;

    let ranks = if let Some(seed) = seed {
        let seed_id = super::helpers::resolve(&graph, seed)?;
        personalized_pagerank(&graph, &[(seed_id, 1.0)], 0.85, 50)
    } else {
        pagerank(&graph, 0.85, 50)
    };
    let mut sorted: Vec<_> = ranks.iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(seed) = seed {
        println!(
            "top {} god-nodes by personalized pagerank from {}:",
            top, seed
        );
    } else {
        println!("top {} god-nodes by weighted pagerank:", top);
    }
    for (id, rank) in sorted
        .iter()
        .filter(|(id, _)| {
            graph
                .node(**id)
                .map(|n| !ariadne_graph::query::is_rank_noise(n))
                .unwrap_or(false)
        })
        .take(top)
    {
        if let Some(n) = graph.node(**id) {
            println!("  {:.6}  {}  ({:?})", rank, n.qualified_name, n.kind);
        }
    }
    Ok(())
}

pub fn cmd_communities(db: &Path, top: usize, algorithm: &str) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let comm = match algorithm {
        "louvain" => louvain(&graph),
        "leiden" => leiden(&graph),
        "infomap" => infomap(&graph),
        other => bail!(
            "unknown community algorithm {}; use louvain, leiden, or infomap",
            other
        ),
    };
    let mut by_comm: std::collections::BTreeMap<usize, Vec<String>> =
        std::collections::BTreeMap::new();
    for (id, &c) in &comm {
        if let Some(n) = graph.node(*id) {
            by_comm.entry(c).or_default().push(n.qualified_name.clone());
        }
    }
    let mut entries: Vec<_> = by_comm.into_iter().collect();
    entries.sort_by_key(|(_, members)| std::cmp::Reverse(members.len()));
    println!(
        "detected {} {} communities (showing top {}):",
        entries.len(),
        algorithm,
        top
    );
    for (c, members) in entries.into_iter().take(top) {
        println!("  community {} ({} members):", c, members.len());
        for m in members.iter().take(5) {
            println!("    {}", m);
        }
        if members.len() > 5 {
            println!("    ... and {} more", members.len() - 5);
        }
    }
    Ok(())
}

pub fn cmd_dedup(
    db: &Path,
    threshold: f32,
    community_boost: f32,
    community_algo: Option<String>,
) -> Result<()> {
    let mut store = Store::open(db)?;
    let graph = store.load()?;

    let options = DedupOptions {
        jw_threshold: threshold,
        community_boost,
        ..Default::default()
    };

    let communities = if let Some(algo) = community_algo {
        match algo.as_str() {
            "louvain" => {
                let comm = ariadne_graph::query::louvain(&graph);
                Some(comm)
            }
            "leiden" => {
                let comm = ariadne_graph::query::leiden(&graph);
                Some(comm)
            }
            _ => {
                println!(
                    "unknown algorithm {}; running without community boost",
                    algo
                );
                None
            }
        }
    } else {
        None
    };

    let mut mutable_graph = graph;
    let result = deduplicate_nodes(
        &mut mutable_graph,
        &communities.unwrap_or_default(),
        Some(options),
    );

    println!(
        "dedup: {} candidates examined, {} merges, {} nodes removed, {} edges re-wired",
        result.candidates_examined, result.merges, result.nodes_removed, result.edges_rewired
    );

    // Save the deduplicated graph back
    store.save(&mutable_graph)?;

    Ok(())
}

pub fn cmd_flows(db: &Path, top: usize) -> Result<()> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let flows = ariadne_graph::extract::flows::all_flows(&graph);
    println!("detected {} flows (showing top {}):", flows.len(), top);
    for flow_id in flows.into_iter().take(top) {
        if let Some(node) = graph.node(flow_id) {
            let entry = node
                .properties
                .get("entry_qualified_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let crit = node
                .properties
                .get("criticality")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let size = node
                .properties
                .get("node_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let depth = node
                .properties
                .get("depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let is_test = node
                .properties
                .get("is_test_flow")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let tag = if is_test { " [test]" } else { "" };
            println!(
                "  crit={:.2} size={:>3} depth={} {}{}",
                crit, size, depth, entry, tag
            );
        }
    }
    Ok(())
}

pub fn cmd_affected_flows(db: &Path, base: &str, top: usize) -> Result<()> {
    let analysis = super::response::detect_changes_json(db, base, 2)?;
    let hits = analysis["affected_flows"]["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let total = analysis["affected_flows"]["total"].as_u64().unwrap_or(0);
    println!(
        "{} flow(s) affected by changes since {} (showing top {}):",
        total, base, top
    );
    for hit in hits.into_iter().take(top) {
        let entry = hit["entry_qualified_name"].as_str().unwrap_or("?");
        let crit = hit["criticality"].as_f64().unwrap_or(0.0);
        let size = hit["node_count"].as_u64().unwrap_or(0);
        let is_test = hit["is_test_flow"].as_bool().unwrap_or(false);
        let tag = if is_test { " [test]" } else { "" };
        println!("  crit={:.2} size={:>3} {}{}", crit, size, entry, tag);
    }
    Ok(())
}

pub fn cmd_blast_radius(db: &Path, base: &str, max_depth: usize, top: usize) -> Result<()> {
    let analysis = super::response::detect_changes_json(db, base, max_depth)?;
    let risk = analysis
        .get("risk")
        .and_then(|r| r.as_str())
        .unwrap_or("unknown");
    let risk_score = analysis
        .get("risk_score")
        .and_then(|r| r.as_f64())
        .unwrap_or(0.0);
    let changed_files_arr = analysis["changed_files"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let changed_files: Vec<&str> = changed_files_arr
        .iter()
        .filter_map(|v| v.as_str())
        .take(top)
        .collect();
    let changed_symbols_arr = analysis["changed_symbols"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let changed_symbols: Vec<(&str, &str)> = changed_symbols_arr
        .iter()
        .filter_map(|v| {
            Some((
                v.get("qualified_name").and_then(|n| n.as_str())?,
                v.get("kind").and_then(|k| k.as_str()).unwrap_or("?"),
            ))
        })
        .take(top)
        .collect();
    let impacted_arr = analysis["impacted"].as_array().cloned().unwrap_or_default();
    let impacted: Vec<(&str, f64)> = impacted_arr
        .iter()
        .filter_map(|v| {
            Some((
                v.get("qualified_name").and_then(|n| n.as_str())?,
                v.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0),
            ))
        })
        .take(top)
        .collect();
    let test_cov = analysis
        .get("test_coverage")
        .cloned()
        .unwrap_or_else(|| json!({"covered": [], "missing": [], "missing_count": 0}));
    let missing_count = test_cov
        .get("missing_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    println!("blast radius: {} ({:.2})", risk, risk_score);
    println!("changed files ({}):", changed_files.len());
    for f in &changed_files {
        println!("  {}", f);
    }
    println!("changed symbols ({}):", changed_symbols.len());
    for (name, kind) in &changed_symbols {
        println!("  {} ({})", name, kind);
    }
    println!("impacted nodes ({}):", impacted.len());
    for (name, score) in &impacted {
        println!("  {:.3}  {}", score, name);
    }
    println!("test coverage: {} missing", missing_count);
    Ok(())
}

pub fn cmd_test_coverage(db: &Path, base: Option<&str>, target: Option<&str>) -> Result<()> {
    let result = match (base, target) {
        (Some(base), _) => {
            let analysis = super::response::detect_changes_json(db, base, 2)?;
            analysis
                .get("test_coverage")
                .cloned()
                .unwrap_or_else(|| json!({"covered": [], "missing": [], "missing_count": 0}))
        }
        (_, Some(target)) => {
            let store = Store::open(db)?;
            let graph = store.load()?;
            let seed = super::helpers::resolve(&graph, target)?;
            let mut covered = Vec::new();
            let mut missing = Vec::new();
            if let Some(node) = graph.node(seed) {
                if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                    let tests: Vec<serde_json::Value> = graph
                        .out_neighbors(seed)
                        .filter(|(_, edge)| edge.kind == EdgeKind::TestedBy)
                        .filter_map(|(test_id, _)| {
                            graph.node(test_id).map(|t| {
                                json!({
                                    "id": test_id.0,
                                    "qualified_name": t.qualified_name,
                                    "source_uri": t.source_uri,
                                })
                            })
                        })
                        .collect();
                    let entry = json!({
                        "id": seed.0,
                        "qualified_name": node.qualified_name,
                        "kind": node.kind,
                        "source_uri": node.source_uri,
                        "tests": tests,
                    });
                    if tests.is_empty() {
                        missing.push(entry);
                    } else {
                        covered.push(entry);
                    }
                }
            }
            json!({
                "target": target,
                "covered": covered,
                "missing": missing,
                "missing_count": missing.len(),
            })
        }
        _ => json!({"covered": [], "missing": [], "missing_count": 0}),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
