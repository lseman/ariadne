pub(super) fn default_ignored_name(name: &str) -> bool {
    (name.starts_with('.') && name.len() > 1)
        || matches!(name, "target" | "node_modules" | "__pycache__")
}
