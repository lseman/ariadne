//! Central language registry — built-in languages + TOML config + custom.
//!
//! Each language definition provides tree-sitter node type sets and optional
//! query templates. The registry merges built-in defaults with `.ariadne/
//! languages.toml` overrides and custom language entries.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Config schema (same as custom_lang.rs)
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
// Built-in language definitions (extracted from existing extractors)
// ---------------------------------------------------------------------------

const BUILT_IN_NAMES: &[&str] = &[
    "rust", "python", "cpp", "typescript", "tsx", "javascript",
];

fn default_rust() -> LanguageDef {
    LanguageDef {
        name: "rust".into(),
        grammar: "rust".into(),
        extensions: vec![".rs".into()],
        function_node_types: vec![
            "function_item".into(),
            "function_type".into(),
            "closure_expression".into(),
        ],
        class_node_types: vec![
            "struct_item".into(),
            "enum_item".into(),
            "union_item".into(),
            "trait_item".into(),
            "impl_block".into(),
        ],
        import_node_types: vec!["use_declaration".into()],
        call_node_types: vec!["call_expression".into()],
        comment: "Rust".into(),
    }
}

fn default_python() -> LanguageDef {
    LanguageDef {
        name: "python".into(),
        grammar: "python".into(),
        extensions: vec![".py".into()],
        function_node_types: vec![
            "function_definition".into(),
            "async_function_definition".into(),
        ],
        class_node_types: vec!["class_definition".into()],
        import_node_types: vec![
            "import_statement".into(),
            "import_from_statement".into(),
        ],
        call_node_types: vec!["call".into()],
        comment: "Python".into(),
    }
}

fn default_cpp() -> LanguageDef {
    LanguageDef {
        name: "cpp".into(),
        grammar: "cpp".into(),
        extensions: vec![
            ".c".into(), ".cc".into(), ".cpp".into(), ".cxx".into(),
            ".h".into(), ".hh".into(), ".hpp".into(), ".hxx".into(),
        ],
        function_node_types: vec![
            "function_definition".into(),
            "destructor".into(),
        ],
        class_node_types: vec![
            "class_specifier".into(),
            "struct_specifier".into(),
            "enum_specifier".into(),
            "enum_declaration".into(),
        ],
        import_node_types: vec!["preproc_include".into()],
        call_node_types: vec!["call_expression".into()],
        comment: "C/C++".into(),
    }
}

fn default_typescript() -> LanguageDef {
    LanguageDef {
        name: "typescript".into(),
        grammar: "typescript".into(),
        extensions: vec![".ts".into(), ".tsx".into()],
        function_node_types: vec![
            "function_declaration".into(),
            "function".into(),
            "arrow_function".into(),
            "method_definition".into(),
            "generator_function_declaration".into(),
            "generator_function".into(),
        ],
        class_node_types: vec![
            "class_declaration".into(),
            "class".into(),
            "interface_declaration".into(),
            "type_alias_declaration".into(),
            "enum_declaration".into(),
            "enum".into(),
        ],
        import_node_types: vec![
            "import_statement".into(),
            "export_specifier".into(),
            "import_clause".into(),
        ],
        call_node_types: vec!["call_expression".into(), "new_expression".into()],
        comment: "TypeScript/TSX".into(),
    }
}

fn default_tsx() -> LanguageDef {
    LanguageDef {
        name: "tsx".into(),
        grammar: "tsx".into(),
        extensions: vec![".tsx".into(), ".jsx".into()],
        function_node_types: vec![
            "function_declaration".into(),
            "function".into(),
            "arrow_function".into(),
            "method_definition".into(),
            "jsx_element".into(),
        ],
        class_node_types: vec![
            "class_declaration".into(),
            "class".into(),
            "interface_declaration".into(),
            "type_alias_declaration".into(),
            "enum_declaration".into(),
            "enum".into(),
        ],
        import_node_types: vec![
            "import_statement".into(),
            "export_specifier".into(),
            "import_clause".into(),
        ],
        call_node_types: vec!["call_expression".into(), "new_expression".into()],
        comment: "TSX/JSX".into(),
    }
}

fn default_javascript() -> LanguageDef {
    LanguageDef {
        name: "javascript".into(),
        grammar: "javascript".into(),
        extensions: vec![".js".into(), ".mjs".into(), ".cjs".into()],
        function_node_types: vec![
            "function_declaration".into(),
            "function".into(),
            "arrow_function".into(),
            "method_definition".into(),
            "generator_function_declaration".into(),
            "generator_function".into(),
        ],
        class_node_types: vec![
            "class_declaration".into(),
            "class".into(),
        ],
        import_node_types: vec![
            "import_statement".into(),
            "export_specifier".into(),
            "import_clause".into(),
        ],
        call_node_types: vec!["call_expression".into(), "new_expression".into()],
        comment: "JavaScript".into(),
    }
}

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

    /// Load registry from built-ins + TOML config.
    pub fn load() -> Self {
        let mut all: HashMap<String, LanguageDef> = HashMap::new();

        // Register built-in languages
        all.insert("rust".into(), default_rust());
        all.insert("python".into(), default_python());
        all.insert("cpp".into(), default_cpp());
        all.insert("typescript".into(), default_typescript());
        all.insert("tsx".into(), default_tsx());
        all.insert("javascript".into(), default_javascript());

        // Load and merge TOML config
        let repo_root = std::env::current_dir().unwrap_or_else(|_| {
            let p = Path::new(".");
            p.to_path_buf()
        });
        let config_path = repo_root.join(CONFIG_RELATIVE_PATH);

        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                if let Ok(config) = toml::from_str::<Config>(&content) {
                    let _count = config.languages.len().min(MAX_CUSTOM_LANGUAGES);
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
        let is_builtin = BUILT_IN_NAMES.contains(&name_lower.as_str());

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
            if entry.extensions.is_none() || entry.extensions.as_ref().map(|e| e.is_empty()).unwrap_or(true) {
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
        self.languages.get(name)
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

/// Clear the registry cache (for testing).
pub fn clear_cache() {
    // Note: OnceLock doesn't support reset. For testing, use a different
    // approach or just let it stay loaded.
}
