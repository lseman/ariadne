use anyhow::Result;
use ariadne_graph::core::Confidence;
use ariadne_graph::query::{
    articulation_points, bridge_scores, call_resolution_stats, core_numbers, cyclic_components,
    leiden,
};
use ariadne_graph::store::Store;
use ariadne_graph::{Graph, NodeId, NodeKind};
use serde_json::{json, Value};
use std::path::Path;

/// Find large functions/classes by source span.
pub fn large_functions_json(graph: &Graph, min_lines: u32, limit: usize) -> Value {
    let mut rows: Vec<_> = graph
        .nodes()
        .filter_map(|(_id, n)| {
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
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => "js",
        "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" => "cpp",
        "md" | "markdown" => "markdown",
        "svg" => "diagram",
        _ => return None,
    })
}

/// Languages that represent documentation rather than executable code.
fn is_doc_language(lang: &str) -> bool {
    matches!(lang, "markdown" | "diagram")
}

/// Rank "surprising" edges: those that cross a community boundary, cross a
/// language boundary, or couple two high-degree hubs.
pub fn surprises_json(graph: &Graph, limit: usize) -> Value {
    let communities = leiden(graph);

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
    for (_id, src, dst, edge) in graph.edges() {
        let (Some(s), Some(d)) = (graph.node(src), graph.node(dst)) else {
            continue;
        };
        if s.qualified_name.starts_with("call::") || d.qualified_name.starts_with("call::") {
            continue;
        }

        let langs = (language_of(s), language_of(d));
        if let (Some(ls), Some(ld)) = langs {
            if ls != ld && edge.kind == ariadne_graph::core::EdgeKind::Calls {
                let inferred = edge.confidence != ariadne_graph::core::Confidence::Extracted;
                let code_to_doc = !is_doc_language(ls) && is_doc_language(ld);
                if inferred || code_to_doc {
                    continue;
                }
            }
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
        if let (Some(ls), Some(ld)) = langs {
            if ls != ld {
                signals.push("cross_language");
                score += 1.5;
            }
        }
        let (ds, dd) = (degree(src), degree(dst));
        if ds >= hub_threshold && dd >= hub_threshold {
            signals.push("hub_coupling");
            score += 1.0 + ((ds + dd) as f32 / (2.0 * hub_threshold as f32)).min(3.0);
        }

        if signals.is_empty() {
            continue;
        }
        rows.push(json!({
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
pub fn diagnostics_json(db: &Path, limit: usize) -> Result<Value> {
    let store = Store::open(db)?;
    let graph = store.load()?;

    let node_count = graph.node_count();
    let edge_count = graph.edge_count();

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

    let (mut extracted, mut inferred, mut ambiguous) = (0usize, 0usize, 0usize);
    for (_, _, _, edge) in graph.edges() {
        match edge.confidence {
            Confidence::Extracted => extracted += 1,
            Confidence::Inferred(_) => inferred += 1,
            Confidence::Ambiguous => ambiguous += 1,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne_graph::core::{Edge, EdgeKind, Node, NodeKind};

    #[test]
    fn surprises_flags_cross_language_edge() {
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

    #[test]
    fn surprises_suppresses_inferred_cross_language_calls() {
        let mut g = Graph::new();
        let mut py = Node::new(NodeKind::Function, "py::print_report");
        py.source_uri = Some("report/gen.py".to_string());
        let py_id = g.add_node(py);
        let mut js = Node::new(NodeKind::Function, "js::print");
        js.source_uri = Some("web/print.js".to_string());
        let js_id = g.add_node(js);
        g.add_edge(py_id, js_id, Edge::inferred(EdgeKind::Calls, 0.7));

        let out = surprises_json(&g, 10);
        let hits = out["hits"].as_array().unwrap();
        assert!(
            hits.is_empty(),
            "inferred cross-language call must be suppressed, got {hits:?}"
        );
    }

    #[test]
    fn surprises_suppresses_code_to_doc_calls() {
        let mut g = Graph::new();
        let mut rs = Node::new(NodeKind::Function, "rs::build");
        rs.source_uri = Some("src/build.rs".to_string());
        let rs_id = g.add_node(rs);
        let mut doc = Node::new(NodeKind::Section, "doc::building");
        doc.source_uri = Some("docs/build.md".to_string());
        let doc_id = g.add_node(doc);
        g.add_edge(rs_id, doc_id, Edge::extracted(EdgeKind::Calls));

        let out = surprises_json(&g, 10);
        let hits = out["hits"].as_array().unwrap();
        assert!(
            hits.is_empty(),
            "code→doc calls edge must be suppressed, got {hits:?}"
        );

        g.add_edge(doc_id, rs_id, Edge::extracted(EdgeKind::Mentions));
        let out = surprises_json(&g, 10);
        let hits = out["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "mentions edge should remain, got {hits:?}");
    }

    fn diagnostics_for(graph: &Graph) -> (Value, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "ariadne_diag_{}_{}.db",
            std::process::id(),
            graph.node_count()
        ));
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

        for key in [
            "health",
            "index_coverage",
            "confidence_mix",
            "call_resolution",
            "warnings",
        ] {
            assert!(report.get(key).is_some(), "missing section: {key}");
        }
        assert_eq!(report["confidence_mix"]["extracted"], 1);
        assert_eq!(report["confidence_mix"]["ambiguous"], 2);
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
