//! MinHash signatures for estimating Jaccard similarity between shingle sets.

use std::hash::{Hash, Hasher};

/// A simple MinHash implementation using SipHash-derived permutation functions.
///
/// Each MinHash signature is a fixed-length vector of u32 values. Two sets
/// produce the same minimum hash value with probability equal to their
/// Jaccard similarity.
pub(super) struct MinHash {
    pub(super) a: Vec<u64>, // multiplication constant for permutation
    pub(super) b: Vec<u64>, // additive constant for permutation
    pub(super) signature: Vec<u32>,
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
    pub(super) fn new(num_permutations: usize) -> Self {
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
            let h = ((self.a[i]
                .wrapping_mul(h1 % 65521)
                .wrapping_add(self.b[i])
                .wrapping_add(h2))
                % 65521) as u32;
            if h < self.signature[i] {
                self.signature[i] = h;
            }
        }
    }

    /// Build a MinHash signature from an iterable of strings.
    pub(super) fn from_iter(iter: impl IntoIterator<Item = String>, num_permutations: usize) -> Self {
        let mut mh = Self::new(num_permutations);
        for item in iter {
            mh.update(&item);
        }
        mh
    }

    /// Estimate Jaccard similarity between two MinHash signatures.
    pub(super) fn jaccard(&self, other: &MinHash) -> f32 {
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
pub(super) fn shingle(label: &str, size: usize) -> Vec<String> {
    if label.len() < size {
        return vec![label.to_string()];
    }
    label
        .as_bytes()
        .windows(size)
        .map(|w| String::from_utf8_lossy(w).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_shingle() {
        let shingles = shingle("hello", 3);
        assert_eq!(shingles, vec!["hel", "ell", "llo"]);
    }
}
