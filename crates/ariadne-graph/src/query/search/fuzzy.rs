//! Pure string-similarity primitives used to rank fuzzy search candidates.
//!
//! No dependency on `Graph` or `Store` — these operate purely on strings and
//! are independently testable.

pub(super) fn normalize_identifier(s: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        if c.is_alphanumeric() {
            if let Some(p) = prev {
                let camel_boundary = p.is_lowercase() && c.is_uppercase();
                let acronym_boundary =
                    p.is_uppercase() && c.is_uppercase() && next.is_some_and(|n| n.is_lowercase());
                let digit_boundary = p.is_alphabetic() != c.is_alphabetic();
                if camel_boundary || acronym_boundary || digit_boundary {
                    out.push(' ');
                }
            }
            out.extend(c.to_lowercase());
            prev = Some(c);
        } else {
            out.push(' ');
            prev = None;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn fuzzy_score(query: &str, candidate: &str) -> f32 {
    if query.is_empty() || candidate.is_empty() {
        return 0.0;
    }
    let compact_query = compact(query);
    let compact_candidate = compact(candidate);
    [
        ratio(query, candidate),
        ratio(&compact_query, &compact_candidate),
        partial_ratio(&compact_query, &compact_candidate),
        token_sort_ratio(query, candidate),
        token_set_ratio(query, candidate),
        acronym_ratio(query, candidate),
        subsequence_ratio(&compact_query, &compact_candidate),
    ]
    .into_iter()
    .fold(0.0, f32::max)
}

fn ratio(a: &str, b: &str) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let distance = levenshtein(a, b) as f32;
    1.0 - distance / a.chars().count().max(b.chars().count()) as f32
}

fn ratio_bytes(a: &[u8], b: &[u8]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let distance = levenshtein_bytes(a, b) as f32;
    1.0 - distance / a.len().max(b.len()) as f32
}

pub(super) fn partial_ratio(shorter: &str, longer: &str) -> f32 {
    if shorter.is_empty() || longer.is_empty() {
        return 0.0;
    }
    if shorter.is_ascii() && longer.is_ascii() {
        return partial_ratio_bytes(shorter.as_bytes(), longer.as_bytes());
    }
    let (needle, haystack) = if shorter.chars().count() <= longer.chars().count() {
        (shorter, longer)
    } else {
        (longer, shorter)
    };
    let needle_len = needle.chars().count();
    let hay_chars: Vec<char> = haystack.chars().collect();
    if needle_len >= hay_chars.len() {
        return ratio(needle, haystack);
    }
    let mut best: f32 = 0.0;
    for start in 0..=hay_chars.len() - needle_len {
        let window: String = hay_chars[start..start + needle_len].iter().collect();
        best = best.max(ratio(needle, &window));
        if best >= 1.0 {
            break;
        }
    }
    best
}

fn partial_ratio_bytes(shorter: &[u8], longer: &[u8]) -> f32 {
    let (needle, haystack) = if shorter.len() <= longer.len() {
        (shorter, longer)
    } else {
        (longer, shorter)
    };
    if needle.len() >= haystack.len() {
        return ratio_bytes(needle, haystack);
    }
    let mut best: f32 = 0.0;
    for window in haystack.windows(needle.len()) {
        best = best.max(ratio_bytes(needle, window));
        if best >= 1.0 {
            break;
        }
    }
    best
}

fn token_sort_ratio(a: &str, b: &str) -> f32 {
    ratio(&sorted_tokens(a).join(" "), &sorted_tokens(b).join(" "))
}

fn token_set_ratio(a: &str, b: &str) -> f32 {
    let mut a_tokens = sorted_tokens(a);
    let mut b_tokens = sorted_tokens(b);
    a_tokens.dedup();
    b_tokens.dedup();
    let common: Vec<&str> = a_tokens
        .iter()
        .copied()
        .filter(|t| b_tokens.contains(t))
        .collect();
    if common.is_empty() {
        return 0.0;
    }
    let common_text = common.join(" ");
    ratio(&common_text, a).max(ratio(&common_text, b))
}

fn acronym_ratio(query: &str, candidate: &str) -> f32 {
    let acronym: String = candidate
        .split_whitespace()
        .filter_map(|token| token.chars().next())
        .collect();
    ratio(&compact(query), &acronym)
}

fn subsequence_ratio(query: &str, candidate: &str) -> f32 {
    let mut qchars = query.chars();
    let mut current = qchars.next();
    let mut matched = 0usize;
    for c in candidate.chars() {
        if Some(c) == current {
            matched += 1;
            current = qchars.next();
            if current.is_none() {
                break;
            }
        }
    }
    if current.is_none() {
        matched as f32 / candidate.chars().count().max(1) as f32
    } else {
        0.0
    }
}

fn sorted_tokens(s: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    tokens.sort_unstable();
    tokens
}

pub(super) fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

pub(super) fn levenshtein(a: &str, b: &str) -> usize {
    if a.is_ascii() && b.is_ascii() {
        return levenshtein_bytes(a.as_bytes(), b.as_bytes());
    }
    levenshtein_chars(a, b)
}

fn levenshtein_chars(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0; b_chars.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}

fn levenshtein_bytes(a: &[u8], b: &[u8]) -> usize {
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut a = a;
    let mut b = b;
    let prefix_len = a
        .iter()
        .zip(b.iter())
        .take_while(|(ca, cb)| ca == cb)
        .count();
    a = &a[prefix_len..];
    b = &b[prefix_len..];

    let suffix_len = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(ca, cb)| ca == cb)
        .count();
    if suffix_len > 0 {
        a = &a[..a.len() - suffix_len];
        b = &b[..b.len() - suffix_len];
    }

    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    if a.len() > b.len() {
        std::mem::swap(&mut a, &mut b);
    }
    if a.len() <= usize::BITS as usize {
        return levenshtein_myers(a, b);
    }
    levenshtein_dp_bytes(a, b)
}

