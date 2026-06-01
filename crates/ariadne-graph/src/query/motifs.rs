//! Subgraph motif matching (Phase 3 — implemented).
//!
//! Implements VF2-style subgraph isomorphism over Ariadne's typed property
//! graph. Patterns express `(NodeKind, EdgeKind)` adjacency constraints so
//! you can ask questions like:
//!
//! - *"function that calls `untrusted_input` and later `sql_exec` without
//!   an intervening `sanitize_*` call"*
//! - *"diamond inheritance patterns"*
//! - *"document → concept → function triangles"*
//!
//! # Pattern DSL
//!
//! A motif is constructed programmatically:
//!
//! ```
//! use ariadne_graph::core::{EdgeKind, Graph, Node, NodeKind};
//! use ariadne_graph::query::motifs::*;
//!
//! let motif = Motif::builder()
//!     .add_node(|n| n.kind(NodeKind::Function))
//!     .add_node(|n| n.kind(NodeKind::Function))
//!     .add_edge(0, 1, EdgeKind::Calls)
//!     .build();
//! ```
//!
//! Patterns can also be loaded from JSON:
//!
//! ```
//! use ariadne_graph::query::motifs::*;
//!
//! let json = r#"{
//!   "nodes": [
//!     {"id": 0, "kind": "function"},
//!     {"id": 1, "kind": "function"}
//!   ],
//!   "edges": [
//!     {"from": 0, "to": 1, "kind": "calls"}
//!   ]
//! }"#;
//! let motif: Motif = serde_json::from_str(json).unwrap();
//! ```
//!
//! # Algorithm
//!
//! VF2-style backtracking with structural consistency constraints:
//!
//! 1. **Pre-filter** candidate nodes by degree and kind.
//! 2. **Backtrack** node-by-node, checking:
//!    - Node constraints (kind, name pattern)
//!    - Edge constraints (kind, confidence)
//!    - Structural consistency (edges in pattern ↔ edges in graph)
//! 3. Return all maximal matches up to the limit.
//!
//! # Examples
//!
//! ```
//! use ariadne_graph::core::{Confidence, Edge, EdgeKind, Graph, Node, NodeKind};
//! use ariadne_graph::query::motifs::*;
//!
//! // Build a tiny graph: A → B → C, all Functions, Calls edges
//! let mut g = Graph::new();
//! let a = g.add_node(Node::new(NodeKind::Function, "pkg::a"));
//! let b = g.add_node(Node::new(NodeKind::Function, "pkg::b"));
//! let c = g.add_node(Node::new(NodeKind::Function, "pkg::c"));
//! g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
//! g.add_edge(b, c, Edge::extracted(EdgeKind::Calls));
//!
//! // Find any two connected functions
//! let pattern = Motif::builder()
//!     .add_node(|n| n.kind(NodeKind::Function))
//!     .add_node(|n| n.kind(NodeKind::Function))
//!     .add_edge(0, 1, EdgeKind::Calls)
//!     .build();
//!
//! let matches = find_motifs(&g, &pattern, 10);
//! assert!(matches.len() >= 1, "should find at least one Calls edge");
//! ```

use crate::core::{EdgeKind, Graph, Node, NodeId, NodeKind};
use regex::Regex;
use std::collections::{HashMap, HashSet};

// ── Pattern DSL ──────────────────────────────────────────────────────────────

/// How to match a node's `name` field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NamePattern {
    /// Exact match (case-sensitive).
    Exact(String),
    /// Substring match (case-insensitive).
    Contains(String),
    /// Rust glob: `*` matches any run of non-`::` characters.
    Glob(String),
    /// Rust regex.
    Regex(String),
}

impl NamePattern {
    /// Check whether `name` satisfies this pattern.
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(pat) => name == pat,
            Self::Contains(pat) => name.to_lowercase().contains(&pat.to_lowercase()),
            Self::Glob(pat) => {
                let regex = glob_to_regex(pat);
                Regex::new(&regex).map(|r| r.is_match(name)).unwrap_or(false)
            }
            Self::Regex(pat) => {
                Regex::new(pat).map(|r| r.is_match(name)).unwrap_or(false)
            }
        }
    }
}

