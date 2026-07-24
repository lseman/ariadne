//! Shared normalization and tokenization for document extractors.

pub(super) fn strip_file_suffix(s: &str) -> &str {
    const SUFFIXES: &[&str] = &[
        ".html", ".md", ".txt", ".htm", ".php", ".js", ".ts", ".rs", ".py",
    ];
    SUFFIXES
        .iter()
        .find_map(|suffix| s.strip_suffix(suffix))
        .unwrap_or(s)
}

pub(super) fn tokenize_code(code: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in code.chars() {
        if c.is_alphanumeric() || "_:.=+-".contains(c) {
            current.push(c);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub(super) fn normalize_for_match(s: &str) -> String {
    let chars: Vec<_> = s.chars().collect();
    let mut result = String::with_capacity(s.len() * 2);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            if prev.is_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_uppercase() && next.is_some_and(char::is_lowercase))
            {
                result.push('_');
            }
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

pub(super) fn slugify(s: &str) -> String {
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

    #[test]
    fn shared_document_normalization() {
        assert_eq!(normalize_for_match("HTTPParser"), "http_parser");
        assert_eq!(slugify(" Hello,  World! "), "hello-world");
        assert_eq!(strip_file_suffix("guide.md"), "guide");
        assert_eq!(tokenize_code("foo(bar_baz)"), ["foo", "bar_baz"]);
    }
}
