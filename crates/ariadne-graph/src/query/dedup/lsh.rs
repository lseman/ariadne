//! LSH (locality-sensitive hashing) blocking over MinHash signatures.

use super::minhash::{shingle, MinHash};
use super::DedupOptions;
use crate::core::{Node, NodeId};
use std::collections::{HashMap, HashSet};

/// LSH index for MinHash signatures.
pub(super) struct LshIndex {
    /// Hash tables: one per band. Each maps a band signature → list of node IDs.
    tables: Vec<HashMap<Vec<u32>, Vec<NodeId>>>,
    num_bands: usize,
    row_length: usize,
}

impl LshIndex {
    pub(super) fn new(num_bands: usize, row_length: usize) -> Self {
        let tables: Vec<_> = (0..num_bands).map(|_| HashMap::new()).collect();
        Self {
            tables,
            num_bands,
            row_length,
        }
    }

    /// Add a MinHash signature with its node ID.
    pub(super) fn add(&mut self, signature: &MinHash, node_id: NodeId) {
        let sig = &signature.signature;
        let row_len = self.row_length;

        for band in 0..self.num_bands {
            let start = band * row_len;
            let end = start + row_len;
            if end > sig.len() {
                continue;
            }
            let band_sig: Vec<u32> = sig[start..end].to_vec();
            self.tables[band].entry(band_sig).or_default().push(node_id);
        }
    }

    /// Find candidate pairs: all node IDs that share at least one band.
    pub(super) fn get_candidates(&self, signature: &MinHash) -> HashSet<NodeId> {
        let sig = &signature.signature;
        let row_len = self.row_length;
        let mut candidates = HashSet::new();

        for band in 0..self.num_bands {
            let start = band * row_len;
            let end = start + row_len;
            if end > sig.len() {
                continue;
            }
            let band_sig: Vec<u32> = sig[start..end].to_vec();
            if let Some(ids) = self.tables[band].get(&band_sig) {
                for id in ids {
                    candidates.insert(*id);
                }
            }
        }
        candidates
    }
}

/// Run MinHash/LSH to find candidate pairs.
///
/// Returns a list of (node_id_a, node_id_b, jaccard_estimate) for pairs
/// that share at least one LSH band. The caller should filter further
/// using Jaro-Winkler.
pub(super) fn lsh_candidate_pairs(
    nodes: &[&Node],
    node_indices: &[NodeId],
    options: &DedupOptions,
) -> Vec<(NodeId, NodeId, f32)> {
    let mut lsh = LshIndex::new(options.num_bands, options.row_length);

    // Build MinHash signatures for all nodes
    let mut signatures: HashMap<NodeId, MinHash> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        let shingles = shingle(&node.name, options.shingle_size);
        let sig = MinHash::from_iter(shingles, options.num_permutations);
        signatures.insert(node_indices[i], sig);
    }

    // Add all signatures to the LSH index
    for (id, sig) in &signatures {
        lsh.add(sig, *id);
    }

    // For each node, find candidates via LSH and compute Jaccard
    let mut pairs: Vec<(NodeId, NodeId, f32)> = Vec::new();
    let mut pair_seen = HashSet::new();

    for (&id_a, sig_a) in &signatures {
        let candidates = lsh.get_candidates(sig_a);
        for id_b in candidates {
            if id_b == id_a {
                continue;
            }
            let pair = if id_a < id_b {
                (id_a, id_b)
            } else {
                (id_b, id_a)
            };
            if pair_seen.contains(&pair) {
                continue;
            }
            pair_seen.insert(pair);
            if let Some(sig_b) = signatures.get(&id_b) {
                let jaccard = sig_a.jaccard(sig_b);
                if jaccard >= options.jaccard_threshold {
                    pairs.push((id_a, id_b, jaccard));
                }
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lsh_index() {
        let mut lsh = LshIndex::new(12, 5);
        let sig = MinHash::from_iter(vec!["a".into(), "b".into(), "c".into()], 64);
        lsh.add(&sig, NodeId(1));
        lsh.add(&sig, NodeId(2)); // Same signature

        let candidates = lsh.get_candidates(&sig);
        assert!(candidates.contains(&NodeId(1)));
        assert!(candidates.contains(&NodeId(2)));
    }
}