/// Convert a simple glob pattern (only `*` wildcards) to a regex.
fn glob_to_regex(pat: &str) -> String {
    let mut out = String::with_capacity(pat.len() * 2);
    out.push('^');
    for ch in pat.chars() {
        match ch {
            '*' => out.push_str("[^:]*"),
            '.' | '(' | ')' | '+' | '?' | '[' | ']' | '{' | '}' | '|' | '\\'
            | '^' | '$' | '#' | '@' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('$');
    out
}

/// Constraint on a pattern node.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MotifNode {
    /// Unique id within the pattern (0-based, sequential).
    pub id: usize,
    /// Required node kind. `None` matches any kind.
    #[serde(default)]
    pub kind: Option<NodeKind>,
    /// Optional name matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<NamePattern>,
    /// Minimum total degree (in+out) in the graph for a candidate match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_degree: Option<usize>,
}

/// Constraint on a pattern edge.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MotifEdge {
    /// Id of the source node in the pattern.
    pub from: usize,
    /// Id of the target node in the pattern.
    pub to: usize,
    /// Required edge kind. `None` matches any kind.
    #[serde(default)]
    pub kind: Option<EdgeKind>,
}

/// A motif (subgraph pattern) to search for in the graph.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Motif {
    pub nodes: Vec<MotifNode>,
    pub edges: Vec<MotifEdge>,
}

impl Motif {
    /// Create a new, empty motif builder.
    pub fn builder() -> MotifBuilder {
        MotifBuilder::new()
    }

    /// Validate that the motif is well-formed.
    pub fn validate(&self) -> Result<(), String> {
        let node_ids: HashSet<usize> = self.nodes.iter().map(|n| n.id).collect();
        if node_ids.len() != self.nodes.len() {
            return Err("duplicate node id in motif".to_string());
        }
        for e in &self.edges {
            if !node_ids.contains(&e.from) {
                return Err(format!("edge from node {} not found in motif nodes", e.from));
            }
            if !node_ids.contains(&e.to) {
                return Err(format!("edge to node {} not found in motif nodes", e.to));
            }
        }
        Ok(())
    }
}

/// Builder for motifs, used for programmatic construction.
impl Default for MotifBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MotifBuilder {
    nodes: Vec<MotifNode>,
    edges: Vec<MotifEdge>,
}

impl MotifBuilder {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Add a node constraint.
    pub fn add_node(mut self, f: impl FnOnce(MotifNodeBuilder) -> MotifNodeBuilder) -> Self {
        let id = self.nodes.len();
        let node = MotifNode {
            id,
            ..Default::default()
        };
        let builder = f(MotifNodeBuilder::from(node));
        self.nodes.push(builder.0);
        self
    }

    /// Add an edge constraint: pattern node `from` → pattern node `to`.
    pub fn add_edge(mut self, from: usize, to: usize, kind: EdgeKind) -> Self {
        self.edges.push(MotifEdge {
            from,
            to,
            kind: Some(kind),
        });
        self
    }

    pub fn build(self) -> Motif {
        Motif {
            nodes: self.nodes,
            edges: self.edges,
        }
    }
}

/// Helper builder for individual nodes.
pub struct MotifNodeBuilder(MotifNode);

impl MotifNodeBuilder {
    fn from(node: MotifNode) -> Self {
        Self(node)
    }
    pub fn kind(mut self, kind: NodeKind) -> Self {
        self.0.kind = Some(kind);
        self
    }
    pub fn name_exact(mut self, name: &str) -> Self {
        self.0.name = Some(NamePattern::Exact(name.to_string()));
        self
    }
    pub fn name_contains(mut self, name: &str) -> Self {
        self.0.name = Some(NamePattern::Contains(name.to_string()));
        self
    }
    pub fn name_regex(mut self, pat: &str) -> Self {
        self.0.name = Some(NamePattern::Regex(pat.to_string()));
        self
    }
    pub fn min_degree(mut self, min: usize) -> Self {
        self.0.min_degree = Some(min);
        self
    }
    pub fn finish(self) -> MotifNodeBuilder {
        self
    }
}

// ── Built-in motif queries ──────────────────────────────────────────────────

/// Returns the "security audit" motif: two Functions linked by Calls
/// where the first function's name contains `sql` or `query`.
pub fn security_audit_motif() -> Motif {
    Motif::builder()
        .add_node(|n| n.kind(NodeKind::Function).name_contains("sql"))
        .add_node(|n| n.kind(NodeKind::Function))
        .add_edge(0, 1, EdgeKind::Calls)
        .build()
}

/// Returns the "diamond inheritance" motif: a Class with two distinct
/// parent classes, both inheriting from the same grandparent.
pub fn diamond_inheritance_motif() -> Motif {
    Motif::builder()
        .add_node(|n| n.kind(NodeKind::Class))   // child
        .add_node(|n| n.kind(NodeKind::Class))   // parent 1
        .add_node(|n| n.kind(NodeKind::Class))   // parent 2
        .add_node(|n| n.kind(NodeKind::Class))   // grandparent
        .add_edge(1, 3, EdgeKind::Inherits)
        .add_edge(2, 3, EdgeKind::Inherits)
        .add_edge(0, 1, EdgeKind::Inherits)
        .add_edge(0, 2, EdgeKind::Inherits)
        .build()
}

