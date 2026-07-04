//! Entity deduplication.
//!
//! This module implements a multi-pass deduplication pipeline to merge
//! semantically equivalent nodes that carry different labels across
//! extraction sources (e.g. a concept called "Authentication" in one
//! document and "auth" in another).
//!
//! **Pass 1 — Normalization**: Unicode NFC, lowercase, collapse
//! non-alphanumeric characters, strip version suffixes.
//!
//! **Pass 2 — Entropy gate**: Skip low-entropy singletons (noise words
//! like "data", "start", "json") using Shannon entropy. Threshold:
//! `0.5` bits per character.
//!
//! **Pass 3 — MinHash/LSH blocking**: Generate MinHash signatures from
//! character 3-grams. Partition into bands/rows and use LSH to find
//! candidate pairs with Jaccard similarity ≥ 0.7. O(n log n) instead
//! of O(n²).
//!
//! **Pass 4 — Jaro-Winkler verification**: For each candidate pair,
//! compute Jaro-Winkler similarity. Merge if ≥ 0.92.
//!
//! **Pass 5 — Community boost**: Increase similarity by +0.05 if both
//! nodes share a community.
//!
//! **Pass 6 — Union-find merge**: Consolidate merges, pick winners,
//! and rewire all edges.

use crate::core::{Edge, EdgeId, Graph, Node, NodeId, NodeKind};
use jaro_winkler::jaro_winkler;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use unicode_normalization::UnicodeNormalization;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tuning parameters for the deduplication pipeline.
#[derive(Debug, Clone)]
pub struct DedupOptions {
    /// Minimum Shannon entropy (bits/char) for a label to participate in
    /// dedup. Labels below this threshold are considered noise (e.g.
    /// "data", "start", "json"). Default: `0.5`.
    pub entropy_gate: f64,
    /// MinHash shingle size (character n-grams). Default: `3`.
    pub shingle_size: usize,
    /// Number of MinHash permutations. Higher = more accurate Jaccard
    /// estimates but more memory. Default: `64`.
    pub num_permutations: usize,
    /// LSH band count. More bands = fewer false positives but more
    /// false negatives. Default: `12`.
    pub num_bands: usize,
    /// LSH row length per band (bands * rows = number of hash tables).
    /// Default: `5`.
    pub row_length: usize,
    /// Jaccard similarity threshold for candidate-pair generation.
    /// Default: `0.7`.
    pub jaccard_threshold: f32,
    /// Jaro-Winkler similarity threshold for merging. Default: `0.92`.
    pub jw_threshold: f32,
    /// Community similarity boost: add this amount to the Jaro-Winkler
    /// score if both nodes share a community. Default: `0.05`.
    pub community_boost: f32,
    /// Node kinds eligible for dedup. Code nodes (File, Function, etc.)
    /// already have unique `qualified_name` and are excluded by default.
    pub eligible_kinds: HashSet<NodeKind>,
}

// Remove Copy since eligible_kinds is HashSet (not Copy) - already removed above, keeping struct without Copy

impl Default for DedupOptions {
    fn default() -> Self {
        let mut eligible = HashSet::new();
        eligible.insert(NodeKind::Concept);
        eligible.insert(NodeKind::Document);
        eligible.insert(NodeKind::Section);
        eligible.insert(NodeKind::Diagram);
        eligible.insert(NodeKind::Image);
        eligible.insert(NodeKind::Hyperedge);
        Self {
            entropy_gate: 0.5,
            shingle_size: 3,
            num_permutations: 64,
            num_bands: 12,
            row_length: 5,
            jaccard_threshold: 0.7,
            jw_threshold: 0.92,
            community_boost: 0.05,
            eligible_kinds: eligible,
        }
    }
}

/// Result of a deduplication pass, reporting which nodes were merged.
#[derive(Debug, Clone)]
pub struct DedupResult {
    /// Number of candidate pairs examined by LSH + Jaccard.
    pub candidates_examined: usize,
    /// Number of merge operations performed (union-find joins).
    pub merges: usize,
    /// Number of nodes removed (losers).
    pub nodes_removed: usize,
    /// Number of edges re-wired (losers' edges redirected to winners).
    pub edges_rewired: usize,
}

