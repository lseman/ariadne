//! Central language registry — bundled defaults + user TOML overlay.
//!
//! Built-in language definitions live in `languages.toml` (embedded in the
//! binary via `include_str!`).  On first access the registry loads the
//! bundled defaults, then merges any user overlay found at
//! `.ariadne/languages.toml` relative to the current working directory.
//!
//! The TOML schema:
//! ```toml
//! [languages.rust]
//! grammar = "rust"
//! extensions = [".rs"]
//! function_node_types = ["function_item", "closure_expression"]
//! class_node_types = ["struct_item", "enum_item"]
//! import_node_types = ["use_declaration"]
//! call_node_types = ["call_expression"]
//! comment = "Rust"
//! ```

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Config schema
// ---------------------------------------------------------------------------

const CONFIG_RELATIVE_PATH: &str = ".ariadne/languages.toml";
const MAX_CUSTOM_LANGUAGES: usize = 20;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    languages: HashMap<String, LanguageEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct LanguageEntry {
    extensions: Option<Vec<String>>,
    grammar: Option<String>,
    #[serde(default)]
    function_node_types: Vec<String>,
    #[serde(default)]
    class_node_types: Vec<String>,
    #[serde(default)]
    import_node_types: Vec<String>,
    #[serde(default)]
    call_node_types: Vec<String>,
    #[serde(default)]
    comment: String,
}

// ---------------------------------------------------------------------------
// Language definition
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LanguageDef {
    pub name: String,
    pub grammar: String,
    pub extensions: Vec<String>,
    pub function_node_types: Vec<String>,
    pub class_node_types: Vec<String>,
    pub import_node_types: Vec<String>,
    pub call_node_types: Vec<String>,
    pub comment: String,
}

impl LanguageDef {
    /// Return true when `path` has an extension listed in this language.
    pub fn matches_ext(&self, path: &std::path::Path) -> bool {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_lowercase();
            self.extensions.iter().any(|e| {
                let e = e.trim_start_matches('.').to_lowercase();
                ext == e
            })
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in defaults — loaded from bundled TOML
// ---------------------------------------------------------------------------

/// The bundled `languages.toml` file that ships with the crate.
const BUNDLED_LANGUAGES_TOML: &str = include_str!("languages.toml");

/// Parse the bundled TOML into a HashMap of LanguageDef.
fn load_builtins() -> HashMap<String, LanguageDef> {
    let config: Config = toml::from_str(BUNDLED_LANGUAGES_TOML).unwrap_or_else(|err| {
        panic!("bundled languages.toml is invalid TOML: {err}");
    });
    config
        .languages
        .into_iter()
        .map(|(name, entry)| {
            let def = LanguageDef {
                name: name.clone(),
                grammar: entry.grammar.unwrap_or_else(|| name.clone()),
                extensions: entry.extensions.unwrap_or_default(),
                function_node_types: entry.function_node_types,
                class_node_types: entry.class_node_types,
                import_node_types: entry.import_node_types,
                call_node_types: entry.call_node_types,
                comment: entry.comment,
            };
            (name.to_lowercase(), def)
        })
        .collect()
}

/// Names of languages that come from the bundled defaults.
const BUILTIN_NAMES: &[&str] = &["rust", "python", "cpp", "typescript", "tsx", "javascript"];

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Thread-safe language registry, loaded once on first access.
use std::sync::OnceLock;

static REGISTRY: OnceLock<LanguageRegistry> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct LanguageRegistry {
    /// Name → definition.
    pub languages: HashMap<String, LanguageDef>,
}

impl LanguageRegistry {
    /// Get the global registry singleton.
    pub fn global() -> &'static Self {
        REGISTRY.get_or_init(LanguageRegistry::load)
    }

    /// Load registry from bundled defaults + user overlay.
    pub fn load() -> Self {
        let mut all = load_builtins();

        // Load and merge user TOML config
        let repo_root = std::env::current_dir().unwrap_or_else(|_| {
            let p = Path::new(".");
            p.to_path_buf()
        });
        let config_path = repo_root.join(CONFIG_RELATIVE_PATH);

        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                if let Ok(config) = toml::from_str::<Config>(&content) {
                    if config.languages.len() > MAX_CUSTOM_LANGUAGES {
                        tracing::warn!(
                            "config has {} entries, using top {}",
                            config.languages.len(),
                            MAX_CUSTOM_LANGUAGES,
                        );
                    }
                    for (name, entry) in config.languages.into_iter().take(MAX_CUSTOM_LANGUAGES) {
                        Self::merge_entry(&mut all, &name, entry);
                    }
                } else {
                    tracing::warn!("failed to parse {}", config_path.display());
                }
            }
        }

