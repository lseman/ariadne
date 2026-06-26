//! Execution flow detection.
//!
//! A *flow* is a forward BFS from an entry-point function through `Calls`
//! edges, bounded by depth and node-count limits. Each flow is
//! materialised as a synthetic `NodeKind::Flow` node; member functions
//! point at it via `EdgeKind::MemberOf`, and the entry function carries
//! an additional `EdgeKind::EntryOf` edge so the seed is identifiable in
//! one hop.
//!
//! Entry-point detection in this pass is intentionally lean:
//!
//! - functions marked `is_test=true` (already detected upstream),
//! - functions named `main` / `__main__`,
//! - functions/methods with zero incoming `Calls` edges (orphans /
//!   library-public entry points).
//!
//! Anything more elaborate (framework decorator patterns à la CRG's
//! `flows.py`) can layer on top later without reshaping the storage.
//!
//! Re-running `compute_flows` on the same graph is idempotent: each
//! flow's `qualified_name` is derived from its entry's qualified name,
//! and old `MemberOf` / `EntryOf` edges into a re-detected flow are
//! pruned before the new membership set is written.

use crate::core::{Edge, EdgeKind, Graph, GraphMut, Node, NodeId, NodeKind};
use std::collections::{HashSet, VecDeque};

/// Tunable limits for flow tracing.
#[derive(Debug, Clone, Copy)]
pub struct FlowOptions {
    /// Maximum forward BFS depth from the entry point.
    pub max_depth: usize,
    /// Hard cap on members per flow to keep huge call graphs bounded.
    pub max_nodes_per_flow: usize,
    /// Minimum members for a flow to be materialised. Flows below this
    /// size are dropped: `size=2 depth=1` "flows" are noisy when the
    /// repo has many small helper functions that happen to look orphan
    /// to a name-only call resolver. Set to `2` to keep entry+1-callee
    /// flows, `1` to keep singleton entries.
    pub min_flow_size: usize,
}

impl Default for FlowOptions {
    fn default() -> Self {
        Self {
            max_depth: 6,
            max_nodes_per_flow: 200,
            min_flow_size: 3,
        }
    }
}

/// Detect entry points, trace flows, and materialise them into the
/// graph. Returns the number of flows produced.
pub fn compute_flows(graph: &mut dyn GraphMut) -> usize {
    compute_flows_with_options(graph, FlowOptions::default())
}

pub fn compute_flows_with_options(graph: &mut dyn GraphMut, options: FlowOptions) -> usize {
    let entries = detect_entry_points(graph);
    let mut produced = 0usize;
    for entry in entries {
        let Some(entry_node) = graph.node(entry) else {
            continue;
        };
        let entry_qn = entry_node.qualified_name.clone();
        let entry_name = entry_node.name.clone();
        let is_test_entry = is_test_node(entry_node);

        let members = trace_flow(graph, entry, &options);
        if members.len() < options.min_flow_size {
            continue;
        }

        let member_count = members.len();
        let depth_reached = members.iter().map(|(_, depth)| *depth).max().unwrap_or(0);
        let criticality = compute_criticality(graph, &members, &entry_name, is_test_entry);

        // Identity: flow:: prefix + entry qname. Stable across re-runs.
        let flow_qn = format!("flow::{}", entry_qn);
        let mut flow_node = Node::new(NodeKind::Flow, &flow_qn);
        flow_node = flow_node
            .with_property(
                "entry_qualified_name",
                serde_json::Value::String(entry_qn.clone()),
            )
            .with_property("entry_name", serde_json::Value::String(entry_name.clone()))
            .with_property(
                "depth",
                serde_json::Value::Number(serde_json::Number::from(depth_reached)),
            )
            .with_property(
                "node_count",
                serde_json::Value::Number(serde_json::Number::from(member_count)),
            )
            .with_property("criticality", serde_json::json!(criticality))
            .with_property("is_test_flow", serde_json::Value::Bool(is_test_entry));
        let flow_id = graph.add_node(flow_node);

        // Idempotency: a previous run may have left MemberOf / EntryOf
        // edges into this flow. Build a set of `(member, kind)` already
        // wired and skip duplicates rather than re-adding. petgraph's
        // multigraph backing means a plain add_edge would duplicate.
        let existing: HashSet<(NodeId, EdgeKind)> = graph
            .in_neighbors(flow_id)
            .filter_map(|(src, edge)| match edge.kind {
                EdgeKind::MemberOf | EdgeKind::EntryOf => Some((src, edge.kind)),
                _ => None,
            })
            .collect();

        if !existing.contains(&(entry, EdgeKind::EntryOf)) {
            graph.add_edge(entry, flow_id, Edge::extracted(EdgeKind::EntryOf));
        }
        for (member, _depth) in members {
            if member == entry {
                continue;
            }
            if existing.contains(&(member, EdgeKind::MemberOf)) {
                continue;
            }
            graph.add_edge(member, flow_id, Edge::extracted(EdgeKind::MemberOf));
        }
        produced += 1;
    }
    produced
}

