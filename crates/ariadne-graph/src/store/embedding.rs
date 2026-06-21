//! Local feature-hash embedding model (ariadne-hash-v2).
//!
//! Deterministic, no external dependencies. Complement to FTS5.

use super::db::DEFAULT_EMBEDDING_DIM;

/// Build the text from which an embedding is computed for a node.
pub fn embedding_source_text(
    kind: &str,
    name: &str,
    qualified_name: &str,
    source_uri: Option<&str>,
) -> String {
    let mut text = format!("{} {} {}", kind, name, qualified_name.replace("::", " "));
    if let Some(source_uri) = source_uri {
        text.push(' ');
        text.push_str(source_uri);
    }
    text
}

/// Build a local feature-hash embedding for a text string.
pub fn semantic_embedding(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0; DEFAULT_EMBEDDING_DIM];
    let tokens = semantic_tokens(text);
    if tokens.is_empty() {
        return vector;
    }

    let unique_tokens = unique_ordered(&tokens);
    for token in &tokens {
        push_signed_hashed_feature(&mut vector, &format!("tok:{token}"), 1.25);
        push_signed_hashed_feature(&mut vector, &format!("stem:{}", code_stem(token)), 0.70);
        let canonical = canonical_token(token);
        if canonical != *token {
            push_signed_hashed_feature(&mut vector, &format!("canon:{canonical}"), 1.05);
        }
        for gram in char_ngrams(token, 3, 5) {
            push_signed_hashed_feature(&mut vector, &format!("char:{gram}"), 0.24);
        }
        for piece in token_pieces(token) {
            push_signed_hashed_feature(&mut vector, &format!("piece:{piece}"), 0.42);
        }
    }

    for pair in tokens.windows(2) {
        push_signed_hashed_feature(&mut vector, &format!("bi:{}:{}", pair[0], pair[1]), 0.82);
        let left = canonical_token(&pair[0]);
        let right = canonical_token(&pair[1]);
        if left != pair[0] || right != pair[1] {
            push_signed_hashed_feature(&mut vector, &format!("cbi:{left}:{right}"), 0.58);
        }
    }
    for triple in tokens.windows(3) {
        push_signed_hashed_feature(
            &mut vector,
            &format!("tri:{}:{}:{}", triple[0], triple[1], triple[2]),
            0.36,
        );
        push_signed_hashed_feature(
            &mut vector,
            &format!("skip:{}:{}", triple[0], triple[2]),
            0.28,
        );
    }

    let acronym: String = unique_tokens
        .iter()
        .filter_map(|token| token.chars().next())
        .collect();
    if acronym.len() >= 2 {
        push_signed_hashed_feature(&mut vector, &format!("acro:{acronym}"), 0.85);
    }

    for concept in semantic_concepts(&unique_tokens) {
        push_signed_hashed_feature(&mut vector, &format!("concept:{concept}"), 3.0);
    }

    normalize_vector(&mut vector);
    vector
}

fn semantic_tokens(raw: &str) -> Vec<String> {
    let mut normalized = String::new();
    let mut prev: Option<char> = None;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        if c.is_ascii_alphanumeric() {
            if let Some(p) = prev {
                let camel_boundary = p.is_ascii_lowercase() && c.is_ascii_uppercase();
                let acronym_boundary = p.is_ascii_uppercase()
                    && c.is_ascii_uppercase()
                    && next.is_some_and(|n| n.is_ascii_lowercase());
                let digit_boundary = p.is_ascii_alphabetic() != c.is_ascii_alphabetic();
                if camel_boundary || acronym_boundary || digit_boundary {
                    normalized.push(' ');
                }
            }
            normalized.push(c.to_ascii_lowercase());
            prev = Some(c);
        } else {
            normalized.push(' ');
            prev = None;
        }
    }

    normalized
        .split_whitespace()
        .map(singularize_token)
        .filter(|token| !token.is_empty())
        .collect()
}

fn unique_ordered(tokens: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for token in tokens {
        if seen.insert(token.as_str()) {
            out.push(token.clone());
        }
    }
    out
}

fn singularize_token(token: &str) -> String {
    if token.len() > 4 && token.ends_with('s') {
        token[..token.len() - 1].to_string()
    } else {
        token.to_string()
    }
}

