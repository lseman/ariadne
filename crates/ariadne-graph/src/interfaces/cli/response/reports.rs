use anyhow::Result;
use ariadne_graph::query::{bridge_scores, leiden, pagerank};
use ariadne_graph::store::Store;
use std::collections::BTreeMap;
use std::path::Path;

use super::analysis::{diagnostics_json, gaps_json, surprises_json};
use super::architecture::architecture_overview_json;
use super::DetailLevel;

/// Generate a Markdown report from the graph and diagnostics.
pub fn generate_report_markdown(db: &Path, top: usize) -> Result<String> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let diag = diagnostics_json(db, top)?;
    let arch = architecture_overview_json(&graph, DetailLevel::Standard);

    // God nodes
    let ranks = pagerank(&graph, 0.85, 50);
    let mut sorted: Vec<_> = ranks.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_nodes: Vec<String> = sorted
        .into_iter()
        .take(top)
        .filter_map(|(id, _)| graph.node(id).map(|n| n.qualified_name.clone()))
        .collect();

    // Bridges
    let communities = leiden(&graph);
    let bridges = bridge_scores(&graph, &communities, top);
    let top_bridges: Vec<String> = bridges
        .into_iter()
        .filter_map(|bs| {
            graph
                .node(bs.node)
                .map(|n| format!("{} ({:.4})", n.qualified_name, bs.score))
        })
        .take(10)
        .collect();

    // Gaps
    let gap_rows = gaps_json(&graph, top);
    let top_gaps: Vec<String> = gap_rows
        .get("hits")
        .and_then(|h| h.as_array())
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h["qualified_name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Surprises
    let surprise_rows = surprises_json(&graph, top);
    let top_surprises: Vec<String> = surprise_rows
        .get("hits")
        .and_then(|h| h.as_array())
        .map(|hits| {
            hits.iter()
                .map(|h| {
                    let src = h
                        .get("src")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let dst = h
                        .get("dst")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let score = h.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    format!("{src} ↔ {dst} ({score:.2})")
                })
                .collect()
        })
        .unwrap_or_default();

    // Communities
    let mut by_comm: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (id, &c) in &communities {
        if let Some(n) = graph.node(*id) {
            by_comm.entry(c).or_default().push(n.qualified_name.clone());
        }
    }
    let mut entries: Vec<_> = by_comm.into_iter().collect();
    entries.sort_by_key(|(_, members)| std::cmp::Reverse(members.len()));
    let community_lines: Vec<String> = entries
        .iter()
        .take(10)
        .map(|(c, members)| {
            format!(
                "- **Community {}** ({} members): {}",
                c,
                members.len(),
                members
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();

    // Build markdown
    let mut md = String::new();
    md.push_str("# Ariadne Graph Report\n\n");
    md.push_str(&format!("Generated from: `{}`\n\n", db.display()));

    // Health
    md.push_str("## Health\n\n");
    if let Some(health) = diag.get("health") {
        md.push_str(&format!(
            "- **Status**: {}\n",
            health.as_str().unwrap_or("unknown")
        ));
    }
    if let Some(warnings) = diag.get("warnings").and_then(|w| w.as_array()) {
        for w in warnings {
            let kind = w.get("kind").and_then(|k| k.as_str()).unwrap_or("unknown");
            let msg = w.get("message").and_then(|m| m.as_str()).unwrap_or("");
            md.push_str(&format!("- ⚠️ **{}**: {}\n", kind, msg));
        }
    }
    md.push('\n');

    // Index coverage
    md.push_str("## Index Coverage\n\n");
    if let Some(ic) = diag.get("index_coverage") {
        if let Some(fts) = ic.get("fts5") {
            md.push_str(&format!(
                "- **FTS5**: {} nodes indexed\n",
                fts.as_str().unwrap_or("?")
            ));
        }
        if let Some(embed) = ic.get("embedding") {
            md.push_str(&format!(
                "- **Embedding**: {} vectors\n",
                embed.as_str().unwrap_or("?")
            ));
        }
    }
    md.push('\n');

    // Confidence
    md.push_str("## Confidence Mix\n\n");
    if let Some(cm) = diag.get("confidence_mix") {
        for key in &["extracted", "inferred", "ambiguous"] {
            if let Some(val) = cm.get(key).and_then(|v| v.as_u64()) {
                md.push_str(&format!("- **{}**: {}\n", key, val));
            }
        }
    }
    md.push('\n');

    // Call resolution
    md.push_str("## Call Resolution\n\n");
    if let Some(cr) = diag.get("call_resolution") {
        if let Some(resolved) = cr.get("resolved").and_then(|v| v.as_u64()) {
            md.push_str(&format!("- Resolved: {}\n", resolved));
        }
        if let Some(unresolved) = cr.get("unresolved").and_then(|v| v.as_u64()) {
            md.push_str(&format!("- Unresolved: {}\n", unresolved));
        }
        if let Some(rate) = cr.get("rate").and_then(|v| v.as_f64()) {
            md.push_str(&format!("- Rate: {:.1}%\n\n", rate * 100.0));
        }
    }

    // Architecture overview
    md.push_str("## Architecture\n\n");
    if let Some(overview) = arch.get("summary") {
        md.push_str(&format!("{}\n\n", overview.as_str().unwrap_or("")));
    }
    if let Some(comm) = arch.get("community_count").and_then(|v| v.as_u64()) {
        md.push_str(&format!("Communities: {}\n\n", comm));
    }

    // Communities
    md.push_str("## Top Communities\n\n");
    for line in &community_lines {
        md.push_str(&format!("{}\n", line));
    }
    if community_lines.is_empty() {
        md.push_str("_No communities detected._\n");
    }
    md.push('\n');

    // God nodes
    md.push_str("## Top Nodes (PageRank)\n\n");
    if top_nodes.is_empty() {
        md.push_str("_No nodes ranked._\n");
    } else {
        for (i, name) in top_nodes.iter().enumerate().take(15) {
            md.push_str(&format!("{}. {}\n", i + 1, name));
        }
    }
    md.push('\n');

    // Bridges
    md.push_str("## Bridge Nodes\n\n");
    if top_bridges.is_empty() {
        md.push_str("_No significant bridges._\n");
    } else {
        for b in &top_bridges {
            md.push_str(&format!("- {}\n", b));
        }
    }
    md.push('\n');

    // Gaps
    md.push_str("## Gaps\n\n");
    if top_gaps.is_empty() {
        md.push_str("_No gaps detected._\n");
    } else {
        for g in &top_gaps {
            md.push_str(&format!("- {}\n", g));
        }
    }
    md.push('\n');

    // Surprises
    md.push_str("## Surprises\n\n");
    if top_surprises.is_empty() {
        md.push_str("_No surprises._\n");
    } else {
        for s in &top_surprises {
            md.push_str(&format!("- {}\n", s));
        }
    }
    md.push('\n');

    // Nodes/edges summary
    md.push_str("## Summary\n\n");
    md.push_str(&format!("- **Nodes**: {}\n", graph.node_count()));
    md.push_str(&format!("- **Edges**: {}\n", graph.edge_count()));

    Ok(md)
}
