//! Static allowlist of call names to suppress when resolving `call::`
//! placeholder edges — builtins, stdlib, and tree-sitter API methods that
//! otherwise dominate the unresolved-call noise floor.

pub fn should_suppress_call_placeholder(name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        // Python builtins and common constructors.
        "abs"
            | "all"
            | "any"
            | "bool"
            | "bytes"
            | "callable"
            | "dict"
            | "dir"
            | "enumerate"
            | "filter"
            | "float"
            | "getattr"
            | "hasattr"
            | "hash"
            | "id"
            | "int"
            | "isinstance"
            | "iter"
            | "len"
            | "list"
            | "map"
            | "max"
            | "min"
            | "next"
            | "open"
            | "print"
            | "range"
            | "repr"
            | "reversed"
            | "round"
            | "set"
            | "sorted"
            | "str"
            | "sum"
            | "super"
            | "tuple"
            | "type"
            | "vars"
            | "zip"
            // Rust/std/common fluent API calls that otherwise dominate
            // unresolved call nodes.
            | "and_then"
            | "as_bytes"
            | "as_deref"
            | "as_ref"
            | "as_str"
            | "chars"
            | "clone"
            | "cloned"
            | "clamp"
            | "collect"
            | "contains"
            | "copied"
            | "count"
            | "default"
            | "ends_with"
            | "entry"
            | "err"
            | "expect"
            | "extend"
            | "filter_map"
            | "find"
            | "first"
            | "flat_map"
            | "fold"
            | "from"
            | "get"
            | "get_mut"
            | "index"
            | "insert"
            | "into"
            | "into_iter"
            | "is_empty"
            | "is_none"
            | "is_some_and"
            | "iter_mut"
            | "join"
            | "last"
            | "lines"
            | "map_err"
            | "new"
            | "none"
            | "ok"
            | "ok_or"
            | "ok_or_else"
            | "or_default"
            | "position"
            | "push"
            | "push_str"
            | "rsplit"
            | "some"
            | "split"
            | "splitn"
            | "starts_with"
            | "take"
            | "to_owned"
            | "to_string"
            | "to_string_lossy"
            | "from_str"
            | "trim"
            | "unwrap"
            | "unwrap_or"
            | "unwrap_or_default"
            | "unwrap_or_else"
            | "with_capacity"
            // std::collections / std::vec methods
            | "sort_by"
            | "sort_by_key"
            | "sort_unstable"
            | "truncate"
            | "reserve"
            | "clear"
            | "contains_key"
            | "values"
            | "concat"
            | "to_vec"
            // std::io / std::fs methods
            | "read"
            | "read_to_string"
            | "read_to_end"
            | "remove_dir_all"
            | "remove_file"
            | "create_dir_all"
            | "exists"
            | "write"
            | "write_all"
            | "flush"
            // std trait methods that create noise
            | "load"
            | "display"
            | "execute"
            | "fg"
            // std::env
            | "temp_dir"
            | "args"
            // std::string methods
            | "strip_prefix"
            | "to_ascii_lowercase"
            | "trim_matches"
            // std::option
            | "is_some"
            | "or_else"
            // std::collections
            | "values_mut"
            | "borrow"
            // std::collections
            | "has"
            | "pop_front"
            | "push_back"
            | "remove"
            | "to_lowercase"
            | "to_uppercase"
            | "split_whitespace"
            | "saturating_sub"
            | "replace"
            | "to_string_pretty"
            | "as_array"
            | "as_u64"
            | "add"
            | "string"
            // std::path
            | "path"
            | "file_name"
            | "file_stem"
            // std::num
            | "wrapping_add"
            // std::fs
            | "current_dir"
            // serde_json
            | "as_bool"
            | "as_f64"
            | "as_object"
            // tui-rs / external lib
            | "render_widget"
            | "highlight_style"
            | "attr"
            | "block"
            | "border_style"
            | "borders"
            | "checkAvailable"
            | "percentage"
            | "strip_suffix"
            | "trim_end_matches"
            | "pop"
            | "chunks"
            | "or_insert"
            | "or_insert_with"
            // SQLite rusqlite bindings
            | "query_map"
            | "prepare"
            | "commit"
            | "transaction"
            | "select"
            | "selected"
            | "query_row"
            | "add_modifier"
            // std::time methods
            | "duration_since"
            | "now"
            | "as_nanos"
            // std::num
            | "saturating_add"
            | "wrapping_mul"
            // std::path
            | "extension"
            // Confidence enum variant leaking as unresolved
            | "inferred"
            // Common graph-library traversal/mutation helpers. Keeping
            // these out of the code graph prevents external petgraph calls
            // from masquerading as unresolved project calls.
            | "contains_node"
            | "edge_indices"
            | "edge_references"
            | "edge_weight_mut"
            | "edges_directed"
            | "node_indices"
            | "node_weight"
            | "node_weight_mut"
            // std::fs::DirEntry / std::path methods
            | "status"
            | "watch"
            | "to_path_buf"
            | "is_dir"
            | "is_file"
            | "filter_entry"
            // C/C++ and libc-style calls.
            | "malloc"
            | "free"
            | "printf"
            | "fprintf"
            | "memcpy"
            | "memset"
            | "strlen"
            | "strcmp"
            | "std"
            // tree-sitter Node API — the AST extractors walk these methods
            // and emit call placeholders; they're not project functions.
            | "child_by_field_name"
            | "children"
            | "end_position"
            | "is_named"
            | "kind"
            | "language"
            | "language_typescript"
            | "language_tsx"
            | "parent"
            | "root_node"
            | "node"
            | "start_position"
            | "text"
            | "walk"
            | "utf8_text"
            // tree-sitter Parser API
            | "parse"
            | "set_language"
            | "included_ranges"
            // Additional tree-sitter / std methods that leak as unresolved
            | "rev"
            | "nth"
            | "to_str"
            | "last_mut"
            | "trim_start"
            | "from_utf8"
            | "windows"
            | "end_byte"
            | "start_byte"
            | "is_ascii_digit"
            | "is_lowercase"
            | "is_uppercase"
            | "is_alphanumeric"
            | "new_ext"
            | "reverse"
            | "next_back"
            | "as_array_mut"
            // Ariadne internal methods — not extractable as project functions
            | "resolve_mentions"
            | "original_nodes"
            | "edges_mut"
            | "qname_index"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_suppress_call_placeholder_works() {
        // Ariadne internal methods
        // Ariadne Graph API are NOT suppressed — they're real project functions
        // that the placeholder resolver should resolve.
        assert!(!should_suppress_call_placeholder("add_node"));
        assert!(!should_suppress_call_placeholder("add_edge"));
        assert!(!should_suppress_call_placeholder("nodes"));
        assert!(!should_suppress_call_placeholder("edges"));
        assert!(!should_suppress_call_placeholder("out_neighbors"));
        assert!(!should_suppress_call_placeholder("in_neighbors"));
        assert!(!should_suppress_call_placeholder("find_by_qname"));
        // Rust std methods
        assert!(should_suppress_call_placeholder("String"));
        assert!(should_suppress_call_placeholder("rev"));
        assert!(should_suppress_call_placeholder("nth"));
        assert!(should_suppress_call_placeholder("windows"));
        // Real project functions should NOT be suppressed
        assert!(!should_suppress_call_placeholder("login"));
        assert!(!should_suppress_call_placeholder("extract_file"));
    }
}
