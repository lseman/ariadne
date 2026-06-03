use anyhow::{bail, Result};
use ariadne_graph::{Graph, NodeId, NodeKind};
use ariadne_graph::query::{
    analyze_impact, articulation_points, bridge_scores, call_resolution_stats, core_numbers,
    cyclic_components, find_top_paths, leiden, pagerank, paths::PathQuery, personalized_pagerank,
    ranked_search, temporal_diff, ImpactQuery, TemporalDiff,
};
use ariadne_graph::store::Store;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::helpers::{resolve, source_matches};
use super::git::{
    git_changed_diff, git_commit_hash, git_is_ancestor, ChangedFile,
};
use super::helpers::{nodes_for_changed_hunk, nodes_for_changed_ranges, nodes_for_files};

/// One-operation JSON interface for agents and MCP wrappers.
pub fn tool_response(db: &Path, operation: &str, params: &Value) -> Result<Value> {
    let store = Store::open(db)?;
    let graph = store.load()?;
    let detail = DetailLevel::from_params(params);
    let response = match operation {
        "minimal_context" | "context" => {
            let target = params.get("target").and_then(Value::as_str);
            let mode = params
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("review");
            compact_for_detail(minimal_context_json(&graph, target, mode), detail)
        }
        "status" => {
            let (nodes, edges) = store.stats()?;
            let calls = call_resolution_stats(&graph);
            json!({
                "operation": operation,
                "nodes": nodes,
                "edges": edges,
                "call_resolution": {
                    "resolved": calls.resolved,
                    "unresolved": calls.unresolved,
                    "rate": calls.rate(),
                },
            })
        }
        "search" => {
            let query = params.get("query").and_then(Value::as_str).unwrap_or("");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let hits: Vec<_> = ranked_search(&graph, query, limit)
                .into_iter()
                .filter_map(|hit| {
                    graph.node(hit.id).map(|n| {
                        json!({
                            "id": hit.id.0,
                            "score": hit.score,
                            "name": n.name,
                            "qualified_name": n.qualified_name,
                            "kind": n.kind,
                            "source_uri": n.source_uri,
                            "signals": hit.signals,
                        })
                    })
                })
                .collect();
            compact_for_detail(json!({ "operation": operation, "hits": hits }), detail)
        }
        "paths" => {
            let from = required_str(params, "from")?;
            let to = required_str(params, "to")?;
            let max_hops = params.get("max_hops").and_then(Value::as_u64).unwrap_or(5) as usize;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let from_id = resolve(&graph, from)?;
            let to_id = resolve(&graph, to)?;
            let paths: Vec<_> =
                find_top_paths(&graph, &PathQuery::between(from_id, to_id, max_hops), limit)
                    .into_iter()
                    .map(|path| {
                        let nodes: Vec<_> = path
                            .nodes
                            .into_iter()
                            .filter_map(|id| {
                                graph.node(id).map(|n| {
                                    json!({
                                        "id": id.0,
                                        "qualified_name": n.qualified_name,
                                        "kind": n.kind,
                                    })
                                })
                            })
                            .collect();
                        json!({ "cost": path.cost, "nodes": nodes })
                    })
                    .collect();
            compact_for_detail(json!({ "operation": operation, "paths": paths }), detail)
        }
        "impact" => {
            let target = required_str(params, "target")?;
            let max_hops = params.get("max_hops").and_then(Value::as_u64).unwrap_or(4) as usize;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            let seed = resolve(&graph, target)?;
            let hits: Vec<_> = analyze_impact(
                &graph,
                ImpactQuery {
                    seed,
                    max_hops,
                    limit,
                },
            )
            .into_iter()
            .filter_map(|hit| {
                graph.node(hit.id).map(|n| {
                    json!({
                        "id": hit.id.0,
                        "score": hit.score,
                        "distance": hit.distance,
                        "qualified_name": n.qualified_name,
                        "kind": n.kind,
                        "source_uri": n.source_uri,
                        "via": hit.via,
                    })
                })
            })
            .collect();
            compact_for_detail(
                json!({ "operation": operation, "target": target, "hits": hits }),
                detail,
            )
        }
        "detect_changes" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(2) as usize;
            compact_for_detail(detect_changes_json(db, base, max_depth)?, detail)
        }
        "review_context" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let max_lines_per_file = params
                .get("max_lines_per_file")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize;
            let token_budget = params
                .get("token_budget")
                .and_then(Value::as_u64)
                .unwrap_or(1600) as usize;
            compact_for_detail(
                review_context_json(db, base, max_lines_per_file, token_budget)?,
                detail,
            )
        }
        "traverse" => {
            let target = required_str(params, "target")?;
            let direction = params
                .get("direction")
                .and_then(Value::as_str)
                .unwrap_or("both");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(3) as usize;
            let token_budget = params
                .get("token_budget")
                .and_then(Value::as_u64)
                .unwrap_or(1200) as usize;
            let seed = resolve(&graph, target)?;
            compact_for_detail(
                traverse_json(&graph, seed, direction, max_depth, token_budget),
                detail,
            )
        }
        "large_functions" => {
            let min_lines = params
                .get("min_lines")
                .and_then(Value::as_u64)
                .unwrap_or(80) as u32;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            compact_for_detail(large_functions_json(&graph, min_lines, limit), detail)
        }
        "bridge_nodes" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(bridge_nodes_json(&graph, limit), detail)
        }
        "cycles" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(cycles_json(&graph, limit), detail)
        }
        "core" | "k_core" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(core_json(&graph, limit), detail)
        }
        "articulation" | "articulation_points" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(articulation_json(&graph, limit), detail)
        }
        "gaps" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(gaps_json(&graph, limit), detail)
        }
        "surprises" | "surprise_scoring" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(surprises_json(&graph, limit), detail)
        }
        "diagnostics" | "health" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            compact_for_detail(diagnostics_json(db, limit)?, detail)
        }
        "graph_diff" => {
            let base = params.get("base").and_then(Value::as_str).unwrap_or("HEAD~1");
            let head = params.get("head").and_then(Value::as_str).unwrap_or("HEAD");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            compact_for_detail(graph_diff_json(db, base, head, limit)?, detail)
        }
        "counterfactual" => {
            let target = required_str(params, "target")?;
            let direction = params.get("direction").and_then(Value::as_str).unwrap_or("out");
            let max_depth = params.get("max_depth").and_then(Value::as_u64).unwrap_or(5) as usize;
            compact_for_detail(counterfactual_json(db, target, direction, max_depth)?, detail)
        }
        "motifs" => {
            let built_in = params.get("built_in").and_then(Value::as_str).unwrap_or("security_audit");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            compact_for_detail(motifs_json(db, built_in, limit)?, detail)
        }
        "suggested_questions" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let analysis = detect_changes_json(db, base, 2)?;
            compact_for_detail(suggested_questions_json(&analysis, limit), detail)
        }
        "architecture_overview" | "architecture" => architecture_overview_json(&graph, detail),
        "god_nodes" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let ranks = if let Some(seed) = params.get("seed").and_then(Value::as_str) {
                let seed_id = resolve(&graph, seed)?;
                personalized_pagerank(&graph, &[(seed_id, 1.0)], 0.85, 50)
            } else {
                pagerank(&graph, 0.85, 50)
            };
            let mut sorted: Vec<_> = ranks.into_iter().collect();
            sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let hits: Vec<_> = sorted
                .into_iter()
                .take(limit)
                .filter_map(|(id, score)| {
                    graph.node(id).map(|n| {
                        json!({
                            "id": id.0,
                            "score": score,
                            "qualified_name": n.qualified_name,
                            "kind": n.kind,
                        })
                    })
                })
                .collect();
            compact_for_detail(json!({ "operation": operation, "hits": hits }), detail)
        }
        "flows" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(25) as usize;
            let ids = ariadne_graph::extract::flows::all_flows(&graph);
            let total = ids.len();
            let hits: Vec<Value> = ids
                .into_iter()
                .take(limit)
                .filter_map(|id| {
                    let node = graph.node(id)?;
                    Some(json!({
                        "id": id.0,
                        "qualified_name": node.qualified_name,
                        "entry_qualified_name": node.properties.get("entry_qualified_name"),
                        "entry_name": node.properties.get("entry_name"),
                        "criticality": node.properties.get("criticality"),
                        "node_count": node.properties.get("node_count"),
                        "depth": node.properties.get("depth"),
                        "is_test_flow": node.properties.get("is_test_flow"),
                    }))
                })
                .collect();
            compact_for_detail(
                json!({
                    "operation": operation,
                    "hits": hits,
                    "total": total,
                    "truncated": total > limit,
                }),
                detail,
            )
        }
        "affected_flows" => {
            let base = params
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("HEAD~1");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let analysis = detect_changes_json(db, base, 2)?;
            let payload = analysis
                .get("affected_flows")
                .cloned()
                .unwrap_or_else(|| json!({"hits": [], "total": 0, "truncated": false}));
            let truncated_hits: Vec<Value> = payload["hits"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(limit)
                .collect();
            compact_for_detail(
                json!({
                    "operation": operation,
                    "base": base,
                    "hits": truncated_hits,
                    "total": payload["total"],
                }),
                detail,
            )
        }
        other => bail!("unknown tool operation {}", other),
    };
    Ok(apply_response_guardrails(response, &graph, params, detail))
}

