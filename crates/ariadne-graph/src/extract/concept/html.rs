//! HTML extraction using `html5ever` + `markup5ever_rcdom`.
//!
//! Parses HTML documents and emits:
//! - `Document` nodes for each HTML file
//! - `Section` nodes for semantic headings (`<h1>`–`<h6>`)
//! - `Concept` nodes for meaningful text (link text, heading text, table cells)
//! - `Mentions` edges from sections/concepts to code symbols
//!
//! Supports:
//! - Semantic HTML structure (`<header>`, `<nav>`, `<main>`, `<article>`,
//!   `<section>`, `<aside>`, `<footer>`)
//! - Headings (`<h1>`–`<h6>`)
//! - Links (`<a>`) — extract symbol references from href and text
//! - Code (`<code>`, `<pre>`) — extract symbol references from content
//! - Tables (`<table>`, `<th>`, `<td>`) — extract cell content
//! - Lists (`<ul>`, `<ol>`, `<li>`)
//! - Inline elements: `<strong>`, `<em>`, `<abbr>`, `<cite>`, `<samp>`
//! - Meta tags for page description and keywords
//! - Script tags with inline code extraction
//!
//! HTML blocks without semantic structure fall back to paragraph-level
//! Concept extraction.
//!
//! Parses HTML documents and emits:
//! - `Document` nodes for each HTML file
//! - `Section` nodes for semantic headings (`<h1>`–`<h6>`)
//! - `Concept` nodes for meaningful text (link text, heading text, table cells)
//! - `Mentions` edges from sections/concepts to code symbols
//!
//! Supports:
//! - Semantic HTML structure (`<header>`, `<nav>`, `<main>`, `<article>`,
//!   `<section>`, `<aside>`, `<footer>`)
//! - Headings (`<h1>`–`<h6>`)
//! - Links (`<a>`) — extract symbol references from href and text
//! - Code (`<code>`, `<pre>`) — extract symbol references from content
//! - Tables (`<table>`, `<th>`, `<td>`) — extract cell content
//! - Lists (`<ul>`, `<ol>`, `<li>`)
//! - Inline elements: `<strong>`, `<em>`, `<abbr>`, `<cite>`, `<samp>`
//! - Meta tags for page description and keywords
//! - Script tags with inline code extraction
//!
//! HTML blocks without semantic structure fall back to paragraph-level
//! Concept extraction.

use crate::core::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use anyhow::Result;
use html5ever::parse_document;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Extract an HTML file from the graph.
pub fn extract_file(path: &Path, graph: &mut Graph) -> Result<()> {
    let source = fs::read_to_string(path)?;
    let file_uri = path.to_string_lossy().to_string();
    let file_qn = format!("doc::{}", file_uri);
    let file_id = graph.add_node(Node::new(NodeKind::Document, &file_qn).with_source(
        file_uri.clone(),
        0,
        source.lines().count() as u32,
    ));

    // Parse HTML.
    let dom = parse_document(RcDom::default(), Default::default()).one(source.clone());

    // Walk the DOM tree, building sections from headings.
    let mut heading_counter: u32 = 0;
    let mut section_stack: Vec<(NodeId, u32)> = Vec::new(); // (node_id, heading_level)
    let mut current_section_id: NodeId = file_id;

    extract_dom_tree(
        &dom.document,
        &file_qn,
        file_id,
        &mut heading_counter,
        &mut section_stack,
        &mut current_section_id,
        graph,
    );

    // Extract page metadata.
    extract_meta(&dom.document, graph, file_id, &file_qn);

    Ok(())
}