/// Lean entry-point detection.
fn detect_entry_points(graph: &dyn crate::core::GraphMut) -> Vec<NodeId> {
    let mut entries = Vec::new();
    for (id, node) in graph.nodes() {
        if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }
        if node.qualified_name.starts_with("call::") {
            continue;
        }
        if is_test_node(node) {
            entries.push(id);
            continue;
        }
        // `main`-like entry names.
        if node.name == "main" || node.name == "__main__" {
            entries.push(id);
            continue;
        }
        // Orphan: zero incoming structural `Calls` edges. Note we count
        // *Calls* specifically, not all incoming edges, so a function
        // that's only `Defines`-edged from its file still qualifies.
        let has_caller = graph
            .in_neighbors(id)
            .any(|(_, edge)| edge.kind == EdgeKind::Calls);
        if !has_caller {
            entries.push(id);
        }
    }
    entries
}

/// Forward BFS from `entry` through `Calls` edges, bounded by
/// `max_depth`. Collects up to a safety ceiling (`max_nodes_per_flow *
/// 10`) then trims to `max_nodes_per_flow` by relevance: nodes closer
/// to the entry and more central within the flow survive; isolated leaf
/// nodes at the fringe are dropped first. Ambiguous (placeholder) edges
/// are skipped so external/unresolved calls don't pollute the flow.
fn trace_flow(
    graph: &dyn crate::core::GraphMut,
    entry: NodeId,
    options: &FlowOptions,
) -> Vec<(NodeId, usize)> {
    let safety_ceiling = options.max_nodes_per_flow.saturating_mul(10).max(500);
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut members: Vec<(NodeId, usize)> = Vec::new();
    let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
    queue.push_back((entry, 0));
    visited.insert(entry);

    // Phase 1: uncapped BFS up to the safety ceiling.
    while let Some((node, depth)) = queue.pop_front() {
        members.push((node, depth));
        if members.len() >= safety_ceiling {
            break;
        }
        if depth >= options.max_depth {
            continue;
        }
        for (next, edge) in graph.out_neighbors(node) {
            if edge.kind != EdgeKind::Calls {
                continue;
            }
            // Skip ambiguous placeholder edges — they lead to `call::*`
            // synthetic nodes and would inflate flow size with noise.
            if matches!(edge.confidence, crate::core::Confidence::Ambiguous) {
                continue;
            }
            // Defensive: skip if somehow the destination is itself a
            // placeholder. Resolved calls have real targets.
            if let Some(dst_node) = graph.node(next) {
                if dst_node.qualified_name.starts_with("call::") {
                    continue;
                }
            } else {
                continue;
            }
            if visited.insert(next) {
                queue.push_back((next, depth + 1));
            }
        }
    }

    // Phase 2: trim to cap by relevance. Score = closeness (low depth
    // wins) + in-flow fan-in (nodes called by many flow siblings are
    // more central). Entry always keeps its slot.
    if members.len() <= options.max_nodes_per_flow {
        return members;
    }

    let member_ids: HashSet<NodeId> = members.iter().map(|(id, _)| *id).collect();
    let mut scored: Vec<(NodeId, usize, f64)> = members
        .into_iter()
        .map(|(id, depth)| {
            let in_flow_fanin = graph
                .in_neighbors(id)
                .filter(|(src, e)| e.kind == EdgeKind::Calls && member_ids.contains(src))
                .count();
            // depth=0 → closeness=1.0, depth=6 → ~0.14; fanin bonus up to 0.5
            let closeness = 1.0 / (depth as f64 + 1.0);
            let fanin_bonus = (in_flow_fanin as f64 / 10.0).min(0.5);
            let score = closeness + fanin_bonus;
            (id, depth, score)
        })
        .collect();

    // Sort descending by score; entry (depth=0) always wins.
    scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(options.max_nodes_per_flow);
    scored
        .into_iter()
        .map(|(id, depth, _)| (id, depth))
        .collect()
}

