use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The kind of entity a node represents.
///
/// Kinds are split into three families:
///
/// - **Code**: `File`, `Module`, `Class`, `Function`, `Method`, `Trait`,
///   `Impl`, `Variable`, `Type`.
/// - **Prose**: `Document`, `Section`, `Concept`.
/// - **Visual**: `Diagram`, `Image`.
/// - **Provenance**: `Commit`, `Author`.
/// - **Synthetic**: `Hyperedge` — a node introduced to model an n-ary
///   relationship that cannot be expressed as a single directed edge.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Module,
    Class,
    Function,
    Method,
    Trait,
    Impl,
    Variable,
    Type,
    Document,
    Section,
    Concept,
    Diagram,
    Image,
    Commit,
    Author,
    Hyperedge,
    /// Synthetic node representing an execution flow (entry point + the
    /// transitive set of functions reachable via `Calls` edges, bounded
    /// by depth and node-count limits). Member functions point at it via
    /// `EdgeKind::MemberOf`; the entry function additionally has an
    /// `EdgeKind::EntryOf` edge.
    Flow,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Class => "class",
            Self::Function => "function",
            Self::Method => "method",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Variable => "variable",
            Self::Type => "type",
            Self::Document => "document",
            Self::Section => "section",
            Self::Concept => "concept",
            Self::Diagram => "diagram",
            Self::Image => "image",
            Self::Commit => "commit",
            Self::Author => "author",
            Self::Hyperedge => "hyperedge",
            Self::Flow => "flow",
        }
    }
}

/// A property-bag node in the graph.
///
/// `qualified_name` is the canonical key used for symbol resolution and
/// must be unique within the graph. For source-derived nodes it is built
/// from `<file>::<module-path>::<name>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub source_uri: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    #[serde(default)]
    pub properties: BTreeMap<String, serde_json::Value>,
    /// First commit SHA in which this node is valid. `None` for "always".
    pub valid_from: Option<String>,
    /// Last commit SHA in which this node is valid. `None` for "still valid".
    pub valid_to: Option<String>,
    /// Source code content for this node (function body, class body, etc.).
    /// Used for semantic embeddings. Truncated to 10KB.
    #[serde(default)]
    pub source_text: Option<String>,
}

impl Node {
    pub fn new(kind: NodeKind, qualified_name: impl Into<String>) -> Self {
        let qn = qualified_name.into();
        let name = qn.rsplit("::").next().unwrap_or(&qn).to_string();
        Self {
            kind,
            name,
            qualified_name: qn,
            source_uri: None,
            line_start: None,
            line_end: None,
            properties: BTreeMap::new(),
            valid_from: None,
            valid_to: None,
            source_text: None,
        }
    }

    pub fn with_source(mut self, uri: impl Into<String>, line_start: u32, line_end: u32) -> Self {
        self.source_uri = Some(uri.into());
        self.line_start = Some(line_start);
        self.line_end = Some(line_end);
        self
    }

    /// Attach source code text for this node. Truncated to 10KB.
    pub fn with_source_text(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.source_text = Some(if text.len() > 10_000 {
            text[..10_000].to_string()
        } else {
            text
        });
        self
    }

    pub fn with_property(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.properties.insert(key.into(), value);
        self
    }
}
