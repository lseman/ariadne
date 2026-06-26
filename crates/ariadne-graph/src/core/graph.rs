use crate::{Edge, EdgeId, EdgeKind, Node, NodeId};
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

    /// Merge all nodes and edges from `other` into `self`.
    ///
    /// Nodes with matching qualified names are deduplicated (same behaviour
    /// as `add_node`). Edges are added with their original source/target
    /// semantics — duplicate edges (same src, dst, kind) are skipped.
    pub fn merge(&mut self, other: Graph) {
        let own_qn: HashMap<String, NodeIndex> = self
            .inner
            .node_indices()
            .map(|i| (self.inner[i].qualified_name.clone(), i))
            .collect();

        // Build a mapping from each node's original index in `other`
        // to its new index in the merged graph.
        let mut remap: HashMap<usize, NodeIndex> = HashMap::new();
        for (idx, (_, node)) in other.inner.node_indices().zip(other.nodes()) {
            let qn = node.qualified_name.clone();
            let mapped = match own_qn.get(&qn) {
                Some(idx) => *idx,
                None => {
                    let idx = self.inner.add_node(node.clone());
                    self.by_qname.insert(qn, idx);
                    idx
                }
            };
            remap.insert(idx.index(), mapped);
        }

        for (_, src, dst, edge) in other.edges() {
            let Some(si) = remap.get(&(src.0 as usize)).copied() else {
                continue;
            };
            let Some(di) = remap.get(&(dst.0 as usize)).copied() else {
                continue;
            };
            if si == di {
                continue;
            }
            if self.has_edge_kind(si, di, edge) {
                continue;
            }
            self.inner.add_edge(si, di, edge.clone());
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Trait abstractions for decoupling graph callers
// ─────────────────────────────────────────────────────────────────────────────

/// Read-only graph operations.
pub trait GraphRead {
    fn find_by_qname(&self, qname: &str) -> Option<NodeId>;
    fn nodes(&self) -> Box<dyn Iterator<Item = (NodeId, &Node)> + '_>;
    fn edges(&self) -> Box<dyn Iterator<Item = (EdgeId, NodeId, NodeId, &Edge)> + '_>;
    fn out_neighbors(&self, id: NodeId) -> Box<dyn Iterator<Item = (NodeId, &Edge)> + '_>;
    fn in_neighbors(&self, id: NodeId) -> Box<dyn Iterator<Item = (NodeId, &Edge)> + '_>;
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
    fn has_edge_kind(&self, src: NodeId, dst: NodeId, kind: &EdgeKind) -> bool;
}

/// Mutable graph operations (includes read methods too).
pub trait GraphMut {
    // Mutation
    fn add_node(&mut self, node: Node) -> NodeId;
    fn add_edge(&mut self, src: NodeId, dst: NodeId, edge: Edge) -> EdgeId;
    fn rename_node(&mut self, id: NodeId, new_qn: &str, new_name: &str) -> NodeId;
    fn remove_node(&mut self, id: NodeId);
    fn remove_nodes_by_id(&mut self, ids: &[NodeId]);
    fn remove_edges_by_id(&mut self, ids: &[EdgeId]);
    fn merge(&mut self, other: Graph);
    fn edge_mut(&mut self, id: EdgeId) -> Option<&mut Edge>;
    // Read (also available through GraphRead for &Graph)
    fn node(&self, id: NodeId) -> Option<&Node>;
    fn node_mut(&mut self, id: NodeId) -> Option<&mut Node>;
    fn nodes(&self) -> Box<dyn Iterator<Item = (NodeId, &Node)> + '_>;
    fn edges(&self) -> Box<dyn Iterator<Item = (EdgeId, NodeId, NodeId, &Edge)> + '_>;
    fn out_neighbors(&self, id: NodeId) -> Box<dyn Iterator<Item = (NodeId, &Edge)> + '_>;
    fn in_neighbors(&self, id: NodeId) -> Box<dyn Iterator<Item = (NodeId, &Edge)> + '_>;
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
    fn find_by_qname(&self, qname: &str) -> Option<NodeId>;
}

impl GraphRead for Graph {
    fn find_by_qname(&self, qname: &str) -> Option<NodeId> {
        self.by_qname.get(qname).map(|i| NodeId(i.index() as u32))
    }

    fn nodes(&self) -> Box<dyn Iterator<Item = (NodeId, &Node)> + '_> {
        Box::new(
            self.inner
                .node_indices()
                .map(|i| (NodeId(i.index() as u32), &self.inner[i])),
        )
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (EdgeId, NodeId, NodeId, &Edge)> + '_> {
        Box::new(self.inner.edge_references().map(|e| {
            (
                EdgeId(e.id().index() as u32),
                NodeId(e.source().index() as u32),
                NodeId(e.target().index() as u32),
                e.weight(),
            )
        }))
    }

    fn out_neighbors(&self, id: NodeId) -> Box<dyn Iterator<Item = (NodeId, &Edge)> + '_> {
        let idx = NodeIndex::new(id.0 as usize);
        Box::new(
            self.inner
                .edges_directed(idx, Direction::Outgoing)
                .map(|e| (NodeId(e.target().index() as u32), e.weight())),
        )
    }

    fn in_neighbors(&self, id: NodeId) -> Box<dyn Iterator<Item = (NodeId, &Edge)> + '_> {
        let idx = NodeIndex::new(id.0 as usize);
        Box::new(
            self.inner
                .edges_directed(idx, Direction::Incoming)
                .map(|e| (NodeId(e.source().index() as u32), e.weight())),
        )
    }

    fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    fn edge_count(&self) -> usize {
        self.inner.edge_count()
    }

    fn has_edge_kind(&self, src: NodeId, dst: NodeId, kind: &EdgeKind) -> bool {
        let s = NodeIndex::new(src.0 as usize);
        let d = NodeIndex::new(dst.0 as usize);
        self.inner
            .edges_directed(s, Direction::Outgoing)
            .any(|e| e.target() == d && e.weight().kind == *kind)
    }
}