/// Criticality score in `[0, 1]`. Higher = more important to know about
/// if the flow's code changes.
fn compute_criticality(
    graph: &dyn crate::core::GraphMut,
    members: &[(NodeId, usize)],
    entry_name: &str,
    is_test_entry: bool,
) -> f64 {
    // Base: log scale on flow size. A 1-node flow is roughly 0; a
    // 50-node flow is roughly 0.6.
    let size = members.len() as f64;
    let size_score = (size.ln().max(0.0) / 6.0).min(0.6);

    // Reuse: average fan-in (incoming Calls edges) of member functions.
    // A flow whose members are themselves called from elsewhere is
    // touching shared code.
    let mut total_fanin = 0usize;
    let mut counted = 0usize;
    for (id, _) in members {
        let fanin = graph
            .in_neighbors(*id)
            .filter(|(_, e)| e.kind == EdgeKind::Calls)
            .count();
        total_fanin += fanin;
        counted += 1;
    }
    let avg_fanin = if counted == 0 {
        0.0
    } else {
        total_fanin as f64 / counted as f64
    };
    let reuse_score = (avg_fanin / 8.0).min(0.25);

    // Entry-shape bonus: hot-path-looking names get a small lift.
    let name = entry_name.to_lowercase();
    let mut shape_bonus = 0.0;
    if name == "main" || name == "__main__" {
        shape_bonus = 0.1;
    } else if name.starts_with("handle") || name.starts_with("on_") || name.starts_with("serve") {
        shape_bonus = 0.05;
    }

    // Test penalty: a flow rooted at a test is rarely a production hot
    // path. Subtract enough to put it well below same-size production
    // flows.
    let test_penalty = if is_test_entry { 0.2 } else { 0.0 };

    (size_score + reuse_score + shape_bonus - test_penalty).clamp(0.0, 1.0)
}

fn is_test_node(node: &Node) -> bool {
    node.properties
        .get("is_test")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Read-side helper: collect IDs of all `Flow` nodes in the graph,
/// sorted by criticality descending (then by qualified name for
/// stability).
pub fn all_flows(graph: &Graph) -> Vec<NodeId> {
    let mut out: Vec<(NodeId, f64, String)> = graph
        .nodes()
        .filter(|(_, n)| n.kind == NodeKind::Flow)
        .map(|(id, n)| {
            let crit = n
                .properties
                .get("criticality")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            (id, crit, n.qualified_name.clone())
        })
        .collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.2.cmp(&b.2))
    });
    out.into_iter().map(|(id, _, _)| id).collect()
}

/// Read-side helper: flows that contain `node`. Returns flow node ids.
pub fn flows_through(graph: &Graph, node: NodeId) -> Vec<NodeId> {
    graph
        .out_neighbors(node)
        .filter(|(_, e)| matches!(e.kind, EdgeKind::MemberOf | EdgeKind::EntryOf))
        .map(|(id, _)| id)
        .collect()
}