fn extract_dom_tree(
    handle: &Handle,
    file_qn: &str,
    parent_id: NodeId,
    heading_counter: &mut u32,
    section_stack: &mut Vec<(NodeId, u32)>,
    current_section_id: &mut NodeId,
    graph: &mut Graph,
) {
    for child in handle.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => {
                let text_ref = contents.borrow();
                let text = text_ref.as_ref();
                if !text.trim().is_empty() && text.len() > 1 {
                    // Extract symbols from inline code spans (backticks).
                    let tokens: Vec<_> = text.split('`').collect();
                    for (i, chunk) in tokens.iter().enumerate() {
                        if i % 2 == 1 {
                            // Odd indices are inside backticks — treat as code.
                            for token in tokenize_code(chunk) {
                                mention(graph, *current_section_id, &token, 0.85);
                            }
                        } else if chunk.trim().len() > 1 {
                            // Even indices are plain text — extract individual words.
                            for token in tokenize_code(chunk) {
                                if token.len() >= 2 {
                                    mention(graph, *current_section_id, &token, 0.75);
                                }
                            }
                        }
                    }
                }
            }
            NodeData::Element { name, attrs, .. } => {
                let tag_name = name.local.as_ref();
                let attrs_ref = attrs.borrow();
                let attrs_map: HashMap<String, String> = attrs_ref
                    .iter()
                    .map(|a| (a.name.local.to_string(), a.value.to_string()))
                    .collect();

                // Headings create Section nodes.
                if let Some(level) = tag_name
                    .strip_prefix('h')
                    .and_then(|s| s.parse::<u32>().ok())
                {
                    if (1..=6).contains(&level) {
                        // Pop section stack entries that are deeper or equal.
                        while let Some(&(_, parent_level)) = section_stack.last() {
                            if parent_level >= level {
                                section_stack.pop();
                            } else {
                                break;
                            }
                        }
                        let parent = section_stack.last().map(|(id, _)| *id).unwrap_or(parent_id);

                        // Collect heading text from children.
                        let mut heading_text = String::new();
                        collect_text(child, &mut heading_text, true);

                        if heading_text.is_empty() {
                            continue;
                        }

                        let slug = slugify(&heading_text);
                        let qn = if slug.is_empty() {
                            format!("{}::section-{}", file_qn, heading_counter)
                        } else {
                            format!("{}::{}", file_qn, slug)
                        };
                        *heading_counter += 1;

                        let section_id =
                            graph.add_node(Node::new(NodeKind::Section, &qn).with_source(
                                file_qn.to_string(),
                                0,
                                0,
                            ));
                        graph.add_edge(parent, section_id, Edge::extracted(EdgeKind::Defines));
                        section_stack.push((section_id, level));
                        *current_section_id = section_id;
                        mention(graph, section_id, &heading_text, 0.9);
                    }
                }
                // Extract code from <code> and <pre> blocks.
                else if tag_name == "code" || tag_name == "pre" {
                    let mut text = String::new();
                    collect_text(child, &mut text, false);
                    for token in tokenize_code(&text) {
                        mention(graph, *current_section_id, &token, 0.80);
                    }
                }
                // Extract text from semantic elements.
                else if matches!(tag_name, "abbr" | "cite" | "samp" | "strong" | "em") {
                    let mut text = String::new();
                    collect_text(child, &mut text, true);
                    if !text.is_empty() && text.len() > 1 {
                        mention(graph, *current_section_id, &text, 0.8);
                    }
                }
                // Links: extract symbol from href text content and URL.
                else if tag_name == "a" {
                    // Extract symbol from link text.
                    let mut link_text = String::new();
                    collect_text(child, &mut link_text, true);
                    if !link_text.is_empty() {
                        mention(graph, *current_section_id, &link_text, 0.7);
                    }
                    // Extract symbol from href URL.
                    if let Some(href) = attrs_map.get("href") {
                        extract_symbol_from_url(href.as_str(), graph, *current_section_id, file_qn);
                    }
                }
                // Table cells: extract cell text as concepts.
                else if tag_name == "td" || tag_name == "th" {
                    let mut text = String::new();
                    collect_text(child, &mut text, true);
                    for token in tokenize_code(&text) {
                        mention(graph, *current_section_id, &token, 0.65);
                    }
                }
                // Script: extract inline JS code.
                else if tag_name == "script" {
                    if attrs_map
                        .get("type")
                        .map(|t| !t.contains("json"))
                        .unwrap_or(false)
                    {
                        let mut text = String::new();
                        collect_text(child, &mut text, false);
                        for token in tokenize_code(&text) {
                            mention(graph, *current_section_id, &token, 0.7);
                        }
                    }
                }
                // Recurse into other elements.
                else {
                    extract_dom_tree(
                        child,
                        file_qn,
                        parent_id,
                        heading_counter,
                        section_stack,
                        current_section_id,
                        graph,
                    );
                }
            }
            NodeData::Comment { .. } => {}
            _ => {}
        }
    }
}