fn levenshtein_myers(pattern: &[u8], text: &[u8]) -> usize {
    debug_assert!(!pattern.is_empty());
    debug_assert!(pattern.len() <= usize::BITS as usize);

    let mut peq = [0usize; 256];
    for (i, &byte) in pattern.iter().enumerate() {
        peq[byte as usize] |= 1usize << i;
    }

    let last = 1usize << (pattern.len() - 1);
    let mut pv = !0usize;
    let mut mv = 0usize;
    let mut score = pattern.len();

    for &byte in text {
        let eq = peq[byte as usize];
        let xv = eq | mv;
        let xh = (((eq & pv).wrapping_add(pv)) ^ pv) | eq;
        let ph = mv | !(xh | pv);
        let mh = pv & xh;

        if (ph & last) != 0 {
            score += 1;
        }
        if (mh & last) != 0 {
            score -= 1;
        }

        let ph = (ph << 1) | 1;
        let mh = mh << 1;
        pv = mh | !(xv | ph);
        mv = ph & xv;
    }

    score
}

fn levenshtein_dp_bytes(a: &[u8], b: &[u8]) -> usize {
    let mut row: Vec<usize> = (0..=a.len()).collect();
    for (i, &bb) in b.iter().enumerate() {
        let mut prev_diag = row[0];
        row[0] = i + 1;
        for (j, &aa) in a.iter().enumerate() {
            let old = row[j + 1];
            let insert = row[j] + 1;
            let delete = old + 1;
            let replace = prev_diag + usize::from(aa != bb);
            row[j + 1] = insert.min(delete).min(replace);
            prev_diag = old;
        }
    }
    row[a.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_normalization_preserves_unicode_identifiers() {
        assert_eq!(
            normalize_identifier("CaféParser Δelta42"),
            "café parser δelta 42"
        );
    }

    #[test]
    fn levenshtein_fast_path_matches_known_distances() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("extract_directory", "extract_dirctory"), 1);
        assert_eq!(levenshtein("HTTPRequestParser", "HTTPParser"), 7);
        assert_eq!(
            levenshtein(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2",
            ),
            1
        );
        assert_eq!(levenshtein("cafe", "café"), 1);
    }

    #[test]
    fn partial_ratio_scores_ascii_windows_without_allocating_strings() {
        assert_eq!(
            partial_ratio("rankedsearch", "prefixrankedsearchsuffix"),
            1.0
        );
        assert!(partial_ratio("rnkedserch", "rankedsearch") >= 0.8);
    }
}
