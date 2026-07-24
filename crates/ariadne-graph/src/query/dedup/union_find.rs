//! Union-find (disjoint-set) structure used to consolidate dedup merges.

use crate::core::NodeId;
use std::collections::HashMap;

/// Simple Union-Find (disjoint-set) data structure with union-by-rank and
/// path compression.
pub(super) struct UnionFind {
    parent: HashMap<NodeId, NodeId>,
    rank: HashMap<NodeId, u32>,
}

impl UnionFind {
    pub(super) fn new() -> Self {
        Self {
            parent: HashMap::new(),
            rank: HashMap::new(),
        }
    }

    pub(super) fn make_set(&mut self, node: NodeId) {
        self.parent.insert(node, node);
        self.rank.entry(node).or_insert(0);
    }

    pub(super) fn find(&mut self, node: NodeId) -> NodeId {
        // Path compression: iterative to avoid recursive borrow issues
        let mut current = node;
        let mut path = vec![];
        while let Some(&p) = self.parent.get(&current) {
            if p == current {
                break;
            }
            path.push(current);
            current = p;
        }
        // Compress path
        for &n in &path {
            self.parent.insert(n, current);
        }
        current
    }

    pub(super) fn union(&mut self, a: NodeId, b: NodeId) {
        let root_a = self.find(a);
        let root_b = self.find(b);
        if root_a == root_b {
            return;
        }
        // Union by rank: larger rank becomes parent
        let rank_a = self.rank.get(&root_a).copied().unwrap_or(0);
        let rank_b = self.rank.get(&root_b).copied().unwrap_or(0);
        if rank_a < rank_b {
            self.parent.insert(root_a, root_b);
        } else if rank_a > rank_b {
            self.parent.insert(root_b, root_a);
        } else {
            self.parent.insert(root_b, root_a);
            *self.rank.get_mut(&root_a).unwrap() += 1;
        }
    }

    /// Collect all merges: (loser, winner) pairs where winner is the root.
    pub(super) fn merges(&self) -> Vec<(NodeId, NodeId)> {
        let mut result = Vec::new();
        for (&node, &root) in &self.parent {
            if node != root {
                result.push((node, root));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_union_find() {
        let mut uf = UnionFind::new();
        uf.make_set(NodeId(1));
        uf.make_set(NodeId(2));
        uf.make_set(NodeId(3));
        uf.union(NodeId(1), NodeId(2));
        assert_eq!(uf.find(NodeId(1)), uf.find(NodeId(2)));
        assert_ne!(uf.find(NodeId(1)), uf.find(NodeId(3)));
    }
}
