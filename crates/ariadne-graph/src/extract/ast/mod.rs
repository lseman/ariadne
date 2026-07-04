//! Pass 1: deterministic AST extraction via tree-sitter.

pub mod custom_lang;
pub mod language_registry;
pub mod languages;
pub mod tsconfig_resolver;

/// Extract source text lines from full source bytes, given 1-indexed
/// start/end line numbers. Returns truncated text (max 10KB).
pub fn extract_source_text(source: &str, line_start: u32, line_end: u32) -> Option<String> {
    if line_start == 0 || line_end == 0 || line_end < line_start {
        return None;
    }
    let lines: Vec<&str> = source.lines().collect();
    // tree-sitter rows are 1-indexed.
    let s = ((line_start as usize).saturating_sub(1)).min(lines.len());
    let e = ((line_end as usize).saturating_sub(1)).min(lines.len());
    if s >= lines.len() || s >= e {
        return None;
    }
    let text: String = lines[s..e].join("\n");
    if text.is_empty() {
        return None;
    }
    if text.len() > 10_000 {
        Some(text[..10_000].to_string())
    } else {
        Some(text)
    }
}
