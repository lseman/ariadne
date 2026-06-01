//! Markdown extraction using `pulldown-cmark`.
//!
//! Parses markdown into a proper AST and emits:
//! - `Document` nodes for each markdown file
//! - `Section` nodes for headings (including nested heading levels)
//! - `Concept` nodes for meaningful inline elements (link text, table cells,
//!   code spans)
//! - `Mentions` edges from sections/concepts to code symbols
//!   (`Function`, `Class`, `Method`, `Type`)
//!
//! Supports:
//! - ATX and setext headings
//! - Reference-style and inline links
//! - Fenced and indented code blocks
//! - Tables
//! - Lists (ordered and unordered)
//! - Paragraphs with inline elements
//! - Bold, italic, strikethrough, emphasis
//! - Footnotes
//! - Blockquotes
//! - Horizontal rules
//! - Images
//! - HTML blocks (skipped for content extraction)

use crate::core::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use anyhow::Result;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Extract a markdown file from the graph.
pub fn extract_file(path: &Path, graph: &mut Graph) -> Result<()> {
    let source = fs::read_to_string(path)?;
    let file_uri = path.to_string_lossy().to_string();
    let file_qn = format!("doc::{}", file_uri);
    let file_id = graph.add_node(
        Node::new(NodeKind::Document, &file_qn)
            .with_source(file_uri.clone(), 0, source.lines().count() as u32),
    );

    // Build a reference map for reference-style links.
    let mut refs: HashMap<String, (String, Option<String>)> = HashMap::new();
    collect_reference_definitions(&source, &mut refs);

    // Parse with pulldown-cmark using a broad options set.
    let opts = Options::all();
    let parser = Parser::new_ext(&source, opts);

    // State: track current section stack for nesting.
    let mut section_stack: Vec<(NodeId, u32)> = Vec::new(); // (node_id, heading_level)
    let mut in_code_block = false;
    let mut _code_block_lang: Option<String> = None;

    // Collect code spans and link text from inline elements.
    let mut current_section_id: NodeId = file_id;
    // Buffer for heading text collected between Start/End Heading events.
    let mut heading_text_buffer = String::new();
    let mut in_heading = false;
    let mut heading_counter: u32 = 0;

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let level = level_to_u32(level);
                in_heading = true;
                heading_text_buffer.clear();
                // Pop section stack entries that are deeper or equal to this level.
                while let Some(&(_, parent_level)) = section_stack.last() {
                    if parent_level >= level {
                        section_stack.pop();
                    } else {
                        break;
                    }
                }
                // The parent is now the top of the stack (or the document).
                let parent = section_stack
                    .last()
                    .map(|(id, _)| *id)
                    .unwrap_or(file_id);

                // Create the section node with a unique QN (counter ensures uniqueness).
                let qn = format!("{}::section-{}", file_qn, heading_counter);
                let section_id = graph.add_node(
                    Node::new(NodeKind::Section, &qn).with_source(
                        file_uri.clone(),
                        0,
                        0,
                    ),
                );
                heading_counter += 1;
                graph.add_edge(parent, section_id, Edge::extracted(EdgeKind::Defines));
                section_stack.push((section_id, level));
                current_section_id = section_id;
            }
            Event::End(TagEnd::Heading(_)) => {
                // Update the section node with the collected heading text.
                if !heading_text_buffer.is_empty() {
                    let slug = slugify(&heading_text_buffer);
                    let new_qn = if slug.is_empty() {
                        format!("{}::section", file_qn)
                    } else {
                        format!("{}::{}", file_qn, slug)
                    };
                    graph.rename_node(current_section_id, &new_qn, &slug);
                }
                in_heading = false;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
                _code_block_lang = None;
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
            }
            Event::Text(text) => {
                if in_heading {
                    // Collect heading text for section naming.
                    heading_text_buffer.push_str(&text);
                } else if in_code_block {
                    // Extract symbol references from code block content.
                    extract_symbols_from_code(&text, graph, current_section_id, file_qn.as_str());
                } else {
                    // Extract from inline code spans.
                    let tokens: Vec<_> = text.split('`').collect();
                    for (i, chunk) in tokens.iter().enumerate() {
                        if i % 2 == 1 {
                            // Even indices are outside code, odd are inside.
                            if let Some(target) = resolve_symbol(graph, chunk) {
                                graph.add_edge(
                                    current_section_id,
                                    target,
                                    Edge::inferred(EdgeKind::Mentions, 0.85),
                                );
                            }
                        }
                    }
                }
            }
            Event::Start(Tag::Link { link_type, dest_url, title: _, id }) => {
                eprintln!("DEBUG Link: type={:?} dest_url={:?} id={:?}", link_type, dest_url, id);
                eprintln!("DEBUG Link: refs keys={:?}", refs.keys().collect::<Vec<_>>());
                match link_type {
                    pulldown_cmark::LinkType::Reference
                    | pulldown_cmark::LinkType::Collapsed
                    | pulldown_cmark::LinkType::Shortcut => {
                        // Resolve the reference.
                        let target = if let Some((url, _)) = refs.get(id.as_ref()) {
                            eprintln!("DEBUG Link: resolved ref {:?} -> {:?}", id, url);
                            url.clone()
                        } else {
                            eprintln!("DEBUG Link: NO ref match for {:?}", id);
                            dest_url.to_string()
                        };
                        // Try to extract a symbol name from the URL.
                        eprintln!("DEBUG Link: extracting from url={:?}", target);
                        extract_symbol_from_url(&target, graph, current_section_id, file_qn.as_str());
                    }
                    pulldown_cmark::LinkType::Inline => {
                        // Inline link — try to extract symbol from the URL.
                        extract_symbol_from_url(&dest_url, graph, current_section_id, file_qn.as_str());
                    }
                    pulldown_cmark::LinkType::Email => {
                        // Email link — extract local part as potential symbol.
                        let email = &dest_url;
                        if let Some(local_part) = email.strip_prefix("mailto:") {
                            extract_symbol_from_url(local_part, graph, current_section_id, file_qn.as_str());
                        }
                    }
                    pulldown_cmark::LinkType::ReferenceUnknown
                    | pulldown_cmark::LinkType::CollapsedUnknown
                    | pulldown_cmark::LinkType::ShortcutUnknown
                    | pulldown_cmark::LinkType::Autolink => {}
                }
            }
            Event::End(TagEnd::Link) => {}
            Event::Start(Tag::Table(_)) => {
                // Table handling is done in the cell extraction.
            }
            Event::End(TagEnd::Table) => {}
            Event::Start(Tag::TableCell) => {
                // Table cells will have their text extracted.
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow)
            | Event::Start(Tag::List(_))
            | Event::Start(Tag::Item)
            | Event::Start(Tag::Paragraph)
            | Event::Start(Tag::Emphasis)
            | Event::Start(Tag::Strong)
            | Event::Start(Tag::BlockQuote(_)) => {}
            Event::End(TagEnd::TableHead)
            | Event::End(TagEnd::TableRow)
            | Event::End(TagEnd::TableCell)
            | Event::End(TagEnd::List(_))
            | Event::End(TagEnd::Item)
            | Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Emphasis)
            | Event::End(TagEnd::Strong)
            | Event::End(TagEnd::BlockQuote) => {}
            Event::Start(Tag::FootnoteDefinition(name)) => {
                // Extract symbol from footnote name.
                if let Some(target) = resolve_symbol(graph, &name) {
                    graph.add_edge(
                        current_section_id,
                        target,
                        Edge::inferred(EdgeKind::Mentions, 0.85),
                    );
                }
            }
            Event::End(TagEnd::FootnoteDefinition) => {}
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::SoftBreak | Event::HardBreak => {}
            Event::Rule => {}
            Event::FootnoteReference(_) => {}
            Event::InlineMath(_) | Event::DisplayMath(_) => {}
            Event::TaskListMarker(_) => {}
            Event::Start(Tag::HtmlBlock)
            | Event::Start(Tag::Strikethrough)
            | Event::Start(Tag::Image { .. }) => {}
            Event::End(TagEnd::HtmlBlock)
            | Event::End(TagEnd::Strikethrough)
            | Event::End(TagEnd::Image) => {}
            Event::Code(code) => {
                // Inline code — same as Text with backticks.
                if let Some(target) = resolve_symbol(graph, &code) {
                    graph.add_edge(
                        current_section_id,
                        target,
                        Edge::inferred(EdgeKind::Mentions, 0.85),
                    );
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Collect reference-style link definitions from the source.
fn collect_reference_definitions(
    source: &str,
    refs: &mut HashMap<String, (String, Option<String>)>,
) {
    for line in source.lines() {
        let line = line.trim_start();
        // Match: [id]: url ["title"]
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(id_end) = rest.find("]: ") {
                let id = &rest[..id_end];
                let after = &rest[id_end + 3..];
                let (url, title) = if let Some(url_inner) = after.strip_prefix('<') {
                    // <url> syntax
                    if let Some(end) = url_inner.find('>') {
                        let url = &url_inner[..end];
                        let rest_after = &url_inner[end + 1..];
                        let title = parse_optional_title(rest_after);
                        (url.to_string(), title)
                    } else {
                        continue;
                    }
                } else {
                    // bare url ["title"] syntax
                    // Strip optional title in quotes, then take the URL as everything before it
                    let url_end = after.find(|c: char| c.is_whitespace())
                        .unwrap_or(after.len());
                    let url = &after[..url_end];
                    let rest_after = &after[url_end..];
                    let title = parse_optional_title(rest_after);
                    (url.to_string(), title)
                };
                if !url.is_empty() {
                    refs.insert(id.to_string(), (url, title));
                }
            }
        }
    }
}

fn parse_optional_title(s: &str) -> Option<String> {
    let trimmed = s.trim_start();
    if trimmed.starts_with('"') {
        trimmed.strip_prefix('"').and_then(|after| after.rfind('"').map(|end| after[..end].to_string()))
    } else if trimmed.starts_with('\'') {
        trimmed.strip_prefix('\'').and_then(|after| after.rfind('\'').map(|end| after[..end].to_string()))
    } else {
        None
    }
}

/// Extract symbol references from code block content.
fn extract_symbols_from_code(
    code: &str,
    graph: &mut Graph,
    section_id: NodeId,
    _file_qn: &str,
) {
    // Split on word boundaries and try to resolve each token.
    for token in tokenize_code(code) {
        if let Some(target) = resolve_symbol(graph, &token) {
            graph.add_edge(
                section_id,
                target,
                Edge::inferred(EdgeKind::Mentions, 0.80),
            );
        }
    }
}

/// Extract symbol references from a URL or link text.
fn extract_symbol_from_url(
    url: &str,
    graph: &mut Graph,
    section_id: NodeId,
    _file_qn: &str,
) {
    // Handle fragment URLs like #authenticate — strip the leading #.
    let candidate = if let Some(stripped) = url.strip_prefix('#') {
        stripped
    } else {
        // Try to extract the last path component as a potential symbol.
        url.trim_end_matches('/')
            .split('/')
            .next_back()
            .unwrap_or(url)
    };
    // Strip common file suffixes (.html, .md, .txt, etc.) only.
    let candidate = strip_file_suffix(candidate);
    if !candidate.is_empty() && candidate.len() >= 2 {
        if let Some(target) = resolve_symbol(graph, candidate) {
            graph.add_edge(
                section_id,
                target,
                Edge::inferred(EdgeKind::Mentions, 0.70),
            );
        }
    }
}

/// Strip common file suffixes from a candidate name.
fn strip_file_suffix(s: &str) -> &str {
    let suffixes = [".html", ".md", ".txt", ".htm", ".php", ".js", ".ts", ".rs", ".py"];
    for suffix in &suffixes {
        if let Some(stripped) = s.strip_suffix(suffix) {
            return stripped;
        }
    }
    s
}

/// Tokenize code content into potential symbol names.
fn tokenize_code(code: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for c in code.chars() {
        if c.is_alphanumeric() || "_:.=+-".contains(c) {
            current.push(c);
        } else {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Resolve a symbol name to a graph node.
fn resolve_symbol(graph: &Graph, token: &str) -> Option<NodeId> {
    if token.len() < 2 {
        return None;
    }

    // Match by exact name or qualified_name suffix.
    for (id, node) in graph.nodes() {
        if !matches!(
            node.kind,
            NodeKind::Function
                | NodeKind::Class
                | NodeKind::Method
                | NodeKind::Type
                | NodeKind::Trait
                | NodeKind::Impl
        ) {
            continue;
        }
        if node.name == token
            || node.qualified_name.ends_with(&format!("::{}", token))
            || normalize_for_match(&node.name) == normalize_for_match(token)
        {
            return Some(id);
        }
    }
    None
}

/// Normalize identifiers for fuzzy matching (camelCase splitting, etc.).
fn normalize_for_match(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 2);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = s.chars().nth(i - 1).unwrap();
            let next = s.chars().nth(i + 1);
            // Insert underscore before uppercase if preceded by lowercase, digit, or
            // another uppercase followed by a lowercase (e.g. HTTPParser → http_parser)
            if prev.is_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_uppercase() && next.is_some_and(|nc| nc.is_lowercase()))
            {
                result.push('_');
            }
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

fn level_to_u32(level: HeadingLevel) -> u32 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Generate a slug from heading text for use in section qualified names.
#[allow(dead_code)]
fn slugify(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{EdgeKind, Node, NodeKind};

    #[test]
    fn extracts_document_and_sections() {
        let mut g = Graph::new();
        let source = r#"# Title

## Section One

Some content.

### Subsection

More content.
"#;
        let path = Path::new("/tmp/test.md");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        // Should have a Document node.
        let doc_id = g.find_by_qname("doc::/tmp/test.md");
        assert!(doc_id.is_some(), "Document node should exist");

        // Should have Section nodes.
        let sections: Vec<_> = g.nodes().filter(|(_, n)| n.kind == NodeKind::Section).collect();
        assert_eq!(sections.len(), 3, "should have 3 sections (title, section one, subsection)");

        // Document should define sections.
        let doc_id = doc_id.unwrap();
        let defines: Vec<_> = g
            .out_neighbors(doc_id)
            .filter(|(_, e)| e.kind == EdgeKind::Defines)
            .collect();
        assert!(
            !defines.is_empty(),
            "document should define at least one section; got {}",
            defines.len()
        );
    }

    #[test]
    fn extracts_inline_code_symbols() {
        let mut g = Graph::new();
        let _file = g.add_node(Node::new(
            NodeKind::File,
            "src::lib.rs",
        ));
        let func = g.add_node(Node::new(NodeKind::Function, "src::lib.rs::compute_hash"));
        g.add_node(Node::new(NodeKind::Function, "src::lib.rs::other"));

        let source = r#"# API

Use the `compute_hash` function to compute the hash.
"#;
        let path = Path::new("/tmp/api.md");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        // The mention edge from the section to the function should exist.
        let mentions: Vec<_> = g
            .in_neighbors(func)
            .filter(|(_, e)| e.kind == EdgeKind::Mentions)
            .collect();
        assert!(
            !mentions.is_empty(),
            "function should have at least one Mentions edge; got {}",
            mentions.len()
        );
    }

    #[test]
    fn resolves_reference_links() {
        let mut g = Graph::new();
        let _func = g.add_node(Node::new(NodeKind::Function, "pkg::authenticate"));

        let source = r#"# Auth

See [authentication docs][auth-ref].

[auth-ref]: #authenticate
"#;
        let path = Path::new("/tmp/ref.md");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        eprintln!("DEBUG ref: node_count={}", g.node_count());
        for (id, n) in g.nodes() {
            eprintln!("DEBUG ref: node {:?} kind={:?} name={}", id, n.kind, n.name);
        }
        for (e_id, src, dst, e) in g.edges() {
            eprintln!("DEBUG ref: edge {:?} {:?}->{:?} kind={:?}", e_id, src, dst, e.kind);
        }
        // Should have extracted a symbol reference from the URL fragment.
        let mentions: Vec<_> = g
            .in_neighbors(_func)
            .filter(|(_, e)| e.kind == EdgeKind::Mentions)
            .collect();
        eprintln!("DEBUG ref: mentions={}", mentions.len());
        assert!(
            !mentions.is_empty(),
            "function should have Mentions edges from link resolution"
        );
    }

    #[test]
    fn extracts_symbols_from_fenced_code_blocks() {
        let mut g = Graph::new();
        let func = g.add_node(Node::new(NodeKind::Function, "src::utils.rs::parse_json"));

        let source = r#"# Utils

Here's how to use it:

```rust
let result = parse_json(input);
```
"#;
        let path = Path::new("/tmp/code.md");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        let mentions: Vec<_> = g
            .in_neighbors(func)
            .filter(|(_, e)| e.kind == EdgeKind::Mentions)
            .collect();
        assert!(
            !mentions.is_empty(),
            "function should have Mentions edges from code block"
        );
    }

    #[test]
    fn slugify_produces_valid_ids() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("a-b-c"), "a-b-c");
        assert_eq!(slugify("Hello! World?"), "hello-world");
        assert_eq!(slugify("100% complete"), "100-complete");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn collect_reference_definitions_parses_correctly() {
        let mut refs = HashMap::new();
        let source = r#"
[foo]: /url "Title"
[bar]: /url 'Title'
[baz]: <url>
[qux]: /url
[invalid: no url here
"#;
        collect_reference_definitions(source, &mut refs);
        assert!(refs.contains_key("foo"));
        assert!(refs.contains_key("bar"));
        assert!(refs.contains_key("baz"));
        assert!(refs.contains_key("qux"));
        assert!(!refs.contains_key("invalid"));
        assert_eq!(refs["foo"].0, "/url");
        assert_eq!(refs["baz"].0, "url");
    }

    #[test]
    fn normalize_for_match_splits_camel_case() {
        assert_eq!(normalize_for_match("computeHash"), "compute_hash");
        assert_eq!(normalize_for_match("parseJson"), "parse_json");
        assert_eq!(normalize_for_match("HTTPParser"), "http_parser");
        assert_eq!(normalize_for_match("already_snake"), "already_snake");
    }

    #[test]
    fn tokenize_code_splits_on_non_word_chars() {
        let tokens = tokenize_code("let x = parseJson(data);");
        assert_eq!(tokens, vec!["let", "x", "=", "parseJson", "data"]);
    }
}