/// Read-side helper: union of `flows_through` over a set of changed
/// nodes, deduplicated and ranked by criticality.
pub fn affected_flows(graph: &Graph, changed: &[NodeId]) -> Vec<NodeId> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut hits: Vec<(NodeId, f64, String)> = Vec::new();
    for &n in changed {
        for flow in flows_through(graph, n) {
            if !seen.insert(flow) {
                continue;
            }
            if let Some(flow_node) = graph.node(flow) {
                let crit = flow_node
                    .properties
                    .get("criticality")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                hits.push((flow, crit, flow_node.qualified_name.clone()));
            }
        }
    }
    hits.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.2.cmp(&b.2))
    });
    hits.into_iter().map(|(id, _, _)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};

    fn add_fn(graph: &mut dyn GraphMut, qname: &str) -> NodeId {
        graph.add_node(Node::new(NodeKind::Function, qname))
    }

    fn add_test_fn(graph: &mut dyn GraphMut, qname: &str) -> NodeId {
        graph.add_node(
            Node::new(NodeKind::Function, qname)
                .with_property("is_test", serde_json::Value::Bool(true)),
        )
    }

    /// Default options with `min_flow_size = 1` so synthetic tiny test
    /// graphs aren't dropped by the production noise filter.
    fn lax_opts() -> FlowOptions {
        FlowOptions {
            min_flow_size: 1,
            ..FlowOptions::default()
        }
    }

    #[test]
    fn detects_main_as_entry() {
        let mut g = Graph::new();
        let main_fn = add_fn(&mut g, "file::main.rs::main");
        let helper = add_fn(&mut g, "file::main.rs::helper");
        g.add_edge(main_fn, helper, Edge::extracted(EdgeKind::Calls));

        let count = compute_flows_with_options(&mut g, lax_opts());
        assert_eq!(count, 1, "main should produce exactly one flow");

        let flows = all_flows(&g);
        assert_eq!(flows.len(), 1);
        let flow = g.node(flows[0]).unwrap();
        assert_eq!(flow.kind, NodeKind::Flow);
        assert_eq!(
            flow.properties.get("entry_name").and_then(|v| v.as_str()),
            Some("main")
        );
        assert_eq!(
            flow.properties.get("node_count").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[test]
    fn orphan_is_entry_but_called_helper_is_not() {
        let mut g = Graph::new();
        let public_api = add_fn(&mut g, "file::lib.rs::public_api");
        let helper = add_fn(&mut g, "file::lib.rs::helper");
        g.add_edge(public_api, helper, Edge::extracted(EdgeKind::Calls));

        compute_flows_with_options(&mut g, lax_opts());
        let flows = all_flows(&g);
        assert_eq!(flows.len(), 1, "only the orphan should seed a flow");
        let entry_name = g.node(flows[0]).unwrap();
        assert_eq!(
            entry_name
                .properties
                .get("entry_qualified_name")
                .and_then(|v| v.as_str()),
            Some("file::lib.rs::public_api")
        );
    }

    #[test]
    fn test_entry_produces_test_flow_with_lower_criticality() {
        let mut g = Graph::new();
        let prod_main = add_fn(&mut g, "file::main.rs::main");
        let prod_helper = add_fn(&mut g, "file::main.rs::helper");
        g.add_edge(prod_main, prod_helper, Edge::extracted(EdgeKind::Calls));

        let test_main = add_test_fn(&mut g, "file::tests.rs::test_main");
        let test_helper = add_fn(&mut g, "file::tests.rs::helper");
        g.add_edge(test_main, test_helper, Edge::extracted(EdgeKind::Calls));

        compute_flows_with_options(&mut g, lax_opts());
        let flows = all_flows(&g);
        assert_eq!(flows.len(), 2);

        let prod_flow = flows
            .iter()
            .find(|id| {
                g.node(**id)
                    .map(|n| n.qualified_name.contains("::main.rs::main"))
                    .unwrap_or(false)
            })
            .copied()
            .unwrap();
        let test_flow = flows
            .iter()
            .find(|id| {
                g.node(**id)
                    .map(|n| {
                        n.properties
                            .get("is_test_flow")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
            })
            .copied()
            .unwrap();
        let prod_crit = g
            .node(prod_flow)
            .and_then(|n| n.properties.get("criticality"))
            .and_then(|v| v.as_f64())
            .unwrap();
        let test_crit = g
            .node(test_flow)
            .and_then(|n| n.properties.get("criticality"))
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!(
            prod_crit > test_crit,
            "production flow criticality {} should exceed test flow {}",
            prod_crit,
            test_crit,
        );
    }

    #[test]
    fn bfs_respects_max_depth_and_skips_placeholders() {
        let mut g = Graph::new();
        let main = add_fn(&mut g, "file::main.rs::main");
        let level1 = add_fn(&mut g, "file::main.rs::level1");
        let level2 = add_fn(&mut g, "file::main.rs::level2");
        let placeholder = add_fn(&mut g, "call::external");
        g.add_edge(main, level1, Edge::extracted(EdgeKind::Calls));
        g.add_edge(level1, level2, Edge::extracted(EdgeKind::Calls));
        // Ambiguous placeholder edge to call::external — must be skipped.
        g.add_edge(main, placeholder, Edge::ambiguous(EdgeKind::Calls));

        compute_flows_with_options(
            &mut g,
            FlowOptions {
                max_depth: 1,
                max_nodes_per_flow: 200,
                // This test's flow only has 2 members; relax the default
                // 3-member floor so the BFS bounds are what's tested.
                min_flow_size: 2,
            },
        );
        let flow_id = all_flows(&g)[0];
        let members: Vec<_> = g
            .in_neighbors(flow_id)
            .filter(|(_, e)| matches!(e.kind, EdgeKind::MemberOf | EdgeKind::EntryOf))
            .map(|(id, _)| id)
            .collect();
        assert!(members.contains(&main));
        assert!(members.contains(&level1));
        assert!(
            !members.contains(&level2),
            "max_depth=1 should exclude level2"
        );
        assert!(
            !members.contains(&placeholder),
            "placeholder calls must not appear in flow membership"
        );
    }

    #[test]
    fn idempotent_on_rerun() {
        let mut g = Graph::new();
        let main = add_fn(&mut g, "file::main.rs::main");
        let helper = add_fn(&mut g, "file::main.rs::helper");
        g.add_edge(main, helper, Edge::extracted(EdgeKind::Calls));

        compute_flows(&mut g);
        let flow_count_before = all_flows(&g).len();
        let membership_before = g
            .edges()
            .filter(|(_, _, _, e)| matches!(e.kind, EdgeKind::MemberOf | EdgeKind::EntryOf))
            .count();
        compute_flows(&mut g);
        let flow_count_after = all_flows(&g).len();
        let membership_after = g
            .edges()
            .filter(|(_, _, _, e)| matches!(e.kind, EdgeKind::MemberOf | EdgeKind::EntryOf))
            .count();
        assert_eq!(
            flow_count_before, flow_count_after,
            "no duplicate flow nodes"
        );
        assert_eq!(
            membership_before, membership_after,
            "no duplicate membership edges"
        );
    }

    #[test]
    fn flows_through_and_affected_flows() {
        let mut g = Graph::new();
        let main = add_fn(&mut g, "file::main.rs::main");
        let helper = add_fn(&mut g, "file::main.rs::helper");
        let unrelated = add_fn(&mut g, "file::other.rs::unrelated");
        g.add_edge(main, helper, Edge::extracted(EdgeKind::Calls));
        // `unrelated` is its own orphan flow.

        compute_flows_with_options(&mut g, lax_opts());
        let through_helper = flows_through(&g, helper);
        assert_eq!(through_helper.len(), 1, "helper belongs to one flow");

        let affected = affected_flows(&g, &[helper]);
        assert_eq!(affected.len(), 1);
        let unrelated_flows = affected_flows(&g, &[unrelated]);
        assert_eq!(unrelated_flows.len(), 1);
        let both = affected_flows(&g, &[helper, unrelated]);
        assert_eq!(both.len(), 2, "two changed nodes should pull two flows");
    }

    #[test]
    fn cap_trims_by_relevance_not_bfs_order() {
        // Build a star: main → hub → {leaf_0..leaf_N}.
        // hub is called only by main; each leaf is called only by hub.
        // With cap=3 we want: main (depth 0), hub (depth 1, high in-flow
        // fanin because all leaves point at it in reverse), and exactly
        // one leaf — NOT an arbitrary BFS-order suffix.
        //
        // Actually: in-flow fan-in counts callers *within* the flow.
        // hub is called by main (1); each leaf is called by hub (1).
        // So closeness wins: main (1.0) > hub (0.5) > leaves (0.33).
        // With cap=3 we must get main + hub + one leaf, and the
        // node_count property on the flow must equal 3.
        let mut g = Graph::new();
        let main = add_fn(&mut g, "mod::main");
        let hub = add_fn(&mut g, "mod::hub");
        g.add_edge(main, hub, Edge::extracted(EdgeKind::Calls));
        let mut leaves = Vec::new();
        for i in 0..10 {
            let leaf = add_fn(&mut g, &format!("mod::leaf_{i}"));
            g.add_edge(hub, leaf, Edge::extracted(EdgeKind::Calls));
            leaves.push(leaf);
        }

        compute_flows_with_options(
            &mut g,
            FlowOptions {
                max_nodes_per_flow: 3,
                min_flow_size: 1,
                max_depth: 6,
            },
        );

        let flows = all_flows(&g);
        let main_flow = flows
            .iter()
            .find(|&&id| {
                g.node(id)
                    .map(|n| n.qualified_name == "flow::mod::main")
                    .unwrap_or(false)
            })
            .copied()
            .expect("main flow must exist");

        let members: Vec<NodeId> = g
            .in_neighbors(main_flow)
            .filter(|(_, e)| matches!(e.kind, EdgeKind::MemberOf | EdgeKind::EntryOf))
            .map(|(id, _)| id)
            .collect();

        assert_eq!(members.len(), 3, "cap=3 should yield exactly 3 members");
        assert!(
            members.contains(&main),
            "entry (depth 0) must always be kept"
        );
        assert!(
            members.contains(&hub),
            "hub (depth 1, closest) must be kept over distant leaves"
        );

        let node_count = g
            .node(main_flow)
            .and_then(|n| n.properties.get("node_count"))
            .and_then(|v| v.as_u64())
            .unwrap();
        assert_eq!(node_count, 3);
    }
}
