use anyhow::{bail, Result};
use ariadne_graph::query::search_by_name;
use ariadne_graph::{Graph, NodeId, NodeKind};
use std::collections::HashSet;

/// Resolve a symbol name to a NodeId with disambiguation logic.
pub fn resolve(graph: &Graph, name: &str) -> Result<NodeId> {
    use ariadne_graph::NodeKind;
    if let Some(id) = graph.find_by_qname(name) {
        return Ok(id);
    }
    let results = search_by_name(graph, name);
    match results.len() {
        0 => bail!("no symbol found matching {}", name),
        1 => Ok(results[0]),
        _ => {
            // Prefer real definitions over `call::` placeholders.
            let defs: Vec<_> = results
                .iter()
                .copied()
                .filter(|id| {
                    graph
                        .node(*id)
                        .map(|n| !n.qualified_name.starts_with("call::"))
                        .unwrap_or(false)
                })
                .collect();
            // Among real defs, prefer Function/Class/Method/Type over Module.
            let callable: Vec<_> = defs
                .iter()
                .copied()
                .filter(|id| {
                    graph
                        .node(*id)
                        .map(|n| {
                            matches!(
                                n.kind,
                                NodeKind::Function
                                    | NodeKind::Method
                                    | NodeKind::Class
                                    | NodeKind::Type
                            )
                        })
                        .unwrap_or(false)
                })
                .collect();
            let pool = if !callable.is_empty() {
                &callable
            } else if !defs.is_empty() {
                &defs
            } else {
                &results
            };
            if pool.len() == 1 {
                return Ok(pool[0]);
            }
            // Exact-name match within the chosen pool.
            let exact: Vec<_> = pool
                .iter()
                .copied()
                .filter(|id| graph.node(*id).map(|n| n.name == name).unwrap_or(false))
                .collect();
            if exact.len() == 1 {
                return Ok(exact[0]);
            }
            let names: Vec<String> = pool
                .iter()
                .take(5)
                .filter_map(|id| graph.node(*id).map(|n| n.qualified_name.clone()))
                .collect();
            bail!("ambiguous symbol {}: matches {:?}", name, names);
        }
    }
}

/// Append unique nodes to a list.
pub fn append_unique_nodes(nodes: &mut Vec<NodeId>, extra: Vec<NodeId>) {
    let mut seen: HashSet<NodeId> = nodes.iter().copied().collect();
    for id in extra {
        if seen.insert(id) {
            nodes.push(id);
        }
    }
}

/// Source string matching.
pub fn source_matches(source: &str, path: &str) -> bool {
    source == path || source.ends_with(path) || path.ends_with(source)
}

/// Node kind specificity for ranking.
pub fn node_kind_specificity(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Function | NodeKind::Method => 5,
        NodeKind::Class | NodeKind::Trait | NodeKind::Impl => 4,
        NodeKind::Module | NodeKind::Type => 3,
        NodeKind::File | NodeKind::Document => 1,
        _ => 2,
    }
}

/// Normalized node span for git diff overlap.
pub fn normalized_node_span(line_start: Option<u32>, line_end: Option<u32>) -> Option<(u32, u32)> {
    let (start, end) = line_start.zip(line_end)?;
    if start == 0 {
        Some((1, end.saturating_add(1).max(1)))
    } else {
        Some((start, end.max(start)))
    }
}

/// Nodes for changed hunk.
pub fn nodes_for_changed_hunk(
    graph: &Graph,
    path: &str,
    hunk: &super::git::ChangedHunk,
) -> Vec<NodeId> {
    let mut scored = Vec::<(u8, u32, NodeId)>::new();
    for (id, node) in graph.nodes() {
        let Some(source) = node.source_uri.as_ref() else {
            continue;
        };
        if !source_matches(source, path) {
            continue;
        }
        let Some((line_start, line_end)) = normalized_node_span(node.line_start, node.line_end)
        else {
            continue;
        };
        if !hunk.overlaps_node(line_start, line_end) {
            continue;
        }
        scored.push((
            node_kind_specificity(node.kind),
            line_end.saturating_sub(line_start),
            id,
        ));
    }
    if scored.iter().any(|(_, _, id)| {
        graph
            .node(*id)
            .map(|node| !matches!(node.kind, NodeKind::File | NodeKind::Document))
            .unwrap_or(false)
    }) {
        scored.retain(|(_, _, id)| {
            graph
                .node(*id)
                .map(|node| !matches!(node.kind, NodeKind::File | NodeKind::Document))
                .unwrap_or(false)
        });
    }
    scored.sort_by_key(|(specificity, span, id)| (std::cmp::Reverse(*specificity), *span, id.0));
    scored.into_iter().map(|(_, _, id)| id).collect()
}

/// Nodes for changed ranges.
pub fn nodes_for_changed_ranges(graph: &Graph, diff: &[super::git::ChangedFile]) -> Vec<NodeId> {
    let mut nodes = Vec::new();
    for file in diff {
        for hunk in &file.hunks {
            append_unique_nodes(&mut nodes, nodes_for_changed_hunk(graph, &file.path, hunk));
        }
    }
    nodes
}

/// Nodes for files.
pub fn nodes_for_files(graph: &Graph, files: &[String]) -> Vec<NodeId> {
    graph
        .nodes()
        .filter(|(_, n)| {
            n.source_uri
                .as_ref()
                .map(|src| files.iter().any(|f| source_matches(src, f)))
                .unwrap_or(false)
        })
        .map(|(id, _)| id)
        .collect()
}