impl GraphMut for Graph {
    fn add_node(&mut self, node: Node) -> NodeId {
        self.add_node(node)
    }

    fn add_edge(&mut self, src: NodeId, dst: NodeId, edge: Edge) -> EdgeId {
        self.add_edge(src, dst, edge)
    }

    fn rename_node(&mut self, id: NodeId, new_qn: &str, new_name: &str) -> NodeId {
        self.rename_node(id, new_qn, new_name)
    }

    fn remove_node(&mut self, id: NodeId) {
        self.remove_node(id);
    }

    fn remove_nodes_by_id(&mut self, ids: &[NodeId]) {
        self.remove_nodes_by_id(ids);
    }

    fn remove_edges_by_id(&mut self, ids: &[EdgeId]) {
        self.remove_edges_by_id(ids);
    }

    fn merge(&mut self, other: Graph) {
        self.merge(other);
    }

    fn edge_mut(&mut self, id: EdgeId) -> Option<&mut Edge> {
        self.edge_mut(id)
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.node(id)
    }

    fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.node_mut(id)
    }

    fn nodes(&self) -> Box<dyn Iterator<Item = (NodeId, &Node)> + '_> {
        GraphRead::nodes(self)
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (EdgeId, NodeId, NodeId, &Edge)> + '_> {
        GraphRead::edges(self)
    }

    fn out_neighbors(&self, id: NodeId) -> Box<dyn Iterator<Item = (NodeId, &Edge)> + '_> {
        GraphRead::out_neighbors(self, id)
    }

    fn in_neighbors(&self, id: NodeId) -> Box<dyn Iterator<Item = (NodeId, &Edge)> + '_> {
        GraphRead::in_neighbors(self, id)
    }

    fn node_count(&self) -> usize {
        GraphRead::node_count(self)
    }

    fn edge_count(&self) -> usize {
        GraphRead::edge_count(self)
    }

    fn find_by_qname(&self, qname: &str) -> Option<NodeId> {
        GraphRead::find_by_qname(self, qname)
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

    #[test]
    fn merge_combines_nodes_and_edges() {
        let mut g1 = Graph::new();
        let a = g1.add_node(Node::new(NodeKind::Function, "m::f"));
        let b = g1.add_node(Node::new(NodeKind::Function, "m::g"));
        g1.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let mut g2 = Graph::new();
        let c = g2.add_node(Node::new(NodeKind::Function, "n::h"));
        let d = g2.add_node(Node::new(NodeKind::Function, "n::i"));
        g2.add_edge(c, d, Edge::extracted(EdgeKind::Calls));

        g1.merge(g2);
        assert_eq!(g1.node_count(), 4);
        assert_eq!(g1.edge_count(), 2);
        assert!(g1.find_by_qname("m::f").is_some());
        assert!(g1.find_by_qname("n::h").is_some());
    }

    #[test]
    fn merge_deduplicates_nodes_by_qname() {
        let mut g1 = Graph::new();
        let a = g1.add_node(Node::new(NodeKind::Function, "m::f"));
        g1.add_node(Node::new(NodeKind::File, "file::a.rs"));

        let mut g2 = Graph::new();
        let _ = g2.add_node(Node::new(NodeKind::Function, "m::f")); // same qname
        g2.add_node(Node::new(NodeKind::File, "file::b.rs"));

        g1.merge(g2);
        assert_eq!(g1.node_count(), 3, "duplicate m::f should be deduped");
        assert_eq!(g1.find_by_qname("m::f"), Some(a));
    }

    #[test]
    fn merge_deduplicates_edges() {
        let mut g1 = Graph::new();
        let a = g1.add_node(Node::new(NodeKind::Function, "a"));
        let b = g1.add_node(Node::new(NodeKind::Function, "b"));
        g1.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let mut g2 = Graph::new();
        let a2 = g2.add_node(Node::new(NodeKind::Function, "a"));
        let b2 = g2.add_node(Node::new(NodeKind::Function, "b"));
        g2.add_edge(a2, b2, Edge::extracted(EdgeKind::Calls));

        g1.merge(g2);
        assert_eq!(g1.edge_count(), 1, "duplicate edge should be skipped");
    }
}
