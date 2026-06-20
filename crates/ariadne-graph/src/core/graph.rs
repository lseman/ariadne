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
    ///
    /// Returns the `NodeId` that now carries `new_qn`. Usually that is `id`,
    /// but if `new_qn` already belongs to a *different* node (e.g. two
    /// markdown headings that slugify identically) the two are merged:
    /// `id`'s incident edges are rewired onto the existing node, `id` is
    /// removed, and the existing node's id is returned. This preserves the
    /// invariant that `qualified_name` is unique within the graph.
    pub fn rename_node(&mut self, id: NodeId, new_qn: &str, new_name: &str) -> NodeId {
        let idx = NodeIndex::new(id.0 as usize);
        if self.inner.node_weight(idx).is_none() {
            return id;
        }

        // Collision: a different node already owns `new_qn`. Merge `id` into
        // it rather than creating a duplicate qualified_name.
        if let Some(&existing) = self.by_qname.get(new_qn) {
            if existing != idx {
                self.merge_into(idx, existing);
                return NodeId(existing.index() as u32);
            }
            // Same node already at new_qn — only the display name may differ.
            self.inner[idx].name = new_name.to_string();
            return id;
        }

        let old_qn = self.inner[idx].qualified_name.clone();
        self.by_qname.remove(&old_qn);
        let node = &mut self.inner[idx];
        node.qualified_name = new_qn.to_string();
        node.name = new_name.to_string();
        self.by_qname.insert(new_qn.to_string(), idx);
        id
    }

    /// Rewire `loser`'s incident edges onto `winner`, then remove `loser`.
    /// Skips self-loops and edges that would duplicate an existing
    /// (src, dst, kind) triple on `winner`.
    fn merge_into(&mut self, loser: NodeIndex, winner: NodeIndex) {
        // Collect loser's incident edges before mutating the graph.
        let incoming: Vec<(NodeIndex, Edge)> = self
            .inner
            .edges_directed(loser, Direction::Incoming)
            .map(|e| (e.source(), e.weight().clone()))
            .collect();
        let outgoing: Vec<(NodeIndex, Edge)> = self
            .inner
            .edges_directed(loser, Direction::Outgoing)
            .map(|e| (e.target(), e.weight().clone()))
            .collect();

        for (src, edge) in incoming {
            let new_src = if src == loser { winner } else { src };
            if new_src == winner {
                continue; // self-loop
            }
            if self.has_edge_kind(new_src, winner, &edge) {
                continue;
            }
            self.inner.add_edge(new_src, winner, edge);
        }
        for (dst, edge) in outgoing {
            let new_dst = if dst == loser { winner } else { dst };
            if winner == new_dst {
                continue; // self-loop
            }
            if self.has_edge_kind(winner, new_dst, &edge) {
                continue;
            }
            self.inner.add_edge(winner, new_dst, edge);
        }

        let loser_qn = self.inner[loser].qualified_name.clone();
        self.inner.remove_node(loser);
        // remove_node on a StableDiGraph keeps other indices stable, so the
        // qname index only needs the loser's entry cleared. The winner's
        // entry (already pointing at `winner`) is untouched.
        self.by_qname.remove(&loser_qn);
    }

    /// Whether an edge of `edge.kind` already exists from `src` to `dst`.
    fn has_edge_kind(&self, src: NodeIndex, dst: NodeIndex, edge: &Edge) -> bool {
        self.inner
            .edges_directed(src, Direction::Outgoing)
            .any(|e| e.target() == dst && e.weight().kind == edge.kind)
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

    /// Remove an edge by index.
    pub fn remove_edge(&mut self, idx: EdgeIndex) {
        self.inner.remove_edge(idx);
    }

    /// Remove a batch of edges by stable id.
    pub fn remove_edges_by_id(&mut self, ids: &[EdgeId]) {
        for id in ids {
            self.inner.remove_edge(EdgeIndex::new(id.0 as usize));
        }
    }

    /// Remove a batch of nodes (and their incident edges) by stable id.
    /// Rebuilds the qname index once.
    pub fn remove_nodes_by_id(&mut self, ids: &[NodeId]) {
        for id in ids {
            let idx = NodeIndex::new(id.0 as usize);
            if self.inner.contains_node(idx) {
                self.inner.remove_node(idx);
            }
        }
        if !ids.is_empty() {
            self.by_qname = rebuild_qname_index(&self.inner);
        }
    }

    /// Remove a node and all its incident edges. Rebuilds the qname index.
    pub fn remove_node(&mut self, id: NodeId) {
        let idx = NodeIndex::new(id.0 as usize);
        if self.inner.contains_node(idx) {
            // Collect qname before removing so we can update the index
            if let Some(node) = self.inner.node_weight(idx) {
                self.by_qname.remove(&node.qualified_name);
            }
            self.inner.remove_node(idx);
            self.by_qname = rebuild_qname_index(&self.inner);
        }
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
    fn rename_into_existing_qname_merges_and_rewires() {
        let mut g = Graph::new();
        let parent = g.add_node(Node::new(NodeKind::Section, "doc::f::a"));
        let s1 = g.add_node(Node::new(NodeKind::Section, "doc::f::section-0"));
        let s2 = g.add_node(Node::new(NodeKind::Section, "doc::f::section-1"));
        g.add_edge(parent, s1, Edge::extracted(EdgeKind::Defines));
        g.add_edge(parent, s2, Edge::extracted(EdgeKind::Defines));

        // First section claims the slug.
        let r1 = g.rename_node(s1, "doc::f::dup", "dup");
        assert_eq!(r1, s1);
        // Second section renames into the same slug → merge into s1.
        let r2 = g.rename_node(s2, "doc::f::dup", "dup");
        assert_eq!(r2, s1, "collision should return the surviving node");

        assert_eq!(g.node_count(), 2, "s2 should have been removed");
        assert!(
            g.node(s2).is_none()
                || g.node(s2)
                    .map(|n| n.qualified_name != "doc::f::section-1")
                    .unwrap_or(true)
        );
        assert_eq!(g.find_by_qname("doc::f::dup"), Some(s1));
        // The parent's two Defines edges collapse to one (dedup on rewire).
        let defines: Vec<_> = g
            .out_neighbors(parent)
            .filter(|(_, e)| e.kind == EdgeKind::Defines)
            .collect();
        assert_eq!(defines.len(), 1, "duplicate Defines edge should be skipped");
        assert_eq!(defines[0].0, s1);
    }

    #[test]
    fn duplicate_qname_merges() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "m::f"));
        let a2 = g.add_node(Node::new(NodeKind::Function, "m::f"));
        assert_eq!(a, a2);
        assert_eq!(g.node_count(), 1);
    }

    #[test]
    fn removing_edges_preserves_qname_index() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "m::f"));
        let b = g.add_node(Node::new(NodeKind::Function, "m::g"));
        let edge = g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));
        let edge2 = g.add_edge(b, a, Edge::extracted(EdgeKind::Calls));

        g.remove_edges_by_id(&[edge]);
        g.remove_edge(g.edge_index(edge2).unwrap());

        assert_eq!(g.edge_count(), 0);
        assert_eq!(g.find_by_qname("m::f"), Some(a));
        assert_eq!(g.find_by_qname("m::g"), Some(b));
    }
}