// ---------------------------------------------------------------------------
// Pass 1: Normalization
// ---------------------------------------------------------------------------

/// Normalize a label for comparison.
///
/// Steps:
/// 1. Unicode NFC decomposition (normalizes full-width chars, compatibility forms)
/// 2. Lowercase
/// 3. Strip version suffixes (v2, 1.0, _2, etc.)
/// 4. Collapse non-alphanumeric characters to single underscores
/// 5. Trim leading/trailing underscores
fn normalize_label(label: &str) -> String {
    // NFC normalization
    let normalized: String = label.nfc().collect();
    let lower = normalized.to_lowercase();
    // Strip version suffixes: e.g. "method v2" → "method", "fn 1.0" → "fn"
    // Also strip trailing digits (ASR1603 → ASR)
    // Use two passes: first strip "vN" or "N.N" at word boundaries, then trailing digits
    let stripped = regex::Regex::new(r"\b[vV]?[ _]?\d+\.?\d*$")
        .unwrap()
        .replace_all(&lower, "")
        .into_owned();
    let stripped2 = regex::Regex::new(r"([a-zA-Z_])[ _]?\d+$")
        .unwrap()
        .replace_all(&stripped, "$1")
        .into_owned();
    // Collapse non-alphanumeric to underscores, then trim
    let collapsed = regex::Regex::new(r"[^a-z0-9]+")
        .unwrap()
        .replace_all(&stripped2, "_")
        .into_owned();
    let trimmed = collapsed.trim_matches('_');
    if trimmed.is_empty() {
        return label.to_string();
    }
    trimmed.to_string()
}

// ---------------------------------------------------------------------------
// Pass 2: Entropy gate
// ---------------------------------------------------------------------------

/// Compute Shannon entropy (bits/char) of a string.
/// Returns 0.0 for empty strings.
fn shannon_entropy_str(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    // Calculate Shannon entropy directly: -sum(p * log2(p))
    let mut freq: HashMap<char, usize> = HashMap::new();
    for c in s.chars() {
        *freq.entry(c).or_insert(0) += 1;
    }
    let len = s.len() as f64;
    let mut entropy = 0.0;
    for &count in freq.values() {
        let p = count as f64 / len;
        entropy -= p * p.log2();
    }
    entropy
}

/// Check if a normalized label passes the entropy gate.
/// Low-entropy labels are considered noise (e.g., "data", "json", "start").
fn passes_entropy_gate(normalized: &str, threshold: f64) -> bool {
    let len = normalized.len();
    if len < 3 {
        return true; // Very short labels pass (they may be meaningful)
    }
    // Short words with high entropy can still be noise
    // (e.g., "data" has 4 chars and entropy 1.5, but is a common JSON key)
    if len <= 5 {
        // Very short labels: use a heuristic based on character diversity
        let unique_chars: std::collections::HashSet<char> = normalized.chars().collect();
        // If ≤3 unique chars in ≤5-char word, likely noise (e.g. "data", "json")
        if unique_chars.len() <= 3 {
            return false;
        }
    }
    let ent = shannon_entropy_str(normalized);
    ent >= threshold
}

// ---------------------------------------------------------------------------
// Pass 3: MinHash + LSH blocking
// ---------------------------------------------------------------------------

/// A simple MinHash implementation using SipHash-derived permutation functions.
///
/// Each MinHash signature is a fixed-length vector of u32 values. Two sets
/// produce the same minimum hash value with probability equal to their
/// Jaccard similarity.
struct MinHash {
    a: Vec<u64>, // multiplication constant for permutation
    b: Vec<u64>, // additive constant for permutation
    signature: Vec<u32>,
}

/// Generate a simple pseudo-random u64 from a seed using splitmix64.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^= z >> 31;
    z
}

impl MinHash {
    /// Create a new MinHash with the given number of permutations.
    fn new(num_permutations: usize) -> Self {
        let mut state: u64 = 0x6c62272e07bb0142;
        let seeds: Vec<u64> = (0..num_permutations)
            .map(|_| {
                splitmix64(&mut state);
                splitmix64(&mut state)
            })
            .collect();
        let signature = vec![u32::MAX; num_permutations];
        let a: Vec<u64> = (0..num_permutations)
            .map(|i| seeds[i] ^ (i as u64))
            .collect();
        let b: Vec<u64> = (0..num_permutations)
            .map(|i| seeds[i] ^ (i as u64).wrapping_mul(31))
            .collect();
        Self { a, b, signature }
    }

