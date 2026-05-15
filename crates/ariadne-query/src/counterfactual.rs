//! Counterfactual reasoning (Phase 3).
//!
//! Plan: clone the in-memory graph, drop the supplied edges, rerun a
//! query, and diff the result. Answers "if I delete this function /
//! sever this dependency, what stops being reachable?" with actual
//! reachability math rather than the conservative blast-radius
//! approximation used by code-review-graph.

use ariadne_core::{EdgeId, Graph};

pub fn run_without_edges(_graph: &Graph, _drop: &[EdgeId]) -> Graph {
    // Placeholder: full implementation will require an owned clone of
    // the in-memory graph and a re-built qname index.
    Graph::new()
}
