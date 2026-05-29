//! Heuristics for recognising test code.
//!
//! Two complementary signals are exposed:
//!
//! - [`is_test_file_path`] — does the file live in a test directory or
//!   follow a test-naming convention?
//! - [`is_test_name`] — is the function/method name itself test-shaped?
//!
//! Language-specific extractors layer their own signals on top (e.g.
//! `#[test]` attributes in Rust). The shared name/path patterns here are
//! deliberately conservative: anything they flag has a strong external
//! convention pointing at "this is a test".

use std::path::Path;

/// Test the *file* (or path component thereof). True if the file lives in
/// a test directory or has a test file-name convention.
pub fn is_test_file_path(path: &Path) -> bool {
    let s = path.to_string_lossy().replace('\\', "/");

    // Directory components — Rust `tests/` (integration), generic `test/`,
    // Python `tests/`, Go convention `_test.go`, JS `__tests__`, etc.
    if s.contains("/tests/")
        || s.contains("/test/")
        || s.starts_with("tests/")
        || s.starts_with("test/")
        || s.contains("/__tests__/")
        || s.starts_with("__tests__/")
        || s.contains("/spec/")
        || s.starts_with("spec/")
    {
        return true;
    }

    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    // File-name conventions across languages.
    name.starts_with("test_")               // Python: test_foo.py
        || stem.ends_with("_test")          // Go/Rust: foo_test.go, foo_test.rs
        || stem.ends_with("_spec")          // Ruby/RSpec convention
        || stem.ends_with(".test")          // JS/TS: foo.test.ts
        || stem.ends_with(".spec") // JS/TS: foo.spec.ts
}

/// Test the *symbol name*. True if the function/method name follows a
/// test convention.
///
/// We intentionally don't match bare `Test` (without a following letter)
/// because that's a common type name; the requirement of `Test[A-Z]`
/// avoids false positives like `Testimony`.
pub fn is_test_name(name: &str) -> bool {
    if name.starts_with("test_") || name.starts_with("Test_") {
        return true;
    }
    if name.ends_with("_test") || name.ends_with("_spec") {
        return true;
    }
    // `XxxTest` — but only if there's something *before* `Test`, so we
    // don't catch the bare type name `Test`.
    if name.len() > 4 && name.ends_with("Test") {
        return true;
    }
    // Java/JUnit style: `should*`, `it_*`, `it*` (when followed by a
    // capital), `given*` are common BDD/specification names. We restrict
    // these to camel-case continuations so we don't catch arbitrary verbs.
    let starts_camel = |prefix: &str| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.chars().next())
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false)
    };
    if starts_camel("should") || starts_camel("it") || starts_camel("given") {
        return true;
    }
    // Pattern `Test[A-Z]…`: TestLogin, TestAuthRefresh, etc.
    if name
        .strip_prefix("Test")
        .and_then(|rest| rest.chars().next())
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
    {
        return true;
    }
    false
}

/// Composite check used by extractors. True if either signal fires.
pub fn looks_like_test(path: &Path, name: &str) -> bool {
    is_test_file_path(path) || is_test_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_python_test_files() {
        assert!(is_test_file_path(&PathBuf::from("tests/test_auth.py")));
        assert!(is_test_file_path(&PathBuf::from("project/tests/foo.py")));
        assert!(is_test_file_path(&PathBuf::from("src/test_auth.py")));
        assert!(!is_test_file_path(&PathBuf::from("src/auth.py")));
    }

    #[test]
    fn detects_rust_and_go_test_files() {
        assert!(is_test_file_path(&PathBuf::from(
            "crates/foo/tests/integration.rs"
        )));
        assert!(is_test_file_path(&PathBuf::from("pkg/foo_test.go")));
        assert!(is_test_file_path(&PathBuf::from("src/auth_test.rs")));
        assert!(!is_test_file_path(&PathBuf::from("src/auth.rs")));
    }

    #[test]
    fn detects_js_test_files() {
        assert!(is_test_file_path(&PathBuf::from("src/auth.test.ts")));
        assert!(is_test_file_path(&PathBuf::from("src/auth.spec.js")));
        assert!(is_test_file_path(&PathBuf::from("__tests__/auth.js")));
    }

    #[test]
    fn detects_test_names() {
        assert!(is_test_name("test_login"));
        assert!(is_test_name("TestLogin"));
        assert!(is_test_name("login_test"));
        assert!(is_test_name("login_spec"));
        assert!(is_test_name("shouldRejectExpiredTokens"));
        assert!(is_test_name("itReturnsNullForMissingUser"));
        assert!(!is_test_name("login"));
        // Common false-positive risks we explicitly avoid:
        assert!(!is_test_name("Test")); // bare Test is often a type
        assert!(!is_test_name("Testimony")); // doesn't follow Test[A-Z]
        assert!(!is_test_name("should")); // bare verb
    }
}