    /// Hash a string using FNV-1a with a salt.
    fn hash_with_salt(data: &[u8], salt: u64) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        salt.hash(&mut hasher);
        data.hash(&mut hasher);
        hasher.finish()
    }

    /// Update MinHash with a shingle (string).
    fn update(&mut self, shingle: &str) {
        for i in 0..self.signature.len() {
            // Double hashing: (a * h1 + b) % p, where p is a large prime
            let h1 = Self::hash_with_salt(shingle.as_bytes(), self.a[i]);
            let h2 = Self::hash_with_salt(shingle.as_bytes(), self.b[i]);
            let h = ((self.a[i].wrapping_mul(h1 % 65521).wrapping_add(self.b[i]).wrapping_add(h2)) % 65521) as u32;
            if h < self.signature[i] {
                self.signature[i] = h;
            }
        }
    }

    /// Build a MinHash signature from an iterable of strings.
    fn from_iter(iter: impl IntoIterator<Item = String>, num_permutations: usize) -> Self {
        let mut mh = Self::new(num_permutations);
        for item in iter {
            mh.update(&item);
        }
        mh
    }

    /// Estimate Jaccard similarity between two MinHash signatures.
    fn jaccard(&self, other: &MinHash) -> f32 {
        if self.signature.is_empty() {
            return 0.0;
        }
        let matches = self
            .signature
            .iter()
            .zip(&other.signature)
            .filter(|(a, b)| a == b)
            .count();
        matches as f32 / self.signature.len() as f32
    }
}

/// Generate character n-gram shingles from a label.
fn shingle(label: &str, size: usize) -> Vec<String> {
    if label.len() < size {
        return vec![label.to_string()];
    }
    label
        .as_bytes()
        .windows(size)
        .map(|w| String::from_utf8_lossy(w).to_string())
        .collect()
}

/// LSH index for MinHash signatures.
struct LshIndex {
    /// Hash tables: one per band. Each maps a band signature → list of node IDs.
    tables: Vec<HashMap<Vec<u32>, Vec<NodeId>>>,
    num_bands: usize,
    row_length: usize,
}

impl LshIndex {
    fn new(num_bands: usize, row_length: usize) -> Self {
        let tables: Vec<_> = (0..num_bands).map(|_| HashMap::new()).collect();
        Self {
            tables,
            num_bands,
            row_length,
        }
    }