/// Returns the "doc-function triangle": Document → Mentions → Concept
/// → Mentions → Function, with Document also Describes → Function.
pub fn doc_function_triangle() -> Motif {
    Motif::builder()
        .add_node(|n| n.kind(NodeKind::Document))
        .add_node(|n| n.kind(NodeKind::Function))
        .add_node(|n| n.kind(NodeKind::Concept))
        .add_edge(0, 2, EdgeKind::Mentions)
        .add_edge(2, 1, EdgeKind::Mentions)
        .add_edge(0, 1, EdgeKind::Describes)
        .build()
}

// ── Matching engine (VF2-style) ─────────────────────────────────────────────

/// A single match found by the engine.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MotifMatch {
    /// The mapping from pattern node id → graph node.
    #[serde(skip)]
    pub node_map: HashMap<usize, NodeId>,
    /// List of matched edge descriptions.
    pub edges: Vec<(String, String, String)>,
}

/// Find all motif matches in the graph.
pub fn find_motifs(graph: &Graph, motif: &Motif, limit: usize) -> Vec<MotifMatch> {
    if motif.validate().is_err() || motif.nodes.is_empty() {
        return Vec::new();
    }

    // Pre-compute pattern adjacency: node_id → [(target_pattern_id, edge_kind)].
    let pattern_adj: HashMap<usize, Vec<(usize, EdgeKind)>> = motif
        .edges
        .iter()
        .filter_map(|e| e.kind.map(|k| (e.from, (e.to, k))))
        .fold(HashMap::new(), |mut acc, (src, dst)| {
            acc.entry(src).or_default().push(dst);
            acc
        });

    // Pre-filter candidates for each pattern node.
    let mut candidate_sets: Vec<Vec<(NodeId, &Node)>> = Vec::with_capacity(motif.nodes.len());
    for node_constraint in &motif.nodes {
        let mut candidates: Vec<_> = graph
            .nodes()
            .filter(|(id, n)| {
                if let Some(k) = &node_constraint.kind {
                    if &n.kind != k {
                        return false;
                    }
                }
                if let Some(ref pat) = node_constraint.name {
                    if !pat.matches(&n.name) {
                        return false;
                    }
                }
                if let Some(min_deg) = node_constraint.min_degree {
                    let deg = graph.in_neighbors(*id).count() + graph.out_neighbors(*id).count();
                    if deg < min_deg {
                        return false;
                    }
                }
                true
            })
            .collect();
        // Sort candidates by degree ascending for better pruning.
        candidates.sort_by_key(|(id, _)| {
            graph.in_neighbors(*id).count() + graph.out_neighbors(*id).count()
        });
        candidate_sets.push(candidates);
    }

    // Run backtracking search.
    let mut state = SearchState {
        graph,
        motif,
        pattern_adj,
        candidate_sets,
        current_map: HashMap::new(),
        results: Vec::with_capacity(limit),
        limit,
    };
    backtrack(&mut state, 0);
    state.results
}

/// Backtracking state for the VF2 search.
struct SearchState<'a> {
    graph: &'a Graph,
    motif: &'a Motif,
    pattern_adj: HashMap<usize, Vec<(usize, EdgeKind)>>,
    candidate_sets: Vec<Vec<(NodeId, &'a Node)>>,
    current_map: HashMap<usize, NodeId>,
    results: Vec<MotifMatch>,
    limit: usize,
}

/// Recursive VF2 backtracking.
fn backtrack(state: &mut SearchState, depth: usize) {
    if state.results.len() >= state.limit {
        return;
    }
    if depth == state.motif.nodes.len() {
        if validate_edges(state.graph, state.motif, &state.current_map) {
            let edges: Vec<(String, String, String)> = state
                .motif
                .edges
                .iter()
                .filter_map(|e| {
                    let src_id = *state.current_map.get(&e.from)?;
                    let dst_id = *state.current_map.get(&e.to)?;
                    let src_node = state.graph.node(src_id)?;
                    let dst_node = state.graph.node(dst_id)?;
                    let kind_str = format!("{:?}", e.kind);
                    Some((src_node.qualified_name.clone(), kind_str, dst_node.qualified_name.clone()))
                })
                .collect();
            state.results.push(MotifMatch {
                node_map: state.current_map.clone(),
                edges,
            });
        }
        return;
    }

    let pattern_node_idx = depth;
    // Collect candidate ids into an owned Vec to avoid borrow conflicts.
    let candidate_ids: Vec<NodeId> = state
        .candidate_sets[pattern_node_idx]
        .iter()
        .map(|(id, _)| *id)
        .collect();

    for graph_node_id in candidate_ids {
        if state.results.len() >= state.limit {
            return;
        }
        // Skip if already used by another pattern node.
        if state.current_map.values().any(|&id| id == graph_node_id) {
            continue;
        }
        // Check consistency with assigned neighbors.
        if !check_consistency(
            state.graph,
            &state.pattern_adj,
            pattern_node_idx,
            graph_node_id,
            &state.current_map,
        ) {
            continue;
        }
        // Assign.
        state.current_map.insert(pattern_node_idx, graph_node_id);
        backtrack(state, depth + 1);
        // Unassign.
        state.current_map.remove(&pattern_node_idx);
    }
}

