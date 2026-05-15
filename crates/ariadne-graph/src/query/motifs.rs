//! Subgraph motif matching (Phase 3).
//!
//! Plan: implement VF2-style subgraph isomorphism over the typed graph,
//! taking patterns expressed as small `(NodeKind, EdgeKind)` adjacency
//! lists. Useful queries: "function that calls `untrusted_input` and
//! later `sql_exec` without an intervening `sanitize_*` call", "diamond
//! inheritance patterns", "doc → concept → function triangles".

use crate::core::{Graph, NodeId};

#[derive(Debug, Clone, Default)]
pub struct Motif {
    // Pattern definition lands here once the DSL stabilises.
    pub _placeholder: (),
}

pub fn find_motifs(_graph: &Graph, _motif: &Motif) -> Vec<Vec<NodeId>> {
    Vec::new()
}
