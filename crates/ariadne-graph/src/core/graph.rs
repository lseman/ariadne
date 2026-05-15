use crate::{Edge, EdgeId, Node, NodeId};
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use petgraph::Direction;
use std::collections::HashMap;

/// In-memory property graph backed by `petgraph::StableDiGraph`.
///
/// Maintains a secondary index from `qualified_name` to node, which is
/// what symbol-resolution queries hit first.
#[derive(Debug, Default)]
pub struct Graph {
    inner: StableDiGraph<Node, Edge>,
    by_qname: HashMap<String, NodeIndex>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, node: Node) -> NodeId {
        let qname = node.qualified_name.clone();
        if let Some(&existing) = self.by_qname.get(&qname) {
            // Same qualified name → update in place rather than duplicate.
            self.inner[existing] = node;
            return NodeId(existing.index() as u32);
        }
        let idx = self.inner.add_node(node);
        self.by_qname.insert(qname, idx);
        NodeId(idx.index() as u32)
    }

    pub fn add_edge(&mut self, src: NodeId, dst: NodeId, edge: Edge) -> EdgeId {
        let s = NodeIndex::new(src.0 as usize);
        let d = NodeIndex::new(dst.0 as usize);
        let e = self.inner.add_edge(s, d, edge);
        EdgeId(e.index() as u32)
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.inner.node_weight(NodeIndex::new(id.0 as usize))
    }

    pub fn edge(&self, id: EdgeId) -> Option<&Edge> {
        self.inner.edge_weight(EdgeIndex::new(id.0 as usize))
    }

    pub fn find_by_qname(&self, qname: &str) -> Option<NodeId> {
        self.by_qname.get(qname).map(|i| NodeId(i.index() as u32))
    }

    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> + '_ {
        self.inner
            .node_indices()
            .map(move |i| (NodeId(i.index() as u32), &self.inner[i]))
    }

    pub fn edges(&self) -> impl Iterator<Item = (EdgeId, NodeId, NodeId, &Edge)> + '_ {
        self.inner.edge_references().map(|e| {
            (
                EdgeId(e.id().index() as u32),
                NodeId(e.source().index() as u32),
                NodeId(e.target().index() as u32),
                e.weight(),
            )
        })
    }

    pub fn out_neighbors(&self, id: NodeId) -> impl Iterator<Item = (NodeId, &Edge)> + '_ {
        let idx = NodeIndex::new(id.0 as usize);
        self.inner
            .edges_directed(idx, Direction::Outgoing)
            .map(|e| (NodeId(e.target().index() as u32), e.weight()))
    }

    pub fn in_neighbors(&self, id: NodeId) -> impl Iterator<Item = (NodeId, &Edge)> + '_ {
        let idx = NodeIndex::new(id.0 as usize);
        self.inner
            .edges_directed(idx, Direction::Incoming)
            .map(|e| (NodeId(e.source().index() as u32), e.weight()))
    }

    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.inner.edge_count()
    }

    pub fn inner(&self) -> &StableDiGraph<Node, Edge> {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EdgeKind, NodeKind};

    #[test]
    fn add_and_traverse() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "m::f"));
        let b = g.add_node(Node::new(NodeKind::Function, "m::g"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        assert_eq!(g.node_count(), 2);
        assert_eq!(g.edge_count(), 1);
        let callees: Vec<_> = g.out_neighbors(a).collect();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].0, b);
    }

    #[test]
    fn duplicate_qname_merges() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "m::f"));
        let a2 = g.add_node(Node::new(NodeKind::Function, "m::f"));
        assert_eq!(a, a2);
        assert_eq!(g.node_count(), 1);
    }
}