fn canonical_token(token: &str) -> String {
    match token {
        "delete" | "deleted" | "remove" | "removed" | "drop" | "purge" | "cleanup" => {
            "remove".to_string()
        }
        "add" | "added" | "create" | "created" | "insert" | "new" => "add".to_string(),
        "change" | "changed" | "changes" | "diff" | "delta" | "modify" | "modified" | "update"
        | "updated" => "change".to_string(),
        "find" | "search" | "lookup" | "query" | "discover" => "search".to_string(),
        "auth" | "authenticate" | "authentication" | "login" | "signin" | "signon" => {
            "auth".to_string()
        }
        "test" | "tests" | "spec" | "specs" | "coverage" => "test".to_string(),
        "bug" | "defect" | "error" | "failure" | "panic" | "regression" => "bug".to_string(),
        "cache" | "cached" | "memo" | "memoize" | "memoized" => "cache".to_string(),
        "config" | "configuration" | "setting" | "settings" => "config".to_string(),
        "db" | "database" | "sqlite" | "store" | "storage" | "persist" | "persistence" => {
            "storage".to_string()
        }
        "doc" | "docs" | "document" | "documentation" | "readme" => "doc".to_string(),
        "embed" | "embedding" | "embeddings" | "semantic" | "vector" | "vectors" => {
            "embedding".to_string()
        }
        "file" | "files" | "path" | "paths" | "source" | "sources" => "source".to_string(),
        "graph" | "node" | "nodes" | "edge" | "edges" | "flow" | "flows" => "graph".to_string(),
        "http" | "server" | "serve" | "route" | "routes" => "server".to_string(),
        "ignore" | "gitignore" | "ariadneignore" | "exclude" | "skip" => "ignore".to_string(),
        "index" | "indexed" | "indexing" | "fts" | "fts5" => "index".to_string(),
        "install" | "installer" | "setup" | "hook" | "hooks" => "install".to_string(),
        "json" | "mcp" | "tool" | "tools" | "agent" | "agents" => "agent".to_string(),
        "rank" | "ranking" | "score" | "scored" | "scoring" | "boost" | "boosted" => {
            "rank".to_string()
        }
        "read" | "reader" | "parse" | "parser" | "extract" | "extraction" => "extract".to_string(),
        "review" | "risk" | "impact" | "blast" | "radius" => "review".to_string(),
        "symbol" | "symbols" | "function" | "functions" | "method" | "methods" => {
            "symbol".to_string()
        }
        "terminal" | "tui" | "ui" | "viewer" | "view" => "ui".to_string(),
        "watch" | "daemon" | "poll" | "polling" => "watch".to_string(),
        other => other.to_string(),
    }
}

fn code_stem(token: &str) -> String {
    let mut stem = singularize_token(token);
    for suffix in ["ing", "ed", "er", "or", "able", "ible", "tion", "ions"] {
        if stem.len() > suffix.len() + 3 && stem.ends_with(suffix) {
            stem.truncate(stem.len() - suffix.len());
            break;
        }
    }
    stem
}

fn token_pieces(token: &str) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 3 {
        return Vec::new();
    }
    let mut pieces = Vec::new();
    for len in [3usize, 4, 5] {
        if chars.len() >= len {
            pieces.push(chars[..len].iter().collect());
            pieces.push(chars[chars.len() - len..].iter().collect());
        }
    }
    pieces.sort();
    pieces.dedup();
    pieces
}

fn semantic_concepts(tokens: &[String]) -> Vec<&'static str> {
    let has = |words: &[&str]| tokens.iter().any(|token| words.contains(&token.as_str()));
    let mut concepts = Vec::new();
    if has(&["embed", "embedding", "semantic", "vector"]) {
        concepts.push("embedding");
    }
    if has(&["delete", "remove", "drop", "purge"]) && has(&["file", "source", "node", "edge"]) {
        concepts.push("delete-source");
    }
    if has(&["test", "spec", "coverage"]) && has(&["risk", "review", "change", "diff"]) {
        concepts.push("review-coverage");
    }
    if has(&["embed", "embedding", "semantic", "vector"]) && has(&["search", "rank", "score"]) {
        concepts.push("semantic-search");
    }
    if has(&["mcp", "tool", "agent", "json"]) {
        concepts.push("agent-interface");
    }
    if has(&["config", "mcp", "setup"]) || has(&["watch", "daemon", "hook", "install"]) {
        concepts.push("automation");
    }
    if has(&["graph", "node", "edge", "flow"]) && has(&["rank", "impact", "path", "community"]) {
        concepts.push("graph-reasoning");
    }
    concepts
}

fn char_ngrams(token: &str, min_n: usize, max_n: usize) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    let mut out = Vec::new();
    for n in min_n..=max_n {
        if chars.len() < n {
            continue;
        }
        for i in 0..=chars.len() - n {
            out.push(chars[i..i + n].iter().collect());
        }
    }
    out
}

fn push_signed_hashed_feature(vector: &mut [f32], feature: &str, weight: f32) {
    let hash = stable_hash64(feature.as_bytes(), 0xcbf29ce484222325);
    let index = (hash as usize) % vector.len();
    let sign = if stable_hash64(feature.as_bytes(), 0x9e3779b97f4a7c15) & 1 == 0 {
        1.0
    } else {
        -1.0
    };
    vector[index] += sign * weight;
}

fn stable_hash64(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

pub fn encode_embedding(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

pub fn decode_embedding(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % std::mem::size_of::<f32>() != 0 {
        return None;
    }
    let mut vector = Vec::with_capacity(blob.len() / std::mem::size_of::<f32>());
    for chunk in blob.chunks_exact(std::mem::size_of::<f32>()) {
        vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(vector)
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (l, r) in left.iter().zip(right.iter()) {
        dot += l * r;
        left_norm += l * l;
        right_norm += r * r;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}