/// Minimal context for a target.
pub fn minimal_context_json(graph: &Graph, target: Option<&str>, mode: &str) -> Value {
    let hits = target
        .map(|q| ranked_search(graph, q, 5))
        .unwrap_or_default();
    let top_symbols: Vec<_> = hits
        .iter()
        .filter_map(|hit| {
            graph.node(hit.id).map(|n| {
                json!({
                    "id": hit.id.0,
                    "score": hit.score,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect();
    let risk = if hits.first().map(|h| h.score > 120.0).unwrap_or(false) {
        "medium"
    } else {
        "low"
    };
    json!({
        "operation": "minimal_context",
        "mode": mode,
        "target": target,
        "summary": if target.is_some() {
            "Resolved target candidates and prepared next graph operations."
        } else {
            "No target supplied; start with search, detect_changes, or bridge_nodes."
        },
        "risk": risk,
        "top_symbols": top_symbols,
        "suggested_next_tools": ["detect_changes", "impact", "traverse", "review_context"]
    })
}

/// Detail level for response compactness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailLevel {
    Minimal,
    Standard,
    Full,
}

impl DetailLevel {
    pub fn parse(value: &str) -> Self {
        match value {
            "minimal" => Self::Minimal,
            "full" => Self::Full,
            _ => Self::Standard,
        }
    }

    pub fn from_params(params: &Value) -> Self {
        params
            .get("detail_level")
            .and_then(Value::as_str)
            .map(Self::parse)
            .unwrap_or(Self::Standard)
    }

    pub fn limit(self, standard: usize) -> usize {
        match self {
            Self::Minimal => standard.min(5),
            Self::Standard => standard,
            Self::Full => standard.saturating_mul(4),
        }
    }
}

fn compact_for_detail(mut value: Value, detail: DetailLevel) -> Value {
    if detail == DetailLevel::Minimal {
        if let Some(arr) = value.get_mut("snippets").and_then(Value::as_array_mut) {
            for item in arr {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("snippet");
                }
            }
        }
    }
    value
}

/// Response guardrails for pagination.
#[derive(Debug, Clone, Copy)]
struct ResponseGuardrails {
    offset: usize,
    limit: usize,
    include_graph_summary: bool,
}

impl ResponseGuardrails {
    const HARD_LIMIT: usize = 500;

    fn from_params(params: &Value, detail: DetailLevel) -> Self {
        let default_limit = match detail {
            DetailLevel::Minimal => 10,
            DetailLevel::Standard => 50,
            DetailLevel::Full => 200,
        };
        let requested = params
            .get("response_limit")
            .or_else(|| params.get("page_limit"))
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(default_limit);
        Self {
            offset: params.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
            limit: requested.clamp(1, Self::HARD_LIMIT),
            include_graph_summary: params
                .get("include_graph_summary")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}

fn apply_response_guardrails(
    mut value: Value,
    graph: &Graph,
    params: &Value,
    detail: DetailLevel,
) -> Value {
    let guardrails = ResponseGuardrails::from_params(params, detail);
    let mut pagination = serde_json::Map::new();
    for key in PAGEABLE_RESPONSE_KEYS {
        if let Some(arr) = value.get_mut(key).and_then(Value::as_array_mut) {
            let total = arr.len();
            let start = guardrails.offset.min(total);
            let end = (start + guardrails.limit).min(total);
            let page: Vec<Value> = arr[start..end].to_vec();
            *arr = page;
            pagination.insert(
                key.to_string(),
                json!({
                    "offset": guardrails.offset,
                    "limit": guardrails.limit,
                    "returned": end.saturating_sub(start),
                    "total": total,
                    "has_more": end < total,
                }),
            );
        }
    }

    if let Some(obj) = value.as_object_mut() {
        if guardrails.include_graph_summary && !obj.contains_key("graph_summary") {
            obj.insert("graph_summary".to_string(), graph_summary_json(graph));
        }
        obj.insert(
            "guardrails".to_string(),
            json!({
                "response_limit": guardrails.limit,
                "offset": guardrails.offset,
                "hard_limit": ResponseGuardrails::HARD_LIMIT,
                "pagination": pagination,
            }),
        );
    }
    value
}

const PAGEABLE_RESPONSE_KEYS: &[&str] = &[
    "hits",
    "nodes",
    "paths",
    "impacted",
    "changed_files",
    "changed_ranges",
    "changed_symbols",
    "changed_nodes",
    "snippets",
    "communities",
    "cross_community_coupling",
    "bridge_nodes",
    "cycles",
    "core_nodes",
    "articulation_points",
    "warnings",
    "questions",
];

/// Graph summary for response guardrails.
pub fn graph_summary_json(graph: &Graph) -> Value {
    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    for (_, node) in graph.nodes() {
        *kind_counts.entry(format!("{:?}", node.kind)).or_insert(0) += 1;
        if let Some(source) = node.source_uri.as_ref() {
            *source_counts.entry(source.clone()).or_insert(0) += 1;
        }
    }

    let mut kinds: Vec<_> = kind_counts.into_iter().collect();
    kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let mut sources: Vec<_> = source_counts.into_iter().collect();
    sources.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

    json!({
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "kind_counts": kinds.into_iter().map(|(kind, count)| json!({
            "kind": kind,
            "count": count,
        })).collect::<Vec<_>>(),
        "top_sources": sources.into_iter().take(5).map(|(source, nodes)| json!({
            "source": source,
            "nodes": nodes,
        })).collect::<Vec<_>>(),
    })
}

/// Architecture overview at community level.
pub fn architecture_overview_json(graph: &Graph, detail: DetailLevel) -> Value {
    let communities = leiden(graph);
    let mut by_comm: HashMap<usize, Vec<NodeId>> = HashMap::new();
    for (&node, &community) in &communities {
        by_comm.entry(community).or_default().push(node);
    }

    let mut summaries: Vec<_> = by_comm
        .iter()
        .map(|(community, members)| {
            let mut files: HashMap<String, usize> = HashMap::new();
            let mut kinds: HashMap<String, usize> = HashMap::new();
            for id in members {
                if let Some(node) = graph.node(*id) {
                    if let Some(source) = &node.source_uri {
                        *files.entry(source.clone()).or_insert(0) += 1;
                    }
                    *kinds.entry(format!("{:?}", node.kind)).or_insert(0) += 1;
                }
            }
            let mut top_files: Vec<_> = files.into_iter().collect();
            top_files.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            let mut kind_counts: Vec<_> = kinds.into_iter().collect();
            kind_counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            json!({
                "community": community,
                "size": members.len(),
                "top_files": top_files.into_iter().take(detail.limit(5)).map(|(path, count)| json!({"path": path, "nodes": count})).collect::<Vec<_>>(),
                "kind_counts": kind_counts.into_iter().map(|(kind, count)| json!({"kind": kind, "count": count})).collect::<Vec<_>>(),
            })
        })
        .collect();
    summaries.sort_by_key(|v| std::cmp::Reverse(v["size"].as_u64().unwrap_or_default()));
    summaries.truncate(detail.limit(12));

    let mut coupling: HashMap<(usize, usize), usize> = HashMap::new();
    for (_, src, dst, _) in graph.edges() {
        let Some(a) = communities.get(&src).copied() else {
            continue;
        };
        let Some(b) = communities.get(&dst).copied() else {
            continue;
        };
        if a != b {
            let key = if a < b { (a, b) } else { (b, a) };
            *coupling.entry(key).or_insert(0) += 1;
        }
    }
    let mut coupling_rows: Vec<_> = coupling
        .into_iter()
        .map(|((a, b), edges)| json!({"from": a, "to": b, "edges": edges}))
        .collect();
    coupling_rows.sort_by_key(|v| std::cmp::Reverse(v["edges"].as_u64().unwrap_or_default()));
    coupling_rows.truncate(detail.limit(10));

    let bridges = bridge_nodes_json(graph, detail.limit(10));
    let cycles = cycles_json(graph, detail.limit(8));
    let core = core_json(graph, detail.limit(10));
    let articulations = articulation_json(graph, detail.limit(10));
    let warnings: Vec<_> = coupling_rows
        .iter()
        .take(5)
        .filter_map(|row| {
            let edges = row["edges"].as_u64().unwrap_or_default();
            (edges >= 5).then(|| {
                json!({
                    "kind": "cross_community_coupling",
                    "severity": if edges >= 20 { "high" } else { "medium" },
                    "communities": [row["from"].clone(), row["to"].clone()],
                    "edges": edges,
                })
            })
        })
        .collect();

    json!({
        "operation": "architecture_overview",
        "detail_level": match detail {
            DetailLevel::Minimal => "minimal",
            DetailLevel::Standard => "standard",
            DetailLevel::Full => "full",
        },
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "community_count": by_comm.len(),
        "communities": summaries,
        "cross_community_coupling": coupling_rows,
        "bridge_nodes": bridges["hits"].clone(),
        "cycles": cycles["hits"].clone(),
        "core_nodes": core["hits"].clone(),
        "articulation_points": articulations["hits"].clone(),
        "warnings": warnings,
        "suggested_next_tools": ["bridge_nodes", "cycles", "core", "articulation_points", "traverse", "impact", "gaps"]
    })
}

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
    let risk_score = risk_score(
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
        "risk_score": risk_score,
        "risk": risk_label(risk_score),
        "mapping_precision": mapping_precision,
        "suggested_next_tools": ["review_context", "impact", "traverse", "suggested_questions"]
    }))
}

fn old_changed_diff(
    graph: &Graph,
    base: &str,
) -> (
    Vec<String>,
    Vec<NodeId>,
    Vec<Value>,
    String,
    Option<TemporalDiff>,
) {
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

fn temporal_diff_json(graph: &Graph, diff: &TemporalDiff) -> Value {
    json!({
        "added_nodes": nodes_json(graph, &diff.added_nodes, 50),
        "removed_nodes": nodes_json(graph, &diff.removed_nodes, 50),
        "added_edges": changed_edges_json(graph, &diff.added_edges),
        "removed_edges": changed_edges_json(graph, &diff.removed_edges),
    })
}

fn changed_edges_json(graph: &Graph, edges: &[ariadne_graph::query::ChangedEdge]) -> Vec<Value> {
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

fn graph_has_temporal_data(graph: &Graph) -> bool {
    graph
        .nodes()
        .any(|(_, n)| n.valid_from.is_some() || n.valid_to.is_some())
        || graph
            .edges()
            .any(|(_, _, _, e)| e.valid_from.is_some() || e.valid_to.is_some())
}

/// Token-budgeted review context for changed and impacted files.
pub fn review_context_json(
    db: &Path,
    base: &str,
    max_lines_per_file: usize,
    token_budget: usize,
) -> Result<Value> {
    let analysis = detect_changes_json(db, base, 2)?;
    let mut files: Vec<String> = analysis["changed_files"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    for item in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        if let Some(source) = item["source_uri"].as_str() {
            if !files.iter().any(|f| source_matches(f, source)) {
                files.push(source.to_string());
            }
        }
    }
    let mut used_tokens = 0usize;
    let mut snippets = Vec::new();
    for file in files {
        if used_tokens >= token_budget {
            break;
        }
        let ranges = ranges_for_file_from_analysis(&analysis, &file);
        if let Ok(snippet) = file_snippet_for_ranges(&file, &ranges, max_lines_per_file) {
            let tokens = approx_tokens(&snippet);
            if used_tokens + tokens > token_budget && !snippets.is_empty() {
                continue;
            }
            used_tokens += tokens;
            snippets.push(json!({
                "path": file,
                "tokens": tokens,
                "changed_ranges": ranges,
                "snippet": snippet
            }));
        }
    }
    Ok(json!({
        "operation": "review_context",
        "base": base,
        "token_budget": token_budget,
        "used_tokens": used_tokens,
        "analysis": analysis,
        "snippets": snippets,
    }))
}

/// Traverse graph relationships from a target with a token budget.
pub fn traverse_json(
    graph: &Graph,
    seed: NodeId,
    direction: &str,
    max_depth: usize,
    token_budget: usize,
) -> Value {
    let mut queue = std::collections::VecDeque::from([(seed, 0usize)]);
    let mut seen = HashSet::from([seed]);
    let mut nodes = Vec::new();
    let mut used = 0usize;
    while let Some((id, depth)) = queue.pop_front() {
        if used >= token_budget {
            break;
        }
        if let Some(n) = graph.node(id) {
            let item = json!({
                "id": id.0,
                "depth": depth,
                "qualified_name": n.qualified_name,
                "kind": n.kind,
                "source_uri": n.source_uri,
                "in_degree": graph.in_neighbors(id).count(),
                "out_degree": graph.out_neighbors(id).count(),
            });
            used += approx_tokens(&item.to_string());
            nodes.push(item);
        }
        if depth >= max_depth {
            continue;
        }
        let mut neighbors = Vec::new();
        if direction == "out" || direction == "both" {
            neighbors.extend(graph.out_neighbors(id).map(|(n, _)| n));
        }
        if direction == "in" || direction == "both" {
            neighbors.extend(graph.in_neighbors(id).map(|(n, _)| n));
        }
        for next in neighbors {
            if seen.insert(next) {
                queue.push_back((next, depth + 1));
            }
        }
    }
    json!({ "operation": "traverse", "direction": direction, "used_tokens": used, "nodes": nodes })
}

/// Find large functions/classes by source span.
pub fn large_functions_json(graph: &Graph, min_lines: u32, limit: usize) -> Value {
    let mut rows: Vec<_> = graph
        .nodes()
        .filter_map(|(id, n)| {
            if !matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Trait
            ) {
                return None;
            }
            let lines = n
                .line_start
                .zip(n.line_end)
                .map(|(s, e)| e.saturating_sub(s) + 1)?;
            (lines >= min_lines).then(|| {
                json!({
                    "id": id.0,
                    "lines": lines,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse(v["lines"].as_u64().unwrap_or_default()));
    rows.truncate(limit);
    json!({ "operation": "large_functions", "hits": rows })
}

/// Find bridge/chokepoint nodes.
pub fn bridge_nodes_json(graph: &Graph, limit: usize) -> Value {
    let communities = leiden(graph);
    let rows: Vec<_> = bridge_scores(graph, &communities, limit)
        .into_iter()
        .filter_map(|row| {
            graph.node(row.node).map(|n| {
                json!({
                    "id": row.node.0,
                    "score": row.score,
                    "communities_touched": row.communities_touched,
                    "degree": row.degree,
                    "approx_betweenness": row.approx_betweenness,
                    "articulation": row.articulation,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect();
    json!({ "operation": "bridge_nodes", "hits": rows })
}

/// Find dependency cycles via strongly connected components.
pub fn cycles_json(graph: &Graph, limit: usize) -> Value {
    let mut cycles = cyclic_components(graph);
    cycles.sort_by_key(|c| std::cmp::Reverse(c.nodes.len()));
    let hits: Vec<_> = cycles
        .into_iter()
        .take(limit)
        .map(|component| {
            let nodes = component
                .nodes
                .into_iter()
                .filter_map(|id| {
                    graph.node(id).map(|n| {
                        json!({
                            "id": id.0,
                            "qualified_name": n.qualified_name,
                            "kind": n.kind,
                            "source_uri": n.source_uri,
                        })
                    })
                })
                .collect::<Vec<_>>();
            json!({ "size": nodes.len(), "nodes": nodes })
        })
        .collect();
    json!({ "operation": "cycles", "hits": hits })
}

/// Rank nodes by k-core/coreness.
pub fn core_json(graph: &Graph, limit: usize) -> Value {
    let core = core_numbers(graph);
    let mut rows: Vec<_> = core
        .into_iter()
        .filter_map(|(id, coreness)| {
            graph.node(id).map(|n| {
                json!({
                    "id": id.0,
                    "core": coreness,
                    "degree": graph.in_neighbors(id).count() + graph.out_neighbors(id).count(),
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse(v["core"].as_u64().unwrap_or_default()));
    rows.truncate(limit);
    json!({ "operation": "core", "hits": rows })
}

/// Find articulation points whose removal disconnects graph regions.
pub fn articulation_json(graph: &Graph, limit: usize) -> Value {
    let points = articulation_points(graph);
    let mut rows: Vec<_> = points
        .into_iter()
        .filter_map(|id| {
            graph.node(id).map(|n| {
                json!({
                    "id": id.0,
                    "degree": graph.in_neighbors(id).count() + graph.out_neighbors(id).count(),
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "source_uri": n.source_uri,
                })
            })
        })
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse(v["degree"].as_u64().unwrap_or_default()));
    rows.truncate(limit);
    json!({ "operation": "articulation_points", "hits": rows })
}

/// Identify structural weaknesses and likely review blind spots.
pub fn gaps_json(graph: &Graph, limit: usize) -> Value {
    let mut rows = Vec::new();
    for (id, n) in graph.nodes() {
        let indeg = graph.in_neighbors(id).count();
        let outdeg = graph.out_neighbors(id).count();
        let lines = n
            .line_start
            .zip(n.line_end)
            .map(|(s, e)| e.saturating_sub(s) + 1)
            .unwrap_or(0);
        if matches!(n.kind, NodeKind::Function | NodeKind::Method) && indeg == 0 {
            rows.push(json!({"kind":"orphan_symbol","severity":"medium","qualified_name":n.qualified_name,"source_uri":n.source_uri}));
        }
        if matches!(n.kind, NodeKind::Function | NodeKind::Method) && outdeg == 0 && lines > 40 {
            rows.push(json!({"kind":"large_leaf","severity":"low","lines":lines,"qualified_name":n.qualified_name,"source_uri":n.source_uri}));
        }
        if n.qualified_name.starts_with("call::") && indeg > 0 {
            rows.push(
                json!({"kind":"unresolved_call","severity":"high","call":n.name,"incoming":indeg}),
            );
        }
        if rows.len() >= limit {
            break;
        }
    }
    json!({ "operation": "gaps", "hits": rows })
}

/// Coarse language label derived from a node's source file extension.
/// Returns `None` for synthetic nodes with no source.
fn language_of(node: &ariadne_graph::core::Node) -> Option<&'static str> {
    let uri = node.source_uri.as_deref()?;
    let ext = uri.rsplit('.').next()?;
    Some(match ext {
        "rs" => "rust",
        "py" => "python",
        "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" => "cpp",
        "md" | "markdown" => "markdown",
        "tex" => "latex",
        "svg" => "diagram",
        _ => return None,
    })
}

/// Rank "surprising" edges: those that cross a community boundary, cross a
/// language boundary, or couple two high-degree hubs. These are the edges
/// most likely to represent unexpected coupling worth a human's attention.
pub fn surprises_json(graph: &Graph, limit: usize) -> Value {
    let communities = leiden(graph);

    // Degree per node, and the threshold above which a node is a "hub"
    // (top decile by degree, with a small floor so tiny graphs behave).
    let degree = |id: NodeId| graph.in_neighbors(id).count() + graph.out_neighbors(id).count();
    let mut degrees: Vec<usize> = graph.nodes().map(|(id, _)| degree(id)).collect();
    degrees.sort_unstable();
    let hub_threshold = if degrees.is_empty() {
        usize::MAX
    } else {
        let idx = (degrees.len() as f64 * 0.9) as usize;
        degrees[idx.min(degrees.len() - 1)].max(4)
    };

    let mut rows: Vec<Value> = Vec::new();
    for (id, src, dst, _edge) in graph.edges() {
        // Skip edges into unresolved placeholders — their "surprise" is
        // just missing resolution, not real coupling.
        let (Some(s), Some(d)) = (graph.node(src), graph.node(dst)) else {
            continue;
        };
        if s.qualified_name.starts_with("call::") || d.qualified_name.starts_with("call::") {
            continue;
        }

        let mut signals: Vec<&str> = Vec::new();
        let mut score = 0.0f32;

        match (communities.get(&src), communities.get(&dst)) {
            (Some(a), Some(b)) if a != b => {
                signals.push("cross_community");
                score += 1.0;
            }
            _ => {}
        }
        if let (Some(ls), Some(ld)) = (language_of(s), language_of(d)) {
            if ls != ld {
                signals.push("cross_language");
                score += 1.5;
            }
        }
        let (ds, dd) = (degree(src), degree(dst));
        if ds >= hub_threshold && dd >= hub_threshold {
            signals.push("hub_coupling");
            // Scale by how far both endpoints exceed the threshold.
            score += 1.0 + ((ds + dd) as f32 / (2.0 * hub_threshold as f32)).min(3.0);
        }

        if signals.is_empty() {
            continue;
        }
        rows.push(json!({
            "id": id.0,
            "score": score,
            "signals": signals,
            "src": s.qualified_name,
            "dst": d.qualified_name,
            "src_degree": ds,
            "dst_degree": dd,
            "source_uri": s.source_uri.clone().or_else(|| d.source_uri.clone()),
        }));
    }

    rows.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .partial_cmp(&a["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows.truncate(limit);
    json!({ "operation": "surprises", "hub_threshold": hub_threshold, "hits": rows })
}

/// Graph health report: index coverage, confidence mix, unresolved calls,
/// and warnings.
///
/// Unlike most operations this reads both the in-memory graph and the
/// store (for FTS5 and embedding index coverage), so it takes the db path.
pub fn diagnostics_json(db: &Path, limit: usize) -> Result<Value> {
    use ariadne_graph::core::Confidence;

    let store = Store::open(db)?;
    let graph = store.load()?;

    let node_count = graph.node_count();
    let edge_count = graph.edge_count();

    // --- index coverage ---
    // The two indexes cover different populations: `rebuild_fts_index`
    // indexes every node, while `rebuild_embeddings` skips synthetic
    // `call::` placeholders. Each coverage ratio uses its own denominator
    // so neither exceeds 1.0.
    let fts_indexed = store.fts_stats().unwrap_or(0);
    let (embeddings, embed_model) = store.embedding_stats().unwrap_or((0, None));
    let embeddable = graph
        .nodes()
        .filter(|(_, n)| !n.qualified_name.starts_with("call::"))
        .count();
    let coverage = |indexed: usize, total: usize| -> f32 {
        if total == 0 {
            0.0
        } else {
            (indexed as f32 / total as f32).min(1.0)
        }
    };

    // --- confidence mix ---
    let (mut extracted, mut inferred, mut ambiguous) = (0usize, 0usize, 0usize);
    for (_, _, _, edge) in graph.edges() {
        match edge.confidence {
            Confidence::Extracted => extracted += 1,
            Confidence::Inferred(_) => inferred += 1,
            Confidence::Ambiguous => ambiguous += 1,
        }
    }

    // --- unresolved calls ---
    let calls = call_resolution_stats(&graph);
    let mut unresolved: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| n.qualified_name.starts_with("call::"))
        .map(|(id, n)| (n.name.clone(), graph.in_neighbors(id).count()))
        .filter(|(_, indeg)| *indeg > 0)
        .collect();
    unresolved.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top_unresolved: Vec<_> = unresolved
        .iter()
        .take(limit)
        .map(|(name, indeg)| json!({ "call": name, "incoming": indeg }))
        .collect();

    // --- warnings ---
    let mut warnings = Vec::new();
    if fts_indexed == 0 {
        warnings.push(json!({
            "kind": "fts_index_empty",
            "severity": "high",
            "detail": "FTS5 index is empty; run `rebuild-fts` for full-text search",
        }));
    } else if coverage(fts_indexed, node_count) < 0.9 {
        warnings.push(json!({
            "kind": "fts_index_stale",
            "severity": "medium",
            "detail": "FTS5 index covers fewer than 90% of nodes; rebuild recommended",
        }));
    }
    if embeddings == 0 {
        warnings.push(json!({
            "kind": "embeddings_missing",
            "severity": "low",
            "detail": "no embeddings; run `embed` to enable semantic search boost",
        }));
    }
    if calls.rate() < 0.5 && calls.total() > 0 {
        warnings.push(json!({
            "kind": "low_call_resolution",
            "severity": "medium",
            "detail": format!(
                "only {:.0}% of call edges resolve to definitions; reachability queries may be incomplete",
                calls.rate() * 100.0
            ),
        }));
    }

    Ok(json!({
        "operation": "diagnostics",
        "health": {
            "node_count": node_count,
            "edge_count": edge_count,
            "embeddable_nodes": embeddable,
        },
        "index_coverage": {
            "fts_indexed": fts_indexed,
            "fts_coverage": coverage(fts_indexed, node_count),
            "embeddings": embeddings,
            "embedding_coverage": coverage(embeddings, embeddable),
            "embedding_model": embed_model,
        },
        "confidence_mix": {
            "extracted": extracted,
            "inferred": inferred,
            "ambiguous": ambiguous,
        },
        "call_resolution": {
            "resolved": calls.resolved,
            "unresolved": calls.unresolved,
            "rate": calls.rate(),
            "top_unresolved": top_unresolved,
        },
        "warnings": warnings,
        "suggested_next_tools": ["gaps", "rebuild_fts", "embed_graph", "surprises"],
    }))
}

/// Temporal diff between two graph snapshots, resolved from git refs.
pub fn graph_diff_json(db: &Path, base: &str, head: &str, top: usize) -> Result<Value> {
    let store = Store::open(db)?;
    // Include archived rows so nodes/edges removed between base and head
    // are still present to be diffed.
    let graph = store.load_temporal()?;

    if !graph_has_temporal_data(&graph) {
        return Ok(json!({
            "operation": "graph_diff",
            "error": "graph has no temporal data; rebuild with git context (run `build` inside a git repo)",
        }));
    }
    let (Some(base_hash), Some(head_hash)) = (git_commit_hash(base)?, git_commit_hash(head)?) else {
        bail!("could not resolve git refs {base} / {head}");
    };

    let mut cache = HashMap::new();
    let diff = temporal_diff(&graph, &base_hash, &head_hash, &mut |ancestor, descendant| {
        *cache
            .entry((ancestor.to_string(), descendant.to_string()))
            .or_insert_with(|| git_is_ancestor(ancestor, descendant))
    });

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

/// Drop a symbol's edges, rerun BFS, and report nodes that become
/// unreachable from the rest of the graph.
pub fn counterfactual_json(
    db: &Path,
    symbol: &str,
    direction: &str,
    max_depth: usize,
) -> Result<Value> {
    use ariadne_graph::query::counterfactual::run_without_edges;

    let store = Store::open(db)?;
    let graph = store.load()?;
    let target = resolve(&graph, symbol)?;

    // Edges incident to the target in the requested direction.
    let drop: Vec<_> = graph
        .edges()
        .filter(|(_, src, dst, _)| match direction {
            "in" => *dst == target,
            "both" => *src == target || *dst == target,
            _ => *src == target,
        })
        .map(|(id, _, _, _)| id)
        .collect();

    // BFS reachable set from the target, before and after the drop.
    let reach = |g: &Graph| -> HashSet<NodeId> {
        let mut seen = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back((target, 0usize));
        seen.insert(target);
        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let next: Vec<NodeId> = match direction {
                "in" => g.in_neighbors(node).map(|(n, _)| n).collect(),
                "both" => g
                    .out_neighbors(node)
                    .chain(g.in_neighbors(node))
                    .map(|(n, _)| n)
                    .collect(),
                _ => g.out_neighbors(node).map(|(n, _)| n).collect(),
            };
            for n in next {
                if seen.insert(n) {
                    queue.push_back((n, depth + 1));
                }
            }
        }
        seen
    };

    let before = reach(&graph);
    let counterfactual = run_without_edges(&graph, &drop);
    let after = reach(&counterfactual);

    let mut lost: Vec<NodeId> = before.difference(&after).copied().collect();
    lost.sort_by_key(|id| id.0);

    Ok(json!({
        "operation": "counterfactual",
        "target": graph.node(target).map(|n| n.qualified_name.clone()),
        "direction": direction,
        "dropped_edges": drop.len(),
        "reachable_before": before.len(),
        "reachable_after": after.len(),
        "unreachable_count": lost.len(),
        "now_unreachable": nodes_json(&graph, &lost, 50),
    }))
}

/// Match a built-in subgraph motif against the graph.
pub fn motifs_json(db: &Path, built_in: &str, limit: usize) -> Result<Value> {
    use ariadne_graph::query::motifs::{
        diamond_inheritance_motif, doc_function_triangle, find_motifs, security_audit_motif,
    };

    let store = Store::open(db)?;
    let graph = store.load()?;

    let motif = match built_in {
        "security_audit" => security_audit_motif(),
        "diamond" => diamond_inheritance_motif(),
        "doc_triangle" => doc_function_triangle(),
        other => bail!("unknown built-in motif {other}; expected security_audit, diamond, or doc_triangle"),
    };

    let matches = find_motifs(&graph, &motif, limit);
    Ok(json!({
        "operation": "motifs",
        "built_in": built_in,
        "match_count": matches.len(),
        "matches": matches,
    }))
}

/// Generate prioritized review questions from graph analysis.
pub fn suggested_questions_json(analysis: &Value, limit: usize) -> Value {
    let mut questions = Vec::new();
    for file in analysis["changed_files"].as_array().unwrap_or(&Vec::new()) {
        if let Some(file) = file.as_str() {
            questions.push(format!(
                "What behavior changed in {} and is it covered by tests?",
                file
            ));
        }
    }
    for hit in analysis["impacted"].as_array().unwrap_or(&Vec::new()) {
        if let Some(name) = hit["qualified_name"].as_str() {
            questions.push(format!(
                "Does the change alter assumptions relied on by {}?",
                name
            ));
        }
    }
    questions
        .push("Are any unresolved calls or large functions involved in this change?".to_string());
    questions.truncate(limit);
    json!({ "operation": "suggested_questions", "questions": questions })
}

/// Audit each changed symbol for test coverage.
fn test_coverage_json(graph: &Graph, changed_nodes: &[NodeId]) -> Value {
    let is_callable = |n: &ariadne_graph::Node| {
        matches!(n.kind, NodeKind::Function | NodeKind::Method)
    };
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
    let flow_ids = ariadne_graph::extract::flows::affected_flows(graph, changed_nodes);
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

/// File snippet utilities.
fn file_snippet(path: &str, max_lines: usize) -> Result<String> {
    let content = std::fs::read_to_string(path)?;
    Ok(content
        .lines()
        .take(max_lines)
        .enumerate()
        .map(|(i, line)| format!("{:>4}: {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn file_snippet_for_ranges(path: &str, ranges: &[(u32, u32)], max_lines: usize) -> Result<String> {
    if ranges.is_empty() {
        return file_snippet(path, max_lines);
    }

    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(String::new());
    }

    let mut windows = Vec::<(usize, usize)>::new();
    let context = 4usize;
    for (start, end) in ranges {
        let range_start = (*start).max(1) as usize;
        let range_end = (*end).max(*start).max(1) as usize;
        let from = range_start.saturating_sub(context + 1);
        let to = (range_end + context).min(lines.len());
        if from < to {
            windows.push((from, to));
        }
    }
    windows.sort_unstable();

    let mut merged = Vec::<(usize, usize)>::new();
    for (from, to) in windows {
        if let Some((_, last_to)) = merged.last_mut() {
            if from <= *last_to + 1 {
                *last_to = (*last_to).max(to);
                continue;
            }
        }
        merged.push((from, to));
    }

    let mut emitted = 0usize;
    let mut out = Vec::new();
    for (idx, (from, to)) in merged.into_iter().enumerate() {
        if emitted >= max_lines {
            break;
        }
        if idx > 0 {
            out.push("   ...".to_string());
        }
        for (local_idx, line) in lines.iter().skip(from).take(to - from).enumerate() {
            if emitted >= max_lines {
                break;
            }
            emitted += 1;
            out.push(format!("{:>4}: {}", from + local_idx + 1, line));
        }
    }
    Ok(out.join("\n"))
}

fn ranges_for_file_from_analysis(analysis: &Value, file: &str) -> Vec<(u32, u32)> {
    analysis["changed_ranges"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|entry| {
            entry["path"]
                .as_str()
                .map(|path| source_matches(path, file))
                .unwrap_or(false)
        })
        .flat_map(|entry| entry["hunks"].as_array().into_iter().flatten())
        .filter_map(|hunk| {
            let start = hunk["new_start"].as_u64()? as u32;
            let end = hunk["new_end"].as_u64()? as u32;
            Some((start.max(1), end.max(start).max(1)))
        })
        .collect()
}

fn approx_tokens(s: &str) -> usize {
    (s.len() / 4).max(1)
}

/// Helper: required string parameter.
fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string param '{}'", key))
}

/// Helper: append unique nodes.
#[allow(dead_code)]
fn append_unique_nodes(nodes: &mut Vec<NodeId>, extra: Vec<NodeId>) {
    let mut seen: HashSet<NodeId> = nodes.iter().copied().collect();
    for id in extra {
        if seen.insert(id) {
            nodes.push(id);
        }
    }
}

/// Helper: nodes JSON.
fn nodes_json(graph: &Graph, ids: &[NodeId], limit: usize) -> Vec<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne_graph::core::{Edge, EdgeKind, Node, NodeKind};

    #[test]
    fn surprises_flags_cross_language_edge() {
        // A Rust function with an edge to a Python function is a
        // cross-language coupling and must be flagged.
        let mut g = Graph::new();
        let mut rs = Node::new(NodeKind::Function, "rs::process_payment");
        rs.source_uri = Some("src/pay.rs".to_string());
        let rs_id = g.add_node(rs);
        let mut py = Node::new(NodeKind::Function, "py::charge");
        py.source_uri = Some("billing/charge.py".to_string());
        let py_id = g.add_node(py);
        g.add_edge(rs_id, py_id, Edge::extracted(EdgeKind::Calls));

        let out = surprises_json(&g, 10);
        let hits = out["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "expected one surprising edge, got {hits:?}");
        let signals: Vec<&str> = hits[0]["signals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            signals.contains(&"cross_language"),
            "expected cross_language signal, got {signals:?}"
        );
    }

    /// Save a small graph to a temp-file db and run `diagnostics_json`
    /// against it (the function opens the store by path, so an in-memory
    /// store will not do).
    fn diagnostics_for(graph: &Graph) -> (Value, std::path::PathBuf) {
        let path = std::env::temp_dir()
            .join(format!("ariadne_diag_{}_{}.db", std::process::id(), graph.node_count()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path).unwrap();
        store.save(graph).unwrap();
        let report = diagnostics_json(&path, 25).unwrap();
        (report, path)
    }

    #[test]
    fn diagnostics_reports_documented_sections() {
        let mut g = Graph::new();
        let caller = g.add_node(Node::new(NodeKind::Function, "caller"));
        let real = g.add_node(Node::new(NodeKind::Function, "real"));
        let ext = g.add_node(Node::new(NodeKind::Function, "call::external"));
        let other = g.add_node(Node::new(NodeKind::Function, "call::other"));
        g.add_edge(caller, real, Edge::extracted(EdgeKind::Calls));
        g.add_edge(caller, ext, Edge::ambiguous(EdgeKind::Calls));
        g.add_edge(caller, other, Edge::ambiguous(EdgeKind::Calls));

        let (report, path) = diagnostics_for(&g);

        // All documented top-level sections are present.
        for key in ["health", "index_coverage", "confidence_mix", "call_resolution", "warnings"] {
            assert!(report.get(key).is_some(), "missing section: {key}");
        }
        // Confidence mix counts the three call edges.
        assert_eq!(report["confidence_mix"]["extracted"], 1);
        assert_eq!(report["confidence_mix"]["ambiguous"], 2);
        // One resolved, two unresolved → rate 0.33 (< 0.5), which trips
        // the low-resolution warning and lists the placeholders.
        assert_eq!(report["call_resolution"]["resolved"], 1);
        assert_eq!(report["call_resolution"]["unresolved"], 2);
        let warnings = report["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|w| w["kind"] == "low_call_resolution"),
            "expected low_call_resolution warning, got {warnings:?}"
        );
        assert!(
            report["call_resolution"]["top_unresolved"]
                .as_array()
                .unwrap()
                .iter()
                .any(|u| u["call"] == "external"),
            "placeholder `external` should appear in top_unresolved"
        );

        let _ = std::fs::remove_file(&path);
    }
}
