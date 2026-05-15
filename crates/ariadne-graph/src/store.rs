//! SQLite-backed persistence for an Ariadne graph.
//!
//! The schema is intentionally tiny: `nodes`, `edges`, `embeddings`, `meta`.
//! Every node and edge row carries `valid_from` and `valid_to` SHA columns
//! so that temporal queries reduce to a `WHERE` clause and never require
//! a re-parse.

use crate::core::{Confidence, Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS nodes (
    id             INTEGER PRIMARY KEY,
    kind           TEXT NOT NULL,
    name           TEXT NOT NULL,
    qualified_name TEXT NOT NULL UNIQUE,
    source_uri     TEXT,
    line_start     INTEGER,
    line_end       INTEGER,
    properties     TEXT NOT NULL DEFAULT '{}',
    valid_from     TEXT,
    valid_to       TEXT
);
CREATE INDEX IF NOT EXISTS idx_nodes_kind  ON nodes(kind);
CREATE INDEX IF NOT EXISTS idx_nodes_qname ON nodes(qualified_name);
CREATE INDEX IF NOT EXISTS idx_nodes_source ON nodes(source_uri);
CREATE INDEX IF NOT EXISTS idx_nodes_valid ON nodes(valid_from, valid_to);

CREATE TABLE IF NOT EXISTS edges (
    id          INTEGER PRIMARY KEY,
    src_id      INTEGER NOT NULL REFERENCES nodes(id),
    dst_id      INTEGER NOT NULL REFERENCES nodes(id),
    kind        TEXT NOT NULL,
    confidence  REAL NOT NULL,
    conf_class  TEXT NOT NULL,
    properties  TEXT NOT NULL DEFAULT '{}',
    valid_from  TEXT,
    valid_to    TEXT
);
CREATE INDEX IF NOT EXISTS idx_edges_src   ON edges(src_id);
CREATE INDEX IF NOT EXISTS idx_edges_dst   ON edges(dst_id);
CREATE INDEX IF NOT EXISTS idx_edges_kind  ON edges(kind);
CREATE INDEX IF NOT EXISTS idx_edges_valid ON edges(valid_from, valid_to);

CREATE TABLE IF NOT EXISTS embeddings (
    node_id INTEGER PRIMARY KEY REFERENCES nodes(id),
    model   TEXT NOT NULL,
    vector  BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS file_state (
    path            TEXT PRIMARY KEY,
    hash            TEXT NOT NULL,
    indexed_at_unix INTEGER NOT NULL
);

INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1');

CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    kind,
    name,
    qualified_name,
    tokenize = "unicode61 separators '_' remove_diacritics 1"
);
"#;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let _: String = conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))
            .unwrap_or_else(|_| "memory".to_string());
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Replace the entire stored graph with the contents of `graph`.
    ///
    /// For the MVP this is a full overwrite. A future `save_incremental`
    /// will diff and emit only the changed nodes/edges with appropriate
    /// `valid_from`/`valid_to` updates.
    pub fn save(&mut self, graph: &Graph) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM edges", [])?;
        tx.execute("DELETE FROM nodes_fts", [])?;
        tx.execute("DELETE FROM nodes", [])?;

        let mut id_map: HashMap<u32, i64> = HashMap::new();
        for (nid, node) in graph.nodes() {
            let kind = serde_json::to_value(node.kind)?
                .as_str()
                .unwrap_or("")
                .to_string();
            let props = serde_json::to_string(&node.properties)?;
            tx.execute(
                "INSERT INTO nodes (kind, name, qualified_name, source_uri,
                                    line_start, line_end, properties,
                                    valid_from, valid_to)
                 VALUES (?,?,?,?,?,?,?,?,?)",
                params![
                    kind,
                    node.name,
                    node.qualified_name,
                    node.source_uri,
                    node.line_start,
                    node.line_end,
                    props,
                    node.valid_from,
                    node.valid_to
                ],
            )?;
            id_map.insert(nid.0, tx.last_insert_rowid());
        }

        tx.execute(
            "INSERT INTO nodes_fts(rowid, kind, name, qualified_name)
             SELECT id, kind, name, qualified_name FROM nodes",
            [],
        )?;

        for (_eid, src, dst, edge) in graph.edges() {
            let kind = serde_json::to_value(edge.kind)?
                .as_str()
                .unwrap_or("")
                .to_string();
            let conf_score = edge.confidence.score() as f64;
            let conf_class = edge.confidence.class_str();
            let props = serde_json::to_string(&edge.properties)?;
            let src_db = *id_map.get(&src.0).context("missing src in id map")?;
            let dst_db = *id_map.get(&dst.0).context("missing dst in id map")?;
            tx.execute(
                "INSERT INTO edges (src_id, dst_id, kind, confidence, conf_class,
                                    properties, valid_from, valid_to)
                 VALUES (?,?,?,?,?,?,?,?)",
                params![
                    src_db,
                    dst_db,
                    kind,
                    conf_score,
                    conf_class,
                    props,
                    edge.valid_from,
                    edge.valid_to
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn load(&self) -> Result<Graph> {
        let mut graph = Graph::new();
        let mut db_to_graph: HashMap<i64, NodeId> = HashMap::new();

        let mut stmt = self.conn.prepare(
            "SELECT id, kind, qualified_name, source_uri, line_start, line_end,
                    properties, valid_from, valid_to FROM nodes",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<u32>>(4)?,
                row.get::<_, Option<u32>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;

        for row in rows {
            let (db_id, kind_str, qname, src, ls, le, props_str, vf, vt) = row?;
            let kind: NodeKind = serde_json::from_value(serde_json::Value::String(kind_str))?;
            let mut node = Node::new(kind, &qname);
            node.source_uri = src;
            node.line_start = ls;
            node.line_end = le;
            node.properties = serde_json::from_str(&props_str).unwrap_or_default();
            node.valid_from = vf;
            node.valid_to = vt;
            let id = graph.add_node(node);
            db_to_graph.insert(db_id, id);
        }

        let mut stmt = self.conn.prepare(
            "SELECT src_id, dst_id, kind, confidence, conf_class,
                    properties, valid_from, valid_to FROM edges",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;

        for row in rows {
            let (src_db, dst_db, kind_str, conf, conf_class, props_str, vf, vt) = row?;
            let kind: EdgeKind = serde_json::from_value(serde_json::Value::String(kind_str))?;
            let confidence = match conf_class.as_str() {
                "extracted" => Confidence::Extracted,
                "inferred" => Confidence::Inferred(conf as f32),
                "ambiguous" => Confidence::Ambiguous,
                _ => Confidence::Inferred(conf as f32),
            };
            let edge = Edge {
                kind,
                confidence,
                properties: serde_json::from_str(&props_str).unwrap_or_default(),
                valid_from: vf,
                valid_to: vt,
            };
            let src = *db_to_graph.get(&src_db).context("unknown src")?;
            let dst = *db_to_graph.get(&dst_db).context("unknown dst")?;
            graph.add_edge(src, dst, edge);
        }

        Ok(graph)
    }

    pub fn stats(&self) -> Result<(usize, usize)> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        let e: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
        Ok((n as usize, e as usize))
    }

    pub fn file_hashes(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT path, hash FROM file_state")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut hashes = HashMap::new();
        for row in rows {
            let (path, hash) = row?;
            hashes.insert(path, hash);
        }
        Ok(hashes)
    }

    pub fn delete_sources(&mut self, sources: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for source in sources {
            tx.execute(
                "DELETE FROM edges
                 WHERE src_id IN (SELECT id FROM nodes WHERE source_uri = ?1)
                    OR dst_id IN (SELECT id FROM nodes WHERE source_uri = ?1)",
                params![source],
            )?;
            tx.execute(
                "DELETE FROM nodes_fts WHERE rowid IN
                 (SELECT id FROM nodes WHERE source_uri = ?1)",
                params![source],
            )?;
            tx.execute("DELETE FROM nodes WHERE source_uri = ?1", params![source])?;
            tx.execute("DELETE FROM file_state WHERE path = ?1", params![source])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_file_hashes(&mut self, hashes: &[(String, String)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        for (path, hash) in hashes {
            tx.execute(
                "INSERT INTO file_state(path, hash, indexed_at_unix)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET
                    hash = excluded.hash,
                    indexed_at_unix = excluded.indexed_at_unix",
                params![path, hash, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Full-text search via the FTS5 `nodes_fts` virtual table.
    ///
    /// Returns `(qualified_name, bm25_score)` pairs ordered by relevance
    /// (bm25 is negative; we negate so higher = better).
    /// Returns an empty `Vec` if the FTS table is not yet populated or the
    /// query contains no indexable tokens.
    pub fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<(String, f64)>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let fts_query = build_fts5_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let sql = "SELECT n.qualified_name, bm25(nodes_fts)
                   FROM nodes_fts
                   JOIN nodes n ON nodes_fts.rowid = n.id
                   WHERE nodes_fts MATCH ?1
                   ORDER BY bm25(nodes_fts)
                   LIMIT ?2";
        let mut stmt = match self.conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let mut results = Vec::new();
        let rows = stmt.query_map(params![fts_query, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?;
        for row in rows {
            if let Ok(r) = row {
                results.push(r);
            }
        }
        Ok(results)
    }

    /// Raw SQL access for temporal and differential queries.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// Build a safe FTS5 MATCH expression from a raw user query.
///
/// Each whitespace/punctuation-separated token becomes a prefix term (`token*`).
/// Special FTS5 syntax characters are stripped to prevent query parse errors.
pub(crate) fn build_fts5_query(raw: &str) -> String {
    let tokens: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| {
            let clean: String = t
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            clean
        })
        .filter(|t| !t.is_empty())
        .map(|t| format!("{}*", t))
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    // Tokens joined by space = AND in FTS5; each token is a prefix match.
    tokens.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{EdgeKind, NodeKind};

    #[test]
    fn round_trip_in_memory() {
        let mut g = Graph::new();
        let a = g.add_node(Node::new(NodeKind::Function, "m::f"));
        let b = g.add_node(Node::new(NodeKind::Function, "m::g"));
        g.add_edge(a, b, Edge::extracted(EdgeKind::Calls));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();
        let loaded = s.load().unwrap();
        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.edge_count(), 1);
        assert!(loaded.find_by_qname("m::f").is_some());
    }

    #[test]
    fn fts_search_finds_node_by_name() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "mymod::detect_changes"));
        g.add_node(Node::new(NodeKind::Function, "mymod::graph_builder"));
        g.add_node(Node::new(NodeKind::Class, "mymod::GraphNode"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let hits = s.fts_search("detect", 10).unwrap();
        assert!(
            !hits.is_empty(),
            "expected at least one FTS hit for 'detect'"
        );
        assert!(hits.iter().any(|(qn, _)| qn == "mymod::detect_changes"));
    }

    #[test]
    fn fts_search_prefix_match() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "ns::graph_traversal"));
        g.add_node(Node::new(NodeKind::Function, "ns::path_finder"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let hits = s.fts_search("graph", 10).unwrap();
        assert!(hits.iter().any(|(qn, _)| qn == "ns::graph_traversal"));
        // unrelated node should not appear
        assert!(!hits.iter().any(|(qn, _)| qn == "ns::path_finder"));
    }

    #[test]
    fn fts_search_empty_query_returns_empty() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "ns::f"));
        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();
        assert!(s.fts_search("", 10).unwrap().is_empty());
        assert!(s.fts_search("  ", 10).unwrap().is_empty());
    }

    #[test]
    fn fts_search_multi_word_matches_snake_case() {
        // With separators "_", "detect_changes" indexes as tokens "detect" + "changes".
        // A two-word query "detect changes" should AND-match both tokens.
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "mymod::detect_changes"));
        g.add_node(Node::new(NodeKind::Function, "mymod::detect_errors"));
        g.add_node(Node::new(NodeKind::Function, "mymod::apply_changes"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let hits = s.fts_search("detect changes", 10).unwrap();
        // Only "detect_changes" contains both tokens.
        assert_eq!(
            hits.len(),
            1,
            "expected only detect_changes to match 'detect changes'"
        );
        assert_eq!(hits[0].0, "mymod::detect_changes");
    }

    #[test]
    fn delete_sources_removes_fts_rows() {
        let mut g = Graph::new();
        let mut n = Node::new(NodeKind::Function, "mod::stale_fn");
        n.source_uri = Some("src/stale.rs".to_string());
        g.add_node(n);
        let mut n2 = Node::new(NodeKind::Function, "mod::keep_fn");
        n2.source_uri = Some("src/keep.rs".to_string());
        g.add_node(n2);

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        // confirm both are searchable
        assert!(!s.fts_search("stale", 10).unwrap().is_empty());
        assert!(!s.fts_search("keep", 10).unwrap().is_empty());

        s.delete_sources(&["src/stale.rs".to_string()]).unwrap();

        // stale_fn must be gone from FTS
        assert!(
            s.fts_search("stale", 10).unwrap().is_empty(),
            "stale FTS row should be removed"
        );
        // keep_fn must still be found
        assert!(!s.fts_search("keep", 10).unwrap().is_empty());
    }

    #[test]
    fn build_fts5_query_produces_prefix_terms() {
        assert_eq!(build_fts5_query("detect changes"), "detect* changes*");
        assert_eq!(build_fts5_query("graph"), "graph*");
        assert_eq!(build_fts5_query("detect_changes"), "detect_changes*");
        assert!(build_fts5_query("").is_empty());
    }
}
