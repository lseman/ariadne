use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Collect file hashes for a directory.
pub fn collect_file_hashes(root: &Path) -> Result<Vec<(String, String)>> {
    let mut hashes = Vec::new();
    let ignore = ariadne_graph::extract::ignore_set(root);
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !ignore.is_ignored(e.path()))
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && ariadne_graph::extract::is_supported(path) {
            hashes.push((path.to_string_lossy().to_string(), hash_file(path)?));
        }
    }
    hashes.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(hashes)
}

/// Hash a single file.
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Get absolute path.
pub fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// Get daemon config path.
pub fn daemon_config_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()?
        .join(".ariadne")
        .join("daemon.json"))
}

/// Load daemon repos from config.
pub fn load_daemon_repos() -> Result<Vec<Value>> {
    let path = daemon_config_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(path)?;
    Ok(serde_json::from_str::<Value>(&data)?
        .get("repos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Save daemon repos to config.
pub fn save_daemon_repos(repos: &[Value]) -> Result<()> {
    let path = daemon_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({ "repos": repos }))?,
    )?;
    Ok(())
}

/// Git commit hash.
pub fn git_commit_hash(rev: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", rev])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// Check if ancestor is ancestor of descendant.
pub fn git_is_ancestor(ancestor: &str, descendant: &str) -> bool {
    if ancestor == descendant {
        return true;
    }
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Get git changed diff.
pub fn git_changed_diff(base: &str) -> Result<Vec<ChangedFile>> {
    let output = Command::new("git")
        .args(["diff", "--unified=0", "--no-ext-diff", base, "--"])
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(parse_git_diff_hunks(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Changed file from git diff.
#[derive(Debug, Clone, Default)]
pub struct ChangedFile {
    pub path: String,
    pub hunks: Vec<ChangedHunk>,
}

/// Changed hunk from git diff.
#[derive(Debug, Clone)]
pub struct ChangedHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
}

impl ChangedHunk {
    pub fn new_end(&self) -> u32 {
        if self.new_count == 0 {
            self.new_start
        } else {
            self.new_start + self.new_count.saturating_sub(1)
        }
    }

    pub fn overlaps_node(&self, line_start: u32, line_end: u32) -> bool {
        let hunk_start = self.new_start.max(1);
        let hunk_end = self.new_end().max(hunk_start);
        line_start <= hunk_end && line_end >= hunk_start
    }
}

/// Parse git diff hunks.
pub fn parse_git_diff_hunks(diff: &str) -> Vec<ChangedFile> {
    let mut files = Vec::<ChangedFile>::new();
    let mut current: Option<ChangedFile> = None;

    for line in diff.lines() {
        if let Some(path) = parse_diff_git_path(line) {
            if let Some(file) = current.take() {
                files.push(file);
            }
            current = Some(ChangedFile {
                path,
                hunks: Vec::new(),
            });
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(file) = current.as_mut() {
                if rest != "/dev/null" {
                    file.path = rest.strip_prefix("b/").unwrap_or(rest).to_string();
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("@@ ") {
            if let (Some(file), Some(hunk)) = (current.as_mut(), parse_hunk_header(rest)) {
                file.hunks.push(hunk);
            }
        }
    }

    if let Some(file) = current {
        files.push(file);
    }

    files
        .into_iter()
        .filter(|file| !file.path.is_empty())
        .collect()
}

/// Parse diff git path.
fn parse_diff_git_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git ")?;
    let mut parts = rest.split_whitespace();
    let _old = parts.next()?;
    let new = parts.next()?;
    Some(new.strip_prefix("b/").unwrap_or(new).to_string())
}

/// Parse hunk header.
fn parse_hunk_header(rest: &str) -> Option<ChangedHunk> {
    let end = rest.find(" @@")?;
    let header = &rest[..end];
    let mut parts = header.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let (old_start, old_count) = parse_hunk_range(old)?;
    let (new_start, new_count) = parse_hunk_range(new)?;
    Some(ChangedHunk {
        old_start,
        old_count,
        new_start,
        new_count,
    })
}

/// Parse hunk range.
fn parse_hunk_range(range: &str) -> Option<(u32, u32)> {
    let mut parts = range.splitn(2, ',');
    let start = parts.next()?.parse().ok()?;
    let count = parts.next().map(|s| s.parse().ok()).unwrap_or(Some(1))?;
    Some((start, count))
}

/// Trait for changed ranges JSON.
#[allow(dead_code)]
pub trait ChangedRangesJson {
    fn changed_ranges_json(&self) -> Vec<serde_json::Value>;
}

impl ChangedRangesJson for Vec<ChangedFile> {
    fn changed_ranges_json(&self) -> Vec<serde_json::Value> {
        // This is implemented in response.rs where we have access to the graph
        unimplemented!("Use response::changed_ranges_json instead")
    }
}

/// Check if a path is source-like.
#[allow(dead_code)]
pub fn is_source_like_path(path: &str) -> bool {
    let ext = path.split('.').next_back().unwrap_or("");
    matches!(
        ext,
        "rs" | "py"
            | "js"
            | "ts"
            | "go"
            | "java"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "cs"
            | "rb"
            | "php"
            | "swift"
            | "kt"
            | "scala"
    )
}

/// Check if a path is doc-like.
#[allow(dead_code)]
pub fn is_doc_like_path(path: &str) -> bool {
    path.ends_with(".md")
        || path.ends_with(".rst")
        || path.ends_with(".txt")
        || path.ends_with(".adoc")
}

/// Check if a path is test-like.
#[allow(dead_code)]
pub fn is_test_like_path(path: &str) -> bool {
    path.contains("/test/")
        || path.contains("/tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("_test.py")
        || path.ends_with("_test.ts")
        || path.ends_with("_test.js")
        || path.ends_with(".test.js")
        || path.ends_with(".test.ts")
        || path.ends_with(".spec.js")
        || path.ends_with(".spec.ts")
}

/// Check if a path is low-signal for review.
#[allow(dead_code)]
pub fn is_low_signal_review_path(path: &str) -> bool {
    path.ends_with(".gen.ts")
        || path.ends_with(".d.ts")
        || path.ends_with("_generated.go")
        || path.ends_with(".pb.go")
        || path.ends_with(".pb.ts")
        || path.ends_with(".pb.js")
        || path.ends_with(".min.js")
        || path.ends_with(".min.css")
}

/// XML escape.
#[allow(dead_code)]
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Cypher string escape.
#[allow(dead_code)]
pub fn cypher_string(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Cypher optional string.
#[allow(dead_code)]
pub fn cypher_opt_string(s: Option<&str>) -> String {
    match s {
        Some(s) => cypher_string(s),
        None => "null".to_string(),
    }
}

/// Cypher relationship type.
#[allow(dead_code)]
pub fn cypher_rel_type(t: &str) -> String {
    format!("[:{}]", t.replace(' ', "_"))
}

/// Slugify a string.
#[allow(dead_code)]
pub fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Git tracked files.
#[allow(dead_code)]
pub fn git_tracked_files(root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for file in String::from_utf8_lossy(&output.stdout).split('\0') {
        if !file.is_empty() {
            files.push(file.to_string());
        }
    }
    Ok(files)
}