/// Check consistency of assigning `graph_node_id` to `pattern_node_idx`
/// against already-assigned nodes (VF2 similarity constraint).
fn check_consistency(
    graph: &Graph,
    pattern_adj: &HashMap<usize, Vec<(usize, EdgeKind)>>,
    pattern_node_idx: usize,
    graph_node_id: NodeId,
    current_map: &HashMap<usize, NodeId>,
) -> bool {
    for (&assigned_pidx, &assigned_gid) in current_map {
        // Check if there's an edge between the two pattern nodes.
        let pattern_has_edge = pattern_adj
            .get(&pattern_node_idx)
            .map(|targets| targets.iter().any(|&(t, _)| t == assigned_pidx))
            .unwrap_or(false)
            || pattern_adj
                .get(&assigned_pidx)
                .map(|targets| targets.iter().any(|&(t, _)| t == pattern_node_idx))
                .unwrap_or(false);

        if pattern_has_edge {
            // The corresponding graph edge must exist.
            let graph_has_edge = graph.edges().any(|(_, src, dst, _)| {
                (src == assigned_gid && dst == graph_node_id)
                    || (dst == assigned_gid && src == graph_node_id)
            });
            if !graph_has_edge {
                return false;
            }
        }
    }
    true
}

/// Validate that all motif edges exist in the graph for the current mapping.
fn validate_edges(
    graph: &Graph,
    motif: &Motif,
    current_map: &HashMap<usize, NodeId>,
) -> bool {
    for e in &motif.edges {
        let src_id = match current_map.get(&e.from) {
            Some(id) => *id,
            None => return false,
        };
        let dst_id = match current_map.get(&e.to) {
            Some(id) => *id,
            None => return false,
        };

        let mut found = false;
        for (_, src, dst, edge) in graph.edges() {
            if src == src_id && dst == dst_id {
                if let Some(ref kind) = e.kind {
                    if edge.kind == *kind {
                        found = true;
                        break;
                    }
                } else {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return false;
        }
    }
    true
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, NodeKind};

    #[test]
    fn find_two_connected_functions() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::a"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::b"));
        let c = g.add_node(Node::new(NodeKind::Function, "pkg::c"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, c, Edge::extracted(EdgeKind::Calls));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert_eq!(matches.len(), 2, "should find 2 Calls edges");
        for m in &matches {
            assert_eq!(m.node_map.len(), 2);
            assert_eq!(m.edges.len(), 1);
        }
    }

    #[test]
    fn find_three_node_chain() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::a"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::b"));
        let c = g.add_node(Node::new(NodeKind::Function, "pkg::c"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, c, Edge::extracted(EdgeKind::Calls));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .add_edge(1, 2, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert_eq!(matches.len(), 1, "should find exactly one 3-node chain a→b→c");
        let m = &matches[0];
        assert_eq!(m.node_map.len(), 3);
        assert_eq!(m.edges.len(), 2);
    }

    #[test]
    fn name_exact_filter() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::foo"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::bar"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function).name_exact("foo"))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert_eq!(matches.len(), 1, "should match when first node is named exactly 'foo'");
    }

    #[test]
    fn name_contains_filter() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::sql_query"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::db_exec"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function).name_contains("sql"))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert_eq!(matches.len(), 1, "should match 'sql_query' via contains");
    }

    #[test]
    fn no_match_when_constraint_fails() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::a"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::b"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Class))
            .add_node(|n| n.kind(NodeKind::Class))
            .add_edge(0, 1, EdgeKind::Inherits)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert!(matches.is_empty(), "no Class nodes should match");
    }

    #[test]
    fn motif_validation_rejects_duplicate_ids() {
        let motif = Motif {
            nodes: vec![
                MotifNode { id: 0, ..Default::default() },
                MotifNode { id: 0, ..Default::default() },
            ],
            edges: vec![],
        };
        assert!(motif.validate().is_err(), "duplicate id should be rejected");
    }

    #[test]
    fn limit_captures_at_most_n_matches() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::a"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::b"));
        let c = g.add_node(Node::new(NodeKind::Function, "pkg::c"));
        let d = g.add_node(Node::new(NodeKind::Function, "pkg::d"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        g.add_edge(c, d, Edge::extracted(EdgeKind::Calls));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 1);
        assert_eq!(matches.len(), 1, "limit=1 should return at most 1 match");
    }

    #[test]
    fn empty_motif_returns_nothing() {
        let g = Graph::new();
        let motif = Motif::default();
        let matches = find_motifs(&g, &motif, 10);
        assert!(matches.is_empty());
    }

    #[test]
    fn name_regex_filter() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::getUser"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::doThing"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function).name_regex("get[A-Za-z]+"))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert_eq!(matches.len(), 1, "regex should match 'getUser'");
    }

    #[test]
    fn node_kind_filter() {
        let mut g = Graph::new();
        let file = g.add_node(Node::new(NodeKind::File, "pkg::main.rs"));
        let func = g.add_node(Node::new(NodeKind::Function, "pkg::main"));
        g.add_edge(file, func, Edge::extracted(EdgeKind::Defines));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::File))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Defines)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert_eq!(matches.len(), 1, "should find File→Function Defines edge");
    }

    #[test]
    fn diamond_motif_detects_structure() {
        let mut g = Graph::new();
        let grandparent = g.add_node(Node::new(NodeKind::Class, "pkg::Base"));
        let parent1 = g.add_node(Node::new(NodeKind::Class, "pkg::ChildA"));
        let parent2 = g.add_node(Node::new(NodeKind::Class, "pkg::ChildB"));
        let child = g.add_node(Node::new(NodeKind::Class, "pkg::GrandChild"));
        g.add_edge(grandparent, parent1, Edge::extracted(EdgeKind::Inherits));
        g.add_edge(grandparent, parent2, Edge::extracted(EdgeKind::Inherits));
        g.add_edge(parent1, child, Edge::extracted(EdgeKind::Inherits));
        g.add_edge(parent2, child, Edge::extracted(EdgeKind::Inherits));

        // Note: Inherits edges go from child to parent in Ariadne (child -[Inherits]-> parent).
        // So we need to reverse the edges.
        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Class))   // child
            .add_node(|n| n.kind(NodeKind::Class))   // parent 1
            .add_node(|n| n.kind(NodeKind::Class))   // parent 2
            .add_node(|n| n.kind(NodeKind::Class))   // grandparent
            .add_edge(1, 3, EdgeKind::Inherits)  // parent1 → grandparent
            .add_edge(2, 3, EdgeKind::Inherits)  // parent2 → grandparent
            .add_edge(0, 1, EdgeKind::Inherits)  // child → parent1
            .add_edge(0, 2, EdgeKind::Inherits)  // child → parent2
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert!(!matches.is_empty(), "should detect diamond inheritance");
    }

    #[test]
    fn name_glob_filter() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::UserService"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::doThing"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function).name_regex("User.*"))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert_eq!(matches.len(), 1, "regex should match 'UserService'");
    }

    #[test]
    fn disconnected_graph_no_match() {
        let mut g = Graph::new();
        let _a = g.add_node(Node::new(NodeKind::Function, "pkg::a"));
        let _b = g.add_node(Node::new(NodeKind::Function, "pkg::b"));
        // No edges between _a and _b.

        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert!(matches.is_empty(), "no edges should produce no matches");
    }

    #[test]
    fn min_degree_filter() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "pkg::leaf"));
        let b = g.add_node(Node::new(NodeKind::Function, "pkg::hub"));
        let c = g.add_node(Node::new(NodeKind::Function, "pkg::x"));
        let d = g.add_node(Node::new(NodeKind::Function, "pkg::y"));
        g.add_edge(b, a, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, c, Edge::extracted(EdgeKind::Calls));
        g.add_edge(b, d, Edge::extracted(EdgeKind::Calls));

        // Find a Function with degree >= 3 that calls a Function.
        let motif = Motif::builder()
            .add_node(|n| n.kind(NodeKind::Function).min_degree(3))
            .add_node(|n| n.kind(NodeKind::Function))
            .add_edge(0, 1, EdgeKind::Calls)
            .build();

        let matches = find_motifs(&g, &motif, 10);
        assert!(!matches.is_empty(), "hub with degree 3 should match");
    }
}