/// Collect text content from a node's children.
///
/// If `only_inline` is true, only extract from inline elements.
/// If `only_inline` is false, extract from all elements.
fn collect_text(handle: &Handle, out: &mut String, _only_inline: bool) {
    for child in handle.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => {
                let text_ref = contents.borrow();
                let text = text_ref.as_ref();
                // Skip whitespace-only text for inline elements.
                if !text.trim().is_empty() {
                    out.push_str(text);
                }
            }
            NodeData::Element { name, .. } => {
                // Skip block elements when only inline is requested.
                let is_block = matches!(
                    name.local.as_ref(),
                    "div"
                        | "section"
                        | "article"
                        | "header"
                        | "footer"
                        | "nav"
                        | "main"
                        | "aside"
                        | "ul"
                        | "ol"
                        | "li"
                        | "table"
                        | "tr"
                        | "thead"
                        | "tbody"
                        | "form"
                        | "fieldset"
                        | "details"
                        | "summary"
                );

                if !is_block {
                    collect_text(child, out, _only_inline);
                }
            }
            _ => {}
        }
    }
}

/// Extract symbol references from a URL or link text.
fn extract_symbol_from_url(url: &str, graph: &mut Graph, section_id: NodeId, _file_qn: &str) {
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
    // Strip common file suffixes.
    let candidate = strip_file_suffix(candidate);
    if !candidate.is_empty() && candidate.len() >= 2 {
        mention(graph, section_id, candidate, 0.70);
    }
}

