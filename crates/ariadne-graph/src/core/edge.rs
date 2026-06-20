use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The kind of relationship between two nodes.
///
/// Structural kinds (top group) are produced deterministically by AST
/// extraction and always carry [`Confidence::Extracted`].
///
/// Semantic kinds are produced by document and vision passes and carry
/// [`Confidence::Inferred`] with a per-edge score.
///
/// Cross-modal kinds bridge code and prose/visuals.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    // Structural
    Defines,
    Calls,
    Imports,
    Inherits,
    Implements,
    ReadsWrites,
    /// Reverse of a test call: `production_symbol` -[TestedBy]-> `test_fn`.
    /// Derived in post-extraction from `Calls` edges whose source carries
    /// `is_test=true`. Lets `tests_for(symbol)` reduce to a single
    /// `in_neighbors` scan and lets review context flag uncovered symbols.
    TestedBy,
    /// `function` -[MemberOf]-> `flow_node`. Bookkeeping edge linking a
    /// member function to its flow. Use with care in graph algorithms —
    /// flows are an overlay, not a structural relationship, so weights
    /// are set near zero.
    MemberOf,
    /// `entry_function` -[EntryOf]-> `flow_node`. Marks the seed of a
    /// flow distinctly from the rest of its members.
    EntryOf,

    // Semantic
    Mentions,
    Describes,
    SimilarTo,
    RationaleFor,

    // Cross-modal
    Illustrates,
    DocumentedBy,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Defines => "defines",
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Inherits => "inherits",
            Self::Implements => "implements",
            Self::ReadsWrites => "reads_writes",
            Self::TestedBy => "tested_by",
            Self::MemberOf => "member_of",
            Self::EntryOf => "entry_of",
            Self::Mentions => "mentions",
            Self::Describes => "describes",
            Self::SimilarTo => "similar_to",
            Self::RationaleFor => "rationale_for",
            Self::Illustrates => "illustrates",
            Self::DocumentedBy => "documented_by",
        }
    }

    pub fn is_structural(&self) -> bool {
        matches!(
            self,
            Self::Defines
                | Self::Calls
                | Self::Imports
                | Self::Inherits
                | Self::Implements
                | Self::ReadsWrites
                | Self::TestedBy
                | Self::MemberOf
                | Self::EntryOf
        )
    }

    pub fn is_semantic(&self) -> bool {
        matches!(
            self,
            Self::Mentions | Self::Describes | Self::SimilarTo | Self::RationaleFor
        )
    }
}

/// Confidence tag attached to every edge.
///
/// - `Extracted` — derived by deterministic AST analysis. Score = 1.0.
/// - `Inferred(s)` — derived semantically (by embeddings,
///   name-matching heuristics) with a per-edge score in `[0, 1]`.
/// - `Ambiguous` — the extractor saw evidence but cannot commit to a
///   score. Surfaced for human review.
///
/// Keeping confidence as a first-class enum, rather than just a float,
/// lets queries express "structural only" and "semantic only" filters
/// without scanning properties.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "score", rename_all = "snake_case")]
pub enum Confidence {
    Extracted,
    Inferred(f32),
    Ambiguous,
}

impl Confidence {
    pub fn score(&self) -> f32 {
        match self {
            Self::Extracted => 1.0,
            Self::Inferred(s) => *s,
            Self::Ambiguous => 0.0,
        }
    }

    pub fn class_str(&self) -> &'static str {
        match self {
            Self::Extracted => "extracted",
            Self::Inferred(_) => "inferred",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub kind: EdgeKind,
    pub confidence: Confidence,
    #[serde(default)]
    pub properties: BTreeMap<String, serde_json::Value>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
}

impl Edge {
    pub fn extracted(kind: EdgeKind) -> Self {
        Self {
            kind,
            confidence: Confidence::Extracted,
            properties: BTreeMap::new(),
            valid_from: None,
            valid_to: None,
        }
    }

    pub fn inferred(kind: EdgeKind, score: f32) -> Self {
        Self {
            kind,
            confidence: Confidence::Inferred(score.clamp(0.0, 1.0)),
            properties: BTreeMap::new(),
            valid_from: None,
            valid_to: None,
        }
    }

    pub fn ambiguous(kind: EdgeKind) -> Self {
        Self {
            kind,
            confidence: Confidence::Ambiguous,
            properties: BTreeMap::new(),
            valid_from: None,
            valid_to: None,
        }
    }
}
