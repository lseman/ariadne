use crate::{Edge, EdgeId, Node, NodeId};
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use petgraph::Direction;
use std::collections::HashMap;

/// Rebuild the by_qname index from the inner graph.
fn rebuild_qname_index(inner: &StableDiGraph<Node, Edge>) -> HashMap<String, NodeIndex> {
    inner
        .node_indices()
        .filter_map(|idx| {
            let node = &inner[idx];
            if node.qualified_name.is_empty() {
                None
            } else {
                Some((node.qualified_name.clone(), idx))
            }
        })
        .collect()
}

/// In-memory property graph backed by `petgraph::StableDiGraph`.
///
/// Maintains a secondary index from `qualified_name` to node, which is
/// what symbol-resolution queries hit first.
#[derive(Debug, Default, Clone)]
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

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.inner.node_weight_mut(NodeIndex::new(id.0 as usize))
    }

    /// Rename a node by updating its qualified name and the lookup index.
    pub fn rename_node(&mut self, id: NodeId, new_qn: &str, new_name: &str) {
        if let Some(node) = self.inner.node_weight_mut(NodeIndex::new(id.0 as usize)) {
            // Remove old QN from index.
            self.by_qname.remove(&node.qualified_name);
            node.qualified_name = new_qn.to_string();
            node.name = new_name.to_string();
            // Insert new QN into index.
            self.by_qname.insert(new_qn.to_string(), NodeIndex::new(id.0 as usize));
        }
    }

    pub fn edge(&self, id: EdgeId) -> Option<&Edge> {
        self.inner.edge_weight(EdgeIndex::new(id.0 as usize))
    }

    /// Convert an EdgeId to a petgraph EdgeIndex for mutation.
    pub fn edge_index(&self, id: EdgeId) -> Option<EdgeIndex> {
        self.inner
            .edge_indices()
            .find(|ei| ei.index() == id.0 as usize)
    }

    /// Remove an edge by index. Rebuilds the qname index.
    pub fn remove_edge(&mut self, idx: EdgeIndex) {
        self.inner.remove_edge(idx);
        self.by_qname = rebuild_qname_index(&self.inner);
    }

    pub fn edge_mut(&mut self, id: EdgeId) -> Option<&mut Edge> {
        self.inner.edge_weight_mut(EdgeIndex::new(id.0 as usize))
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