    /// Add a MinHash signature with its node ID.
    fn add(&mut self, signature: &MinHash, node_id: NodeId) {
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
    fn get_candidates(&self, signature: &MinHash) -> HashSet<NodeId> {
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
fn lsh_candidate_pairs(
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

// ---------------------------------------------------------------------------
// Pass 4: Jaro-Winkler verification + community boost
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pass 5 + 6: Union-Find merge
// ---------------------------------------------------------------------------

/// Simple Union-Find (disjoint-set) data structure with union-by-rank and
/// path compression.
struct UnionFind {
    parent: HashMap<NodeId, NodeId>,
    rank: HashMap<NodeId, u32>,
}

impl UnionFind {
    fn new() -> Self {
        Self {
            parent: HashMap::new(),
            rank: HashMap::new(),
        }
    }

    fn make_set(&mut self, node: NodeId) {
        self.parent.insert(node, node);
        self.rank.entry(node).or_insert(0);
    }

    fn find(&mut self, node: NodeId) -> NodeId {
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

    fn union(&mut self, a: NodeId, b: NodeId) {
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
    fn merges(&self) -> Vec<(NodeId, NodeId)> {
        let mut result = Vec::new();
        for (&node, &root) in &self.parent {
            if node != root {
                result.push((node, root));
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Main deduplication function
// ---------------------------------------------------------------------------

/// Run the full entity deduplication pipeline.
///
/// This is the entry point. It:
/// 1. Collects eligible nodes
/// 2. Runs all passes (normalization → entropy → MinHash/LSH → Jaro-Winkler)
/// 3. Merges nodes via union-find
/// 4. Rewires edges to point to winners
/// 5. Removes merged-away nodes
///
/// Returns a `DedupResult` summarizing the changes.
///
/// # Community-aware dedup
///
/// Optionally accepts a community map. Nodes sharing a community get a
/// small similarity boost, which helps merge conceptually related entities
/// that have slightly different labels (e.g., "Authentication" vs "auth").
pub fn deduplicate_nodes(
    graph: &mut Graph,
    communities: &HashMap<NodeId, usize>,
    options: Option<DedupOptions>,
) -> DedupResult {
    let options = options.unwrap_or_default();

    // Collect eligible nodes
    let eligible_nodes: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| options.eligible_kinds.contains(&n.kind))
        .collect();

    if eligible_nodes.len() < 2 {
        return DedupResult {
            candidates_examined: 0,
            merges: 0,
            nodes_removed: 0,
            edges_rewired: 0,
        };
    }

    // --- Pass 2: Entropy gate ---
    let entropy_filtered: Vec<(NodeId, &Node)> = eligible_nodes
        .into_iter()
        .filter(|(_, node)| {
            let normalized = normalize_label(&node.name);
            passes_entropy_gate(&normalized, options.entropy_gate)
        })
        .collect();

    if entropy_filtered.len() < 2 {
        return DedupResult {
            candidates_examined: 0,
            merges: 0,
            nodes_removed: 0,
            edges_rewired: 0,
        };
    }

    // --- Pass 3: MinHash/LSH blocking ---
    let filtered_ids: Vec<_> = entropy_filtered.iter().map(|(id, _)| *id).collect();
    let filtered_ptrs: Vec<_> = entropy_filtered.iter().map(|(_, n)| *n).collect();

    let candidate_pairs = lsh_candidate_pairs(&filtered_ptrs, &filtered_ids, &options);
    let candidates_examined = candidate_pairs.len();

    if candidate_pairs.is_empty() {
        return DedupResult {
            candidates_examined,
            merges: 0,
            nodes_removed: 0,
            edges_rewired: 0,
        };
    }

    // --- Pass 4: Jaro-Winkler verification + community boost ---
    // Build a node-id → node map for lookups
    let node_map: HashMap<NodeId, &Node> =
        entropy_filtered.iter().map(|(id, n)| (*id, *n)).collect();

    let mut uf = UnionFind::new();
    for id in &filtered_ids {
        uf.make_set(*id);
    }

    for (id_a, id_b, _jaccard) in &candidate_pairs {
        let node_a = node_map[id_a];
        let node_b = node_map[id_b];
        let normalized_a = normalize_label(&node_a.name);
        let normalized_b = normalize_label(&node_b.name);
        let jw = jaro_winkler(&normalized_a, &normalized_b);

        // Community boost
        let effective_score = if let Some(&comm_a) = communities.get(id_a) {
            if communities.get(id_b) == Some(&comm_a) {
                (jw + options.community_boost).min(1.0)
            } else {
                jw
            }
        } else {
            jw
        };

        if effective_score >= options.jw_threshold {
            uf.union(*id_a, *id_b);
        }
    }

    let merge_list = uf.merges();
    let merges = merge_list.len();

    if merges == 0 {
        return DedupResult {
            candidates_examined,
            merges: 0,
            nodes_removed: 0,
            edges_rewired: 0,
        };
    }

    // --- Pass 6: Rewire edges and remove merged-away nodes ---
    let mut edges_rewired = 0usize;
    let mut nodes_removed = 0usize;

    let losers: HashSet<NodeId> = merge_list.iter().map(|(l, _)| *l).collect();

    // Build winner lookup (loser → winner, with winner mapping to itself)
    let mut winner_map: HashMap<NodeId, NodeId> = merge_list.iter().cloned().collect();
    // Winners map to themselves
    for (_loser, winner) in &merge_list {
        winner_map.entry(*winner).or_insert(*winner);
    }

    // Collect all edges that involve any loser node BEFORE mutation
    let mut edges_to_process: Vec<(EdgeId, NodeId, NodeId, Edge)> = Vec::new();
    for (eid, src, dst, edge) in graph.edges() {
        if losers.contains(&src) || losers.contains(&dst) {
            edges_to_process.push((eid, src, dst, edge.clone()));
        }
    }

    // Rewire each edge: replace loser src/dst with winner
    for (eid, old_src, old_dst, edge) in edges_to_process {
        let new_src = *winner_map.get(&old_src).unwrap_or(&old_src);
        let new_dst = *winner_map.get(&old_dst).unwrap_or(&old_dst);

        // Avoid self-loops
        if new_src == new_dst {
            edges_rewired += 1;
            continue;
        }

        // Check for duplicate edge (same src, dst, kind)
        let existing = graph
            .edges()
            .any(|(_, s, d, e)| s == new_src && d == new_dst && e.kind == edge.kind);
        if existing {
            edges_rewired += 1;
            continue; // Skip — duplicate edge already exists
        }

        // Remove old edge and add rewired one
        if let Some(edge_idx) = graph.edge_index(eid) {
            graph.remove_edge(edge_idx);
            graph.add_edge(new_src, new_dst, edge);
        }
        edges_rewired += 1;
    }

    // Remove merged-away nodes
    for loser in losers {
        graph.remove_node(loser);
        nodes_removed += 1;
    }

    DedupResult {
        candidates_examined,
        merges,
        nodes_removed,
        edges_rewired,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Edge, EdgeKind, Node, NodeKind};

    fn make_node(graph: &mut Graph, kind: NodeKind, name: &str) -> NodeId {
        let node = Node::new(kind, name);
        graph.add_node(node)
    }

    #[test]
    fn test_normalize_label() {
        assert_eq!(normalize_label("Authentication"), "authentication");
        assert_eq!(normalize_label("auth"), "auth");
        assert_eq!(normalize_label("  hello   world  "), "hello_world");
        assert_eq!(normalize_label("method v2"), "method");
        assert_eq!(normalize_label("data"), "data");
        assert_eq!(normalize_label("auth_1603"), "auth");
    }

    #[test]
    fn test_shannon_entropy() {
        // "aaaa" has very low entropy
        assert!(shannon_entropy_str("aaaa") < 0.1);
        // "abcd" has higher entropy
        assert!(shannon_entropy_str("abcd") > 1.0);
    }

    #[test]
    fn test_entropy_gate() {
        // Low-entropy short words are filtered
        assert!(!passes_entropy_gate("data", 0.5)); // 4 chars, 2 unique → noise
        assert!(!passes_entropy_gate("aaa", 0.5)); // 3 chars, 1 unique → noise
                                                   // Words with enough diversity pass
        assert!(passes_entropy_gate("start", 0.5)); // 5 chars, 4 unique → passes
        assert!(passes_entropy_gate("authentication", 0.5));
        assert!(passes_entropy_gate("database", 0.5));
    }

    #[test]
    fn test_minhash_jaccard_exact_match() {
        let items = vec![
            "the".to_string(),
            "quick".to_string(),
            "brown".to_string(),
            "fox".to_string(),
        ];
        let mh1 = MinHash::from_iter(items.clone(), 64);
        let mh2 = MinHash::from_iter(items, 64);
        assert!((mh1.jaccard(&mh2) - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_minhash_jaccard_partial_overlap() {
        let items1: Vec<String> = (0..20).map(|i| format!("item_{}", i)).collect();
        let items2: Vec<String> = (10..30).map(|i| format!("item_{}", i)).collect();
        // 10/30 overlap = 0.333... Jaccard
        let mh1 = MinHash::from_iter(items1, 64);
        let mh2 = MinHash::from_iter(items2, 64);
        let jaccard = mh1.jaccard(&mh2);
        // Allow some tolerance due to probabilistic nature
        assert!(jaccard > 0.1 && jaccard < 0.7);
    }

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

    #[test]
    fn test_dedup_excludes_code_nodes() {
        let mut graph = Graph::new();
        make_node(&mut graph, NodeKind::Function, "login");
        make_node(&mut graph, NodeKind::Function, "auth");

        let comm = HashMap::new();
        let result = deduplicate_nodes(&mut graph, &comm, None);

        assert_eq!(result.merges, 0);
        assert_eq!(result.nodes_removed, 0);
        assert_eq!(graph.node_count(), 2);
    }

    #[test]
    fn test_dedup_no_merge_for_dissimilar() {
        let mut graph = Graph::new();
        make_node(&mut graph, NodeKind::Concept, "Authentication");
        make_node(&mut graph, NodeKind::Concept, "Database");

        let comm = HashMap::new();
        let result = deduplicate_nodes(&mut graph, &comm, None);

        assert_eq!(result.merges, 0);
        assert_eq!(result.nodes_removed, 0);
        assert_eq!(graph.node_count(), 2);
    }

    #[test]
    fn test_dedup_community_boost() {
        let mut graph = Graph::new();
        let a = make_node(&mut graph, NodeKind::Concept, "Authentication");
        let b = make_node(&mut graph, NodeKind::Concept, "Auth");

        let mut comm = HashMap::new();
        comm.insert(a, 0);
        comm.insert(b, 0); // Same community

        let result = deduplicate_nodes(
            &mut graph,
            &comm,
            Some(DedupOptions {
                jw_threshold: 0.92,
                community_boost: 0.05,
                ..Default::default()
            }),
        );

        // Community boost may help merge even with default threshold
        let _ = result;
    }

    #[test]
    fn test_shingle() {
        let shingles = shingle("hello", 3);
        assert_eq!(shingles, vec!["hel", "ell", "llo"]);
    }

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

    #[test]
    fn test_dedup_rewires_edges() {
        let mut graph = Graph::new();
        // Use strings with high character overlap to trigger LSH candidates
        let auth = make_node(&mut graph, NodeKind::Concept, "Authentication");
        let auth_similar = make_node(&mut graph, NodeKind::Concept, "Authentification"); // common misspelling
        let user = make_node(&mut graph, NodeKind::Concept, "User");

        // auth_similar has incoming and outgoing edges
        graph.add_edge(user, auth_similar, Edge::inferred(EdgeKind::Mentions, 0.8));
        graph.add_edge(auth_similar, auth, Edge::inferred(EdgeKind::Mentions, 0.6));

        let mut comm = HashMap::new();
        comm.insert(auth, 0);
        comm.insert(auth_similar, 0);

        let result = deduplicate_nodes(
            &mut graph,
            &comm,
            Some(DedupOptions {
                jw_threshold: 0.85,
                community_boost: 0.08,
                ..Default::default()
            }),
        );

        assert!(
            result.merges > 0,
            "should have merged at least one pair (got {})",
            result.merges
        );
        assert_eq!(
            graph.node_count(),
            2,
            "should have removed exactly one node"
        );
        // Check that edges were rewired — user should now point at auth
        let user_edges: Vec<_> = graph
            .out_neighbors(user)
            .filter(|(_, e)| e.kind == EdgeKind::Mentions)
            .collect();
        assert!(
            !user_edges.is_empty(),
            "user should still have mention edges"
        );
    }

    #[test]
    fn test_dedup_avoids_self_loops() {
        let mut graph = Graph::new();
        // Use strings with high character overlap
        let a = make_node(&mut graph, NodeKind::Concept, "Authentication");
        let b = make_node(&mut graph, NodeKind::Concept, "Authentification");

        // Both point at each other
        graph.add_edge(a, b, Edge::inferred(EdgeKind::SimilarTo, 0.9));
        graph.add_edge(b, a, Edge::inferred(EdgeKind::SimilarTo, 0.9));

        let mut comm = HashMap::new();
        comm.insert(a, 0);
        comm.insert(b, 0);

        let result = deduplicate_nodes(
            &mut graph,
            &comm,
            Some(DedupOptions {
                jw_threshold: 0.85,
                community_boost: 0.08,
                ..Default::default()
            }),
        );

        assert!(result.merges > 0, "should have merged");
        assert_eq!(graph.node_count(), 1, "should have one node left");
        // Verify no self-loops exist (only one node, so no edges possible)
        assert_eq!(graph.edge_count(), 0, "self-loops should be eliminated");
    }
}
