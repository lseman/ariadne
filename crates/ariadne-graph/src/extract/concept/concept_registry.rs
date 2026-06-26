//! Concept (prose + diagram) registry.
//!
//! Maps file extensions to concept extractors. After AST extraction
//! fails to match a file, the walker checks this registry for
//! document/diagram formats.
//!
//! Each entry is `(extension, extractor_function)`. Extension is
//! lowercased, without leading dot.

use std::path::Path;

type Extractor = fn(&std::path::Path, &mut dyn crate::core::GraphMut) -> anyhow::Result<()>;

/// All concept extractors indexed by extension.
const CONCEPT_EXTRACTORS: &[(&str, Extractor)] = &[
    ("md", extract_markdown),
    ("markdown", extract_markdown),
    ("html", extract_html),
    ("htm", extract_html),
    ("tex", extract_latex),
    ("svg", extract_svg),
];

/// Look up a concept extractor by file path. Returns None if no
/// document/diagram extractor matches.
pub fn get_by_path(path: &Path) -> Option<Extractor> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    CONCEPT_EXTRACTORS
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, f)| *f)
}

// Re-export for the registry entries.
use super::super::vision::svg::extract_file as extract_svg;
use super::html::extract_file as extract_html;
use super::latex::extract_file as extract_latex;
use super::markdown::extract_file as extract_markdown;

/// Resolve mentions across all concept extractors.
///
/// Idempotent: running multiple times adds no duplicate edges.
pub fn resolve_all_mentions(graph: &mut dyn crate::core::GraphMut) -> usize {
    let added = super::markdown::resolve_mentions(graph);
    // HTML delegates to markdown's resolver, SVG has no mentions yet.
    added
}
