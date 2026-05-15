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
        }
    }

    pub fn with_source(mut self, uri: impl Into<String>, line_start: u32, line_end: u32) -> Self {
        self.source_uri = Some(uri.into());
        self.line_start = Some(line_start);
        self.line_end = Some(line_end);
        self
    }

    pub fn with_property(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.properties.insert(key.into(), value);
        self
    }
}