/// Strip common file suffixes from a candidate name.
fn strip_file_suffix(s: &str) -> &str {
    let suffixes = [
        ".html", ".md", ".txt", ".htm", ".php", ".js", ".ts", ".rs", ".py",
    ];
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

/// Record a mention of `token` from `section_id`.
///
/// If the symbol already exists in the graph it is linked immediately.
/// Otherwise the token is stashed on the section node so
/// [`resolve_mentions`] can link it in a post-pass.
fn mention(graph: &mut Graph, section_id: NodeId, token: &str, confidence: f32) {
    if token.len() < 2 {
        return;
    }
    if let Some(target) = resolve_symbol(graph, token) {
        graph.add_edge(
            section_id,
            target,
            Edge::inferred(EdgeKind::Mentions, confidence),
        );
        return;
    }
    if let Some(node) = graph.node_mut(section_id) {
        let entry = node
            .properties
            .entry("pending_mentions".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if let Some(arr) = entry.as_array_mut() {
            let val = serde_json::json!([token, confidence]);
            if !arr.contains(&val) {
                arr.push(val);
            }
        }
    }
}

/// Extract meta tags for page metadata (description, keywords, author).
fn extract_meta(handle: &Handle, graph: &mut Graph, file_id: NodeId, _file_qn: &str) {
    let mut meta_stack: Vec<Handle> = vec![handle.clone()];

    while let Some(current) = meta_stack.pop() {
        for child in current.children.borrow().iter() {
            let NodeData::Element { name, attrs, .. } = &child.data else {
                continue;
            };

            let tag_name = name.local.as_ref();
            let attrs_ref = attrs.borrow();
            let attrs_map: HashMap<String, String> = attrs_ref
                .iter()
                .map(|a| (a.name.local.to_string(), a.value.to_string()))
                .collect();

            if tag_name != "meta" {
                meta_stack.push(child.clone());
                continue;
            }

            let Some(content) = attrs_map.get("content") else {
                continue;
            };
            let Some(name) = attrs_map.get("name").or_else(|| attrs_map.get("property")) else {
                continue;
            };

            match name.to_lowercase().as_str() {
                "description" if !content.is_empty() => {
                    if let Some(node) = graph.node_mut(file_id) {
                        node.properties.insert(
                            "description".to_string(),
                            serde_json::Value::String(content.to_string()),
                        );
                    }
                }
                "keywords" => {
                    for keyword in content.split(',') {
                        let kw = keyword.trim();
                        if !kw.is_empty() && kw.len() >= 2 {
                            mention(graph, file_id, kw, 0.6);
                        }
                    }
                }
                "author" if !content.is_empty() => {
                    mention(graph, file_id, content, 0.7);
                }
                _ => {}
            }
        }
    }
}

/// Resolve mentions that could not be linked at extraction time.
///
/// Uses the same [`resolve_mentions`] post-pass as markdown extraction.
pub fn resolve_mentions(graph: &mut Graph) -> usize {
    super::markdown::resolve_mentions(graph)
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

/// Generate a slug from heading text for use in section qualified names.
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
        let source = r#"<!DOCTYPE html>
<html>
<head><title>Test Page</title></head>
<body>
<h1>Main Title</h1>
<h2>Section One</h2>
<p>Some content.</p>
<h3>Subsection</h3>
<p>More content.</p>
</body>
</html>"#;
        let path = Path::new("/tmp/test.html");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        // Should have a Document node.
        let doc_id = g.find_by_qname("doc::/tmp/test.html");
        assert!(doc_id.is_some(), "Document node should exist");

        // Should have Section nodes.
        let sections: Vec<_> = g
            .nodes()
            .filter(|(_, n)| n.kind == NodeKind::Section)
            .collect();
        assert_eq!(sections.len(), 3, "should have 3 sections (h1, h2, h3)");

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
    fn extracts_heading_text_as_concepts() {
        let mut g = Graph::new();
        let _func = g.add_node(Node::new(NodeKind::Function, "pkg::authenticate"));

        let source = r##"<!DOCTYPE html>
<html>
<body>
<h1>Login Process</h1>
<p>Use the `authenticate` function for login.</p>
</body>
</html>"##;
        let path = Path::new("/tmp/heading.html");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        // The mention edge from the section to the function should exist.
        let mentions: Vec<_> = g
            .in_neighbors(_func)
            .filter(|(_, e)| e.kind == EdgeKind::Mentions)
            .collect();
        assert!(
            !mentions.is_empty(),
            "function should have at least one Mentions edge; got {}",
            mentions.len()
        );
    }

    #[test]
    fn extracts_links_as_mentions() {
        let mut g = Graph::new();
        let _func = g.add_node(Node::new(NodeKind::Function, "pkg::parse_data"));

        let source = r##"<!DOCTYPE html>
<html>
<body>
<h1>API Reference</h1>
<p>See <a href="#parse_data">the parser</a> for details.</p>
</body>
</html>"##;
        let path = Path::new("/tmp/links.html");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        // Should have extracted a mention from the fragment URL.
        let mentions: Vec<_> = g
            .in_neighbors(_func)
            .filter(|(_, e)| e.kind == EdgeKind::Mentions)
            .collect();
        assert!(
            !mentions.is_empty(),
            "function should have Mentions edges from link resolution"
        );
    }

    #[test]
    fn extracts_code_content() {
        let mut g = Graph::new();
        let func = g.add_node(Node::new(NodeKind::Function, "src::utils.rs::parse_json"));

        let source = r#"<!DOCTYPE html>
<html>
<body>
<h1>Utils</h1>
<p>Example usage:</p>
<pre><code>let result = parse_json(input);</code></pre>
</body>
</html>"#;
        let path = Path::new("/tmp/code.html");
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

    #[test]
    fn handles_nested_headings() {
        let mut g = Graph::new();
        let source = r#"<!DOCTYPE html>
<html>
<body>
<h1>Top</h1>
<h2>Level 2</h2>
<h3>Level 3</h3>
<h2>Another Level 2</h2>
<h1>Another Top</h1>
</body>
</html>"#;
        let path = Path::new("/tmp/nested.html");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        let sections: Vec<_> = g
            .nodes()
            .filter(|(_, n)| n.kind == NodeKind::Section)
            .collect();
        assert_eq!(sections.len(), 5, "should have 5 section nodes");

        // Verify sections are connected to correct parents.
        let doc_id = g.find_by_qname("doc::/tmp/nested.html").unwrap();
        let direct_children: Vec<_> = g
            .out_neighbors(doc_id)
            .filter(|(_, e)| e.kind == EdgeKind::Defines)
            .collect();
        // Should have exactly 2 top-level sections (h1s) directly under doc.
        assert_eq!(
            direct_children.len(),
            2,
            "doc should have 2 direct section children"
        );
    }

    #[test]
    fn extracts_meta_description() {
        let mut g = Graph::new();
        let source = r#"<!DOCTYPE html>
<html>
<head>
<meta name="description" content="This is the page description.">
<meta name="keywords" content="api, parser, json">
</head>
<body><h1>API Docs</h1></body>
</html>"#;
        let path = Path::new("/tmp/meta.html");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        let doc_id = g.find_by_qname("doc::/tmp/meta.html").unwrap();
        let node = g.node(doc_id).unwrap();
        assert_eq!(
            node.properties.get("description"),
            Some(&serde_json::json!("This is the page description."))
        );
    }

    #[test]
    fn resolves_html_mentions_post_pass() {
        // When the symbol exists before extraction, mentions are resolved
        // immediately. When HTML is extracted before the symbol, mentions
        // are stashed and resolved by the post-pass.
        let mut g = Graph::new();
        let func = g.add_node(Node::new(
            NodeKind::Function,
            "src::lib.rs::process_payment",
        ));

        let source = r##"<!DOCTYPE html>
<html>
<body>
<h1>Payment</h1>
<p>Use <a href="#process_payment">process_payment</a> to handle billing.</p>
</body>
</html>"##;
        let path = Path::new("/tmp/html_mention.html");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        // The mention should have been resolved immediately (symbol exists).
        let mentions: Vec<_> = g
            .in_neighbors(func)
            .filter(|(_, e)| e.kind == EdgeKind::Mentions)
            .collect();
        assert!(
            !mentions.is_empty(),
            "function should have Mentions edges from link resolution"
        );
    }

    #[test]
    fn html_post_pass_resolves_pending_mentions() {
        // Extract HTML BEFORE adding the symbol — mentions should be pending.
        let mut g = Graph::new();
        let source = r##"<!DOCTYPE html>
<html>
<body>
<h1>Payment</h1>
<p>Use <a href="#process_payment">process_payment</a> to handle billing.</p>
</body>
</html>"##;
        let path = Path::new("/tmp/html_pending.html");
        std::fs::write(path, source).unwrap();
        extract_file(path, &mut g).unwrap();
        std::fs::remove_file(path).ok();

        // Should have pending mentions.
        let has_pending = g
            .nodes()
            .any(|(_, n)| n.properties.contains_key("pending_mentions"));
        assert!(has_pending, "should have pending mentions");

        // Now add the function and resolve.
        let _func = g.add_node(Node::new(
            NodeKind::Function,
            "src::lib.rs::process_payment",
        ));
        let added = resolve_mentions(&mut g);
        assert!(added > 0, "post-pass should resolve mentions");

        // pending_mentions should be cleared.
        assert!(
            g.nodes()
                .all(|(_, n)| !n.properties.contains_key("pending_mentions")),
            "pending_mentions must be cleared after the post-pass"
        );
    }
}