        Self { languages: all }
    }

    fn merge_entry(all: &mut HashMap<String, LanguageDef>, name: &str, entry: LanguageEntry) {
        let name_lower = name.to_lowercase();

        // Built-in names cannot be overridden (but can merge node types)
        let is_builtin = BUILTIN_NAMES.contains(&name_lower.as_str());

        if is_builtin {
            // Merge node types into existing built-in
            if let Some(def) = all.get_mut(&name_lower) {
                if !entry.function_node_types.is_empty() {
                    def.function_node_types = entry.function_node_types;
                }
                if !entry.class_node_types.is_empty() {
                    def.class_node_types = entry.class_node_types;
                }
                if !entry.import_node_types.is_empty() {
                    def.import_node_types = entry.import_node_types;
                }
                if !entry.call_node_types.is_empty() {
                    def.call_node_types = entry.call_node_types;
                }
                if let Some(exts) = entry.extensions {
                    def.extensions = exts;
                }
                if let Some(grammar) = entry.grammar {
                    def.grammar = grammar;
                }
            }
        } else {
            // New custom language
            if entry.grammar.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
                tracing::warn!("custom language '{}' has empty grammar, skipping", name);
                return;
            }
            if entry.extensions.is_none()
                || entry
                    .extensions
                    .as_ref()
                    .map(|e| e.is_empty())
                    .unwrap_or(true)
            {
                tracing::warn!("custom language '{}' has no extensions, skipping", name);
                return;
            }
            if entry.function_node_types.is_empty()
                && entry.class_node_types.is_empty()
                && entry.import_node_types.is_empty()
                && entry.call_node_types.is_empty()
            {
                tracing::warn!("custom language '{}' has no node types, skipping", name);
                return;
            }

            let def = LanguageDef {
                name: name_lower.clone(),
                grammar: entry.grammar.unwrap_or_default(),
                extensions: entry.extensions.unwrap_or_default(),
                function_node_types: entry.function_node_types,
                class_node_types: entry.class_node_types,
                import_node_types: entry.import_node_types,
                call_node_types: entry.call_node_types,
                comment: entry.comment,
            };
            all.insert(name_lower, def);
        }
    }

    /// Look up a language by name.
    pub fn get(&self, name: &str) -> Option<&LanguageDef> {
        self.languages
            .get(name)
            .or_else(|| self.languages.get(&name.to_lowercase()))
    }

    /// Look up a language by file path (matches extension).
    pub fn get_by_path(&self, path: &Path) -> Option<&LanguageDef> {
        self.languages.values().find(|l| l.matches_ext(path))
    }

    /// Get all language names.
    pub fn names(&self) -> Vec<String> {
        self.languages.keys().cloned().collect()
    }

    /// Get all definitions.
    pub fn all(&self) -> Vec<&LanguageDef> {
        self.languages.values().collect()
    }
}

/// Get the global registry.
pub fn registry() -> &'static LanguageRegistry {
    LanguageRegistry::global()
}

/// Look up a language by name.
pub fn get_language(name: &str) -> Option<&'static LanguageDef> {
    registry().get(name)
}

/// Look up a language by file path.
pub fn get_language_by_path(path: &Path) -> Option<&'static LanguageDef> {
    registry().get_by_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_toml_parses_cleanly() {
        // Verify the bundled TOML is valid and contains expected languages.
        let defs = load_builtins();
        assert!(defs.contains_key("rust"));
        assert!(defs.contains_key("python"));
        assert!(defs.contains_key("cpp"));
        assert!(defs.contains_key("typescript"));
        assert!(defs.contains_key("tsx"));
        assert!(defs.contains_key("javascript"));

        // Rust must have extensions and node types.
        let rust = defs.get("rust").unwrap();
        assert_eq!(rust.extensions, vec![".rs"]);
        assert!(rust
            .function_node_types
            .contains(&"function_item".to_string()));
        assert!(rust.class_node_types.contains(&"struct_item".to_string()));
    }

    #[test]
    fn matches_ext_case_insensitive() {
        let defs = load_builtins();
        let rust = defs.get("rust").unwrap();
        assert!(rust.matches_ext(Path::new("src/lib.RS")));
        assert!(rust.matches_ext(Path::new("src/lib.rs")));
        assert!(!rust.matches_ext(Path::new("src/main.py")));
    }

    #[test]
    fn tsx_matches_jsx() {
        let defs = load_builtins();
        let tsx = defs.get("tsx").unwrap();
        assert!(tsx.matches_ext(Path::new("App.tsx")));
        assert!(tsx.matches_ext(Path::new("View.jsx")));
    }

    #[test]
    fn cpp_matches_all_cxx_extensions() {
        let defs = load_builtins();
        let cpp = defs.get("cpp").unwrap();
        for ext in &[".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".hxx"] {
            assert!(
                cpp.matches_ext(Path::new(&format!("f{ext}"))),
                "missing {}",
                ext
            );
        }
    }
}
