use ariadne_graph::query::{temporal_diff, TemporalDiff, analyze_impact, ImpactQuery};
use ariadne_graph::{Graph, NodeId, NodeKind};
use ariadne_graph::store::Store;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::git::{git_changed_diff, git_commit_hash, git_is_ancestor, ChangedFile};
use super::helpers::{nodes_for_changed_hunk, nodes_for_changed_ranges, nodes_for_files};
use ariadne_graph::extract::flows::affected_flows;

/// Risk-scored change analysis from a git diff base.
pub fn detect_changes_json(db: &Path, base: &str, max_depth: usize) -> Result<Value> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let (changed_files, changed_nodes, changed_ranges, mapping_precision, temporal) =
        if let Some(head) = git_commit_hash("HEAD")? {
            if graph_has_temporal_data(&graph) {
                if let Some(base_hash) = git_commit_hash(base)? {
                    let diff = git_changed_diff(base).unwrap_or_default();
                    let mut cache = HashMap::new();
                    let temporal =
                        temporal_diff(&graph, &base_hash, &head, &mut |ancestor, descendant| {
                            *cache
                                .entry((ancestor.to_string(), descendant.to_string()))
                                .or_insert_with(|| git_is_ancestor(ancestor, descendant))
                        });
                    let changed_nodes = temporal.changed_nodes();
                    let changed_files = changed_nodes
                        .iter()
                        .filter_map(|id| graph.node(*id).and_then(|n| n.source_uri.clone()))
                        .collect::<HashSet<_>>();
                    let mut changed_files: Vec<String> = changed_files.into_iter().collect();
                    changed_files.sort();
                    let mapping_precision = if diff.iter().any(|f| !f.hunks.is_empty()) {
                        "temporal+line_ranges"
                    } else {
                        "temporal"
                    };
                    (
                        changed_files,
                        changed_nodes,
                        changed_ranges_json(&graph, &diff),
                        mapping_precision.to_string(),
                        Some(temporal),
                    )
                } else {
                    old_changed_diff(&graph, base)
                }
            } else {
                old_changed_diff(&graph, base)
            }
        } else {
            old_changed_diff(&graph, base)
        };
    let mut impacted = Vec::new();
    let mut seen = HashSet::new();
    for seed in &changed_nodes {
        for hit in analyze_impact(
            &graph,
            ImpactQuery {
                seed: *seed,
                max_hops: max_depth,
                limit: 8,
            },
        ) {
            if seen.insert(hit.id) {
                if let Some(n) = graph.node(hit.id) {
                    impacted.push(json!({
                        "id": hit.id.0,
                        "score": hit.score,
                        "distance": hit.distance,
                        "qualified_name": n.qualified_name,
                        "kind": n.kind,
                        "source_uri": n.source_uri,
                    }));
                }
            }
        }
    }
    impacted.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .partial_cmp(&a["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    impacted.truncate(25);
    let test_coverage = test_coverage_json(&graph, &changed_nodes);
    let missing_count = test_coverage["missing_count"].as_u64().unwrap_or(0) as usize;
    let affected_flows = affected_flows_json(&graph, &changed_nodes, 10);
    let top_flow_criticality = affected_flows["hits"]
        .as_array()
        .and_then(|hits| hits.first())
        .and_then(|f| f["criticality"].as_f64())
        .unwrap_or(0.0);
    let score = risk_score(
        &graph,
        &changed_nodes,
        impacted.len(),
        missing_count,
        top_flow_criticality,
    );
    Ok(json!({
        "operation": "detect_changes",
        "base": base,
        "changed_files": changed_files,
        "changed_ranges": changed_ranges,
        "changed_symbols": nodes_json(&graph, &changed_nodes, 50),
        "changed_nodes": nodes_json(&graph, &changed_nodes, 50),
        "temporal": temporal.as_ref().map(|diff| temporal_diff_json(&graph, diff)),
        "impacted": impacted,
        "test_coverage": test_coverage,
        "affected_flows": affected_flows,
        "risk_score": score,
        "risk": risk_label(score),
        "mapping_precision": mapping_precision,
        "suggested_next_tools": ["review_context", "impact", "traverse", "suggested_questions"]
    }))
}

fn old_changed_diff(
    graph: &Graph,
    base: &str,
) -> (Vec<String>, Vec<NodeId>, Vec<Value>, String, Option<TemporalDiff>) {
    let diff = git_changed_diff(base).unwrap_or_default();
    let changed_files: Vec<String> = diff.iter().map(|file| file.path.clone()).collect();
    let line_changed_nodes = nodes_for_changed_ranges(graph, &diff);
    let fallback_to_file_scope = line_changed_nodes.is_empty() && !changed_files.is_empty();
    let changed_nodes = if fallback_to_file_scope {
        nodes_for_files(graph, &changed_files)
    } else {
        line_changed_nodes
    };
    let mapping_precision = if fallback_to_file_scope {
        "file"
    } else if diff.iter().any(|f| !f.hunks.is_empty()) && !changed_nodes.is_empty() {
        "line"
    } else {
        "none"
    };
    (
        changed_files,
        changed_nodes,
        changed_ranges_json(graph, &diff),
        mapping_precision.to_string(),
        None,
    )
}

/// Temporal diff between two graph snapshots, resolved from git refs.
pub fn graph_diff_json(db: &Path, base: &str, head: &str, top: usize) -> Result<Value> {
    let store = Store::open(db)?;
    let graph = store.load_temporal()?;

    if !graph_has_temporal_data(&graph) {
        return Ok(json!({
            "operation": "graph_diff",
            "error": "graph has no temporal data; rebuild with git context (run `build` inside a git repo)",
        }));
    }
    let (Some(base_hash), Some(head_hash)) = (git_commit_hash(base)?, git_commit_hash(head)?)
    else {
        bail!("could not resolve git refs {base} / {head}");
    };

    let mut cache = HashMap::new();
    let diff = temporal_diff(
        &graph,
        &base_hash,
        &head_hash,
        &mut |ancestor, descendant| {
            *cache
                .entry((ancestor.to_string(), descendant.to_string()))
                .or_insert_with(|| git_is_ancestor(ancestor, descendant))
        },
    );

    Ok(json!({
        "operation": "graph_diff",
        "base": base,
        "head": head,
        "added_nodes": nodes_json(&graph, &diff.added_nodes, top),
        "removed_nodes": nodes_json(&graph, &diff.removed_nodes, top),
        "added_edges": changed_edges_json(&graph, &diff.added_edges),
        "removed_edges": changed_edges_json(&graph, &diff.removed_edges),
    }))
}

fn graph_has_temporal_data(graph: &Graph) -> bool {
    graph
        .nodes()
        .any(|(_, n)| n.valid_from.is_some() || n.valid_to.is_some())
        || graph
            .edges()
            .any(|(_, _, _, e)| e.valid_from.is_some() || e.valid_to.is_some())
}

fn temporal_diff_json(graph: &Graph, diff: &TemporalDiff) -> Value {
    json!({
        "added_nodes": nodes_json(graph, &diff.added_nodes, 50),
        "removed_nodes": nodes_json(graph, &diff.removed_nodes, 50),
        "added_edges": changed_edges_json(graph, &diff.added_edges),
        "removed_edges": changed_edges_json(graph, &diff.removed_edges),
    })
}

fn changed_edges_json(
    graph: &Graph,
    edges: &[ariadne_graph::query::ChangedEdge],
) -> Vec<Value> {
    edges
        .iter()
        .map(|edge| {
            let src = graph.node(edge.src);
            let dst = graph.node(edge.dst);
            json!({
                "id": edge.id.0,
                "kind": edge.edge_kind,
                "change": edge.change,
                "src_id": edge.src.0,
                "dst_id": edge.dst.0,
                "src": src.map(|node| node.qualified_name.clone()),
                "dst": dst.map(|node| node.qualified_name.clone()),
                "source_uri": src.and_then(|node| node.source_uri.clone())
                    .or_else(|| dst.and_then(|node| node.source_uri.clone())),
            })
        })
        .collect()
}

/// Helper: nodes JSON.
pub(super) fn nodes_json(graph: &Graph, ids: &[NodeId], limit: usize) -> Vec<Value> {
    ids.iter()
        .take(limit)
        .filter_map(|id| {
            graph.node(*id).map(|n| {
                json!({
                    "id": id.0,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                    "line_start": n.line_start,
                    "line_end": n.line_end,
                })
            })
        })
        .collect()
}

/// Helper: changed ranges JSON.
fn changed_ranges_json(graph: &Graph, diff: &[ChangedFile]) -> Vec<Value> {
    diff.iter()
        .map(|file| {
            json!({
                "path": file.path,
                "hunks": file.hunks.iter().map(|hunk| {
                    json!({
                        "old_start": hunk.old_start,
                        "old_count": hunk.old_count,
                        "new_start": hunk.new_start,
                        "new_count": hunk.new_count,
                        "new_end": hunk.new_end(),
                        "symbols": nodes_json(
                            graph,
                            &nodes_for_changed_hunk(graph, &file.path, hunk),
                            20
                        ),
                    })
                }).collect::<Vec<_>>()
            })
        })
        .collect()
}

/// Audit each changed symbol for test coverage.
fn test_coverage_json(graph: &Graph, changed_nodes: &[NodeId]) -> Value {
    let is_callable =
        |n: &ariadne_graph::Node| matches!(n.kind, NodeKind::Function | NodeKind::Method);
    let is_test_node = |n: &ariadne_graph::Node| {
        n.properties
            .get("is_test")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };

    let mut covered = Vec::new();
    let mut missing = Vec::new();
    for &id in changed_nodes {
        let Some(node) = graph.node(id) else { continue };
        if !is_callable(node) || is_test_node(node) {
            continue;
        }
        let tests: Vec<Value> = graph
            .out_neighbors(id)
            .filter(|(_, edge)| edge.kind == ariadne_graph::EdgeKind::TestedBy)
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
            "id": id.0,
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
    json!({
        "covered": covered,
        "missing": missing,
        "missing_count": missing.len(),
    })
}

/// Flows touched by the changed symbols.
fn affected_flows_json(graph: &Graph, changed_nodes: &[NodeId], limit: usize) -> Value {
    let flow_ids = affected_flows(graph, changed_nodes);
    let total = flow_ids.len();
    let hits: Vec<Value> = flow_ids
        .into_iter()
        .take(limit)
        .filter_map(|flow_id| {
            let node = graph.node(flow_id)?;
            Some(json!({
                "id": flow_id.0,
                "qualified_name": node.qualified_name,
                "entry_name": node.properties.get("entry_name"),
                "entry_qualified_name": node.properties.get("entry_qualified_name"),
                "criticality": node.properties.get("criticality"),
                "node_count": node.properties.get("node_count"),
                "depth": node.properties.get("depth"),
                "is_test_flow": node.properties.get("is_test_flow"),
            }))
        })
        .collect();
    json!({
        "hits": hits,
        "total": total,
        "truncated": total > limit,
    })
}

/// Risk score calculation.
fn risk_score(
    graph: &Graph,
    changed_nodes: &[NodeId],
    impacted_count: usize,
    missing_tests: usize,
    top_flow_criticality: f64,
) -> f64 {
    let degree: usize = changed_nodes
        .iter()
        .map(|id| graph.in_neighbors(*id).count() + graph.out_neighbors(*id).count())
        .sum();
    let flow_bump = top_flow_criticality.clamp(0.0, 1.0) * 15.0;
    ((changed_nodes.len() as f64 * 0.8)
        + (impacted_count as f64 * 1.2)
        + (degree as f64 * 0.25)
        + (missing_tests as f64 * 2.5)
        + flow_bump)
        .min(100.0)
}

fn risk_label(score: f64) -> &'static str {
    if score >= 35.0 {
        "high"
    } else if score >= 12.0 {
        "medium"
    } else {
        "low"
    }
}
