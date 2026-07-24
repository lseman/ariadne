//! SQLite schema owned by the persistence adapter.

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS nodes (
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
    qualified_name TEXT NOT NULL UNIQUE, source_uri TEXT, line_start INTEGER,
    line_end INTEGER, properties TEXT NOT NULL DEFAULT '{}', valid_from TEXT,
    valid_to TEXT, source_text TEXT
);
CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);
CREATE INDEX IF NOT EXISTS idx_nodes_qname ON nodes(qualified_name);
CREATE INDEX IF NOT EXISTS idx_nodes_source ON nodes(source_uri);
CREATE INDEX IF NOT EXISTS idx_nodes_valid ON nodes(valid_from, valid_to);
CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY, src_id INTEGER NOT NULL REFERENCES nodes(id),
    dst_id INTEGER NOT NULL REFERENCES nodes(id), kind TEXT NOT NULL,
    confidence REAL NOT NULL, conf_class TEXT NOT NULL,
    properties TEXT NOT NULL DEFAULT '{}', valid_from TEXT, valid_to TEXT
);
CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src_id);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst_id);
CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
CREATE INDEX IF NOT EXISTS idx_edges_valid ON edges(valid_from, valid_to);
CREATE TABLE IF NOT EXISTS embeddings (
    node_id INTEGER PRIMARY KEY REFERENCES nodes(id), model TEXT NOT NULL, vector BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS node_versions (
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
    qualified_name TEXT NOT NULL, source_uri TEXT, line_start INTEGER,
    line_end INTEGER, properties TEXT NOT NULL DEFAULT '{}', valid_from TEXT,
    valid_to TEXT, source_text TEXT
);
CREATE INDEX IF NOT EXISTS idx_node_versions_qname ON node_versions(qualified_name);
CREATE INDEX IF NOT EXISTS idx_node_versions_source ON node_versions(source_uri);
CREATE INDEX IF NOT EXISTS idx_node_versions_valid ON node_versions(valid_from, valid_to);
CREATE TABLE IF NOT EXISTS edge_versions (
    id INTEGER PRIMARY KEY, src_qname TEXT NOT NULL, dst_qname TEXT NOT NULL,
    kind TEXT NOT NULL, confidence REAL NOT NULL, conf_class TEXT NOT NULL,
    properties TEXT NOT NULL DEFAULT '{}', source_uri TEXT, valid_from TEXT, valid_to TEXT
);
CREATE INDEX IF NOT EXISTS idx_edge_versions_src ON edge_versions(src_qname);
CREATE INDEX IF NOT EXISTS idx_edge_versions_dst ON edge_versions(dst_qname);
CREATE INDEX IF NOT EXISTS idx_edge_versions_kind ON edge_versions(kind);
CREATE INDEX IF NOT EXISTS idx_edge_versions_valid ON edge_versions(valid_from, valid_to);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS file_state (
    path TEXT PRIMARY KEY, hash TEXT NOT NULL, indexed_at_unix INTEGER NOT NULL
);
INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '2');
CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    kind, name, qualified_name,
    tokenize = "unicode61 separators '_' remove_diacritics 1"
);
"#;
