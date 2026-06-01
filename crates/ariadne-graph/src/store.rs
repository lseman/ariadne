//! SQLite-backed persistence for an Ariadne graph.
//!
//! The schema is intentionally tiny: `nodes`, `edges`, `embeddings`, `meta`.
//! Every node and edge row carries `valid_from` and `valid_to` SHA columns
//! so that temporal queries reduce to a `WHERE` clause and never require
//! a re-parse.

use crate::core::{Confidence, Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

pub const DEFAULT_EMBEDDING_MODEL: &str = "ariadne-hash-v2";
const DEFAULT_EMBEDDING_DIM: usize = 384;

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

CREATE TABLE IF NOT EXISTS node_versions (
    id             INTEGER PRIMARY KEY,
    kind           TEXT NOT NULL,
    name           TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    source_uri     TEXT,
    line_start     INTEGER,
    line_end       INTEGER,
    properties     TEXT NOT NULL DEFAULT '{}',
    valid_from     TEXT,
    valid_to       TEXT
);
CREATE INDEX IF NOT EXISTS idx_node_versions_qname ON node_versions(qualified_name);
CREATE INDEX IF NOT EXISTS idx_node_versions_source ON node_versions(source_uri);
CREATE INDEX IF NOT EXISTS idx_node_versions_valid ON node_versions(valid_from, valid_to);

CREATE TABLE IF NOT EXISTS edge_versions (
    id             INTEGER PRIMARY KEY,
    src_qname      TEXT NOT NULL,
    dst_qname      TEXT NOT NULL,
    kind           TEXT NOT NULL,
    confidence     REAL NOT NULL,
    conf_class     TEXT NOT NULL,
    properties     TEXT NOT NULL DEFAULT '{}',
    source_uri     TEXT,
    valid_from     TEXT,
    valid_to       TEXT
);
CREATE INDEX IF NOT EXISTS idx_edge_versions_src ON edge_versions(src_qname);
CREATE INDEX IF NOT EXISTS idx_edge_versions_dst ON edge_versions(dst_qname);
CREATE INDEX IF NOT EXISTS idx_edge_versions_kind ON edge_versions(kind);
CREATE INDEX IF NOT EXISTS idx_edge_versions_valid ON edge_versions(valid_from, valid_to);

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

#[derive(Debug, Clone)]
pub struct StoredNodeRow {
    pub node: Node,
}

#[derive(Debug, Clone)]
pub struct StoredEdgeRow {
    pub src_qname: String,
    pub dst_qname: String,
    pub edge: Edge,
    pub source_uri: Option<String>,
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
        let existing_embedding_model: Option<String> = self
            .conn
            .query_row("SELECT model FROM embeddings LIMIT 1", [], |r| r.get(0))
            .optional()?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM edges", [])?;
        tx.execute("DELETE FROM embeddings", [])?;
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
        if let Some(model) = existing_embedding_model {
            self.rebuild_embeddings(&model)?;
        }
        Ok(())
    }

    pub fn reset_all(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM edge_versions", [])?;
        tx.execute("DELETE FROM node_versions", [])?;
        tx.execute("DELETE FROM embeddings", [])?;
        tx.execute("DELETE FROM edges", [])?;
        tx.execute("DELETE FROM nodes_fts", [])?;
        tx.execute("DELETE FROM nodes", [])?;
        tx.execute("DELETE FROM file_state", [])?;
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

    pub fn embedding_stats(&self) -> Result<(usize, Option<String>)> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))?;
        let model = self
            .conn
            .query_row("SELECT model FROM embeddings LIMIT 1", [], |r| r.get(0))
            .optional()?;
        Ok((count as usize, model))
    }

    pub fn fts_stats(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes_fts", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    pub fn has_temporal_history(&self) -> Result<bool> {
        let archived_nodes: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM node_versions", [], |r| r.get(0))?;
        let archived_edges: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM edge_versions", [], |r| r.get(0))?;
        if archived_nodes > 0 || archived_edges > 0 {
            return Ok(true);
        }
        let stamped_nodes: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE valid_from IS NOT NULL OR valid_to IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        let stamped_edges: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE valid_from IS NOT NULL OR valid_to IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(stamped_nodes > 0 || stamped_edges > 0)
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

    pub fn active_nodes_for_sources(&self, sources: &[String]) -> Result<Vec<StoredNodeRow>> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stmt = self.conn.prepare(
            "SELECT kind, qualified_name, source_uri, line_start, line_end,
                    properties, valid_from, valid_to
             FROM nodes
             WHERE source_uri = ?1",
        )?;
        for source in sources {
            let rows = stmt.query_map(params![source], |row| {
                Ok(node_row_from_sql(
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<u32>>(3)?,
                    row.get::<_, Option<u32>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?;
            for row in rows {
                let node = row?;
                if seen.insert(node.qualified_name.clone()) {
                    out.push(StoredNodeRow { node });
                }
            }
        }
        Ok(out)
    }

    pub fn active_edges_for_sources(&self, sources: &[String]) -> Result<Vec<StoredEdgeRow>> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stmt = self.conn.prepare(
            "SELECT s.qualified_name, d.qualified_name, e.kind, e.confidence, e.conf_class,
                    e.properties, e.valid_from, e.valid_to,
                    COALESCE(s.source_uri, d.source_uri)
             FROM edges e
             JOIN nodes s ON e.src_id = s.id
             JOIN nodes d ON e.dst_id = d.id
             WHERE s.source_uri = ?1 OR d.source_uri = ?1",
        )?;
        for source in sources {
            let rows = stmt.query_map(params![source], |row| {
                Ok(StoredEdgeRow {
                    src_qname: row.get::<_, String>(0)?,
                    dst_qname: row.get::<_, String>(1)?,
                    edge: edge_row_from_sql(
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ),
                    source_uri: row.get::<_, Option<String>>(8)?,
                })
            })?;
            for row in rows {
                let row = row?;
                if seen.insert(edge_identity(&row.src_qname, &row.dst_qname, row.edge.kind)) {
                    out.push(row);
                }
            }
        }
        Ok(out)
    }

    pub fn archive_nodes(&mut self, nodes: &[StoredNodeRow], valid_to: &str) -> Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for row in nodes {
            let kind = serde_json::to_value(row.node.kind)?
                .as_str()
                .unwrap_or("")
                .to_string();
            let props = serde_json::to_string(&row.node.properties)?;
            tx.execute(
                "INSERT INTO node_versions (kind, name, qualified_name, source_uri,
                                             line_start, line_end, properties,
                                             valid_from, valid_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    kind,
                    row.node.name,
                    row.node.qualified_name,
                    row.node.source_uri,
                    row.node.line_start,
                    row.node.line_end,
                    props,
                    row.node
                        .valid_from
                        .clone()
                        .or_else(|| Some(valid_to.to_string())),
                    Some(valid_to.to_string()),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn archive_edges(&mut self, edges: &[StoredEdgeRow], valid_to: &str) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for row in edges {
            let kind = serde_json::to_value(row.edge.kind)?
                .as_str()
                .unwrap_or("")
                .to_string();
            let props = serde_json::to_string(&row.edge.properties)?;
            tx.execute(
                "INSERT INTO edge_versions (src_qname, dst_qname, kind, confidence, conf_class,
                                             properties, source_uri, valid_from, valid_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    row.src_qname,
                    row.dst_qname,
                    kind,
                    row.edge.confidence.score() as f64,
                    row.edge.confidence.class_str(),
                    props,
                    row.source_uri,
                    row.edge
                        .valid_from
                        .clone()
                        .or_else(|| Some(valid_to.to_string())),
                    Some(valid_to.to_string()),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn temporal_nodes(&self) -> Result<Vec<StoredNodeRow>> {
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT kind, qualified_name, source_uri, line_start, line_end,
                    properties, valid_from, valid_to
             FROM nodes
             UNION ALL
             SELECT kind, qualified_name, source_uri, line_start, line_end,
                    properties, valid_from, valid_to
             FROM node_versions",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredNodeRow {
                node: node_row_from_sql(
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<u32>>(3)?,
                    row.get::<_, Option<u32>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ),
            })
        })?;
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn temporal_edges(&self) -> Result<Vec<StoredEdgeRow>> {
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT s.qualified_name, d.qualified_name, e.kind, e.confidence, e.conf_class,
                    e.properties, e.valid_from, e.valid_to,
                    COALESCE(s.source_uri, d.source_uri)
             FROM edges e
             JOIN nodes s ON e.src_id = s.id
             JOIN nodes d ON e.dst_id = d.id
             UNION ALL
             SELECT src_qname, dst_qname, kind, confidence, conf_class,
                    properties, valid_from, valid_to, source_uri
             FROM edge_versions",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredEdgeRow {
                src_qname: row.get::<_, String>(0)?,
                dst_qname: row.get::<_, String>(1)?,
                edge: edge_row_from_sql(
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ),
                source_uri: row.get::<_, Option<String>>(8)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
        Ok(out)
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
            tx.execute(
                "DELETE FROM embeddings WHERE node_id IN
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
        for r in rows.flatten() {
            results.push(r);
        }
        Ok(results)
    }

    /// Rebuild the FTS5 index from the current `nodes` table.
    ///
    /// `save` maintains this automatically for normal builds and updates; this
    /// method is useful for older databases or manual repair.
    pub fn rebuild_fts_index(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM nodes_fts", [])?;
        tx.execute(
            "INSERT INTO nodes_fts(rowid, kind, name, qualified_name)
             SELECT id, kind, name, qualified_name FROM nodes",
            [],
        )?;
        tx.commit()?;
        self.fts_stats()
    }

    /// Build or rebuild lightweight local embeddings for semantic search.
    ///
    /// The default model is a deterministic local feature-hash vector
    /// (`ariadne-hash-v2`). It requires no external services and is meant
    /// to complement FTS5, not replace it.
    pub fn rebuild_embeddings(&mut self, model: &str) -> Result<usize> {
        if model != DEFAULT_EMBEDDING_MODEL {
            bail!(
                "unsupported embedding model {}; supported model is {}",
                model,
                DEFAULT_EMBEDDING_MODEL
            );
        }
        let rows: Vec<(i64, String, String, String, Option<String>)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, kind, name, qualified_name, source_uri
                 FROM nodes
                 WHERE qualified_name NOT LIKE 'call::%'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };

        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM embeddings", [])?;
        for (node_id, kind, name, qname, source_uri) in rows {
            let text = embedding_source_text(&kind, &name, &qname, source_uri.as_deref());
            let vector = semantic_embedding(&text);
            tx.execute(
                "INSERT INTO embeddings(node_id, model, vector) VALUES (?1, ?2, ?3)",
                params![node_id, model, encode_embedding(&vector)],
            )?;
        }
        tx.commit()?;
        let (count, _) = self.embedding_stats()?;
        Ok(count)
    }

    /// Semantic search over stored embeddings. Returns `(qualified_name,
    /// cosine_score)` pairs ordered descending.
    pub fn semantic_search(&self, query: &str, limit: usize) -> Result<Vec<(String, f32)>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let (count, _) = self.embedding_stats()?;
        if count == 0 {
            return Ok(Vec::new());
        }

        let query_vector = semantic_embedding(query);
        if query_vector.iter().all(|v| *v == 0.0) {
            return Ok(Vec::new());
        }

        let mut stmt = self.conn.prepare(
            "SELECT n.qualified_name, e.vector
             FROM embeddings e
             JOIN nodes n ON e.node_id = n.id
             WHERE n.qualified_name NOT LIKE 'call::%'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut results = Vec::new();
        for row in rows {
            let (qname, blob) = row?;
            let Some(vector) = decode_embedding(&blob) else {
                continue;
            };
            let score = cosine_similarity(&query_vector, &vector);
            if score >= 0.20 {
                results.push((qname, score));
            }
        }
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }

    /// Raw SQL access for temporal and differential queries.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn embedding_source_text(
    kind: &str,
    name: &str,
    qualified_name: &str,
    source_uri: Option<&str>,
) -> String {
    let mut text = format!("{} {} {}", kind, name, qualified_name.replace("::", " "));
    if let Some(source_uri) = source_uri {
        text.push(' ');
        text.push_str(source_uri);
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn node_row_from_sql(
    kind_str: String,
    qname: String,
    source_uri: Option<String>,
    line_start: Option<u32>,
    line_end: Option<u32>,
    properties: String,
    valid_from: Option<String>,
    valid_to: Option<String>,
) -> Node {
    let kind: NodeKind =
        serde_json::from_value(serde_json::Value::String(kind_str)).unwrap_or(NodeKind::Function);
    let mut node = Node::new(kind, qname);
    node.source_uri = source_uri;
    node.line_start = line_start;
    node.line_end = line_end;
    node.properties = serde_json::from_str(&properties).unwrap_or_default();
    node.valid_from = valid_from;
    node.valid_to = valid_to;
    node
}

fn edge_row_from_sql(
    kind_str: String,
    confidence: f64,
    conf_class: String,
    properties: String,
    valid_from: Option<String>,
    valid_to: Option<String>,
) -> Edge {
    let kind: EdgeKind =
        serde_json::from_value(serde_json::Value::String(kind_str)).unwrap_or(EdgeKind::Calls);
    let confidence = match conf_class.as_str() {
        "extracted" => Confidence::Extracted,
        "inferred" => Confidence::Inferred(confidence as f32),
        "ambiguous" => Confidence::Ambiguous,
        _ => Confidence::Inferred(confidence as f32),
    };
    Edge {
        kind,
        confidence,
        properties: serde_json::from_str(&properties).unwrap_or_default(),
        valid_from,
        valid_to,
    }
}

pub fn edge_identity(src_qname: &str, dst_qname: &str, kind: EdgeKind) -> String {
    format!("{}\u{1f}{}\u{1f}{:?}", src_qname, dst_qname, kind)
}

fn semantic_embedding(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0; DEFAULT_EMBEDDING_DIM];
    let tokens = semantic_tokens(text);
    if tokens.is_empty() {
        return vector;
    }

    let unique_tokens = unique_ordered(&tokens);
    for token in &tokens {
        push_signed_hashed_feature(&mut vector, &format!("tok:{token}"), 1.25);
        push_signed_hashed_feature(&mut vector, &format!("stem:{}", code_stem(token)), 0.70);
        let canonical = canonical_token(token);
        if canonical != *token {
            push_signed_hashed_feature(&mut vector, &format!("canon:{canonical}"), 1.05);
        }
        for gram in char_ngrams(token, 3, 5) {
            push_signed_hashed_feature(&mut vector, &format!("char:{gram}"), 0.24);
        }
        for piece in token_pieces(token) {
            push_signed_hashed_feature(&mut vector, &format!("piece:{piece}"), 0.42);
        }
    }

    for pair in tokens.windows(2) {
        push_signed_hashed_feature(&mut vector, &format!("bi:{}:{}", pair[0], pair[1]), 0.82);
        let left = canonical_token(&pair[0]);
        let right = canonical_token(&pair[1]);
        if left != pair[0] || right != pair[1] {
            push_signed_hashed_feature(&mut vector, &format!("cbi:{left}:{right}"), 0.58);
        }
    }
    for triple in tokens.windows(3) {
        push_signed_hashed_feature(
            &mut vector,
            &format!("tri:{}:{}:{}", triple[0], triple[1], triple[2]),
            0.36,
        );
        push_signed_hashed_feature(
            &mut vector,
            &format!("skip:{}:{}", triple[0], triple[2]),
            0.28,
        );
    }

    let acronym: String = unique_tokens
        .iter()
        .filter_map(|token| token.chars().next())
        .collect();
    if acronym.len() >= 2 {
        push_signed_hashed_feature(&mut vector, &format!("acro:{acronym}"), 0.85);
    }

    for concept in semantic_concepts(&unique_tokens) {
        push_signed_hashed_feature(&mut vector, &format!("concept:{concept}"), 3.0);
    }

    normalize_vector(&mut vector);
    vector
}

fn semantic_tokens(raw: &str) -> Vec<String> {
    let mut normalized = String::new();
    let mut prev: Option<char> = None;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        if c.is_ascii_alphanumeric() {
            if let Some(p) = prev {
                let camel_boundary = p.is_ascii_lowercase() && c.is_ascii_uppercase();
                let acronym_boundary = p.is_ascii_uppercase()
                    && c.is_ascii_uppercase()
                    && next.is_some_and(|n| n.is_ascii_lowercase());
                let digit_boundary = p.is_ascii_alphabetic() != c.is_ascii_alphabetic();
                if camel_boundary || acronym_boundary || digit_boundary {
                    normalized.push(' ');
                }
            }
            normalized.push(c.to_ascii_lowercase());
            prev = Some(c);
        } else {
            normalized.push(' ');
            prev = None;
        }
    }

    normalized
        .split_whitespace()
        .map(singularize_token)
        .filter(|token| !token.is_empty())
        .collect()
}

fn unique_ordered(tokens: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for token in tokens {
        if seen.insert(token.as_str()) {
            out.push(token.clone());
        }
    }
    out
}

fn singularize_token(token: &str) -> String {
    if token.len() > 4 && token.ends_with('s') {
        token[..token.len() - 1].to_string()
    } else {
        token.to_string()
    }
}

fn canonical_token(token: &str) -> String {
    match token {
        "delete" | "deleted" | "remove" | "removed" | "drop" | "purge" | "cleanup" => {
            "remove".to_string()
        }
        "add" | "added" | "create" | "created" | "insert" | "new" => "add".to_string(),
        "change" | "changed" | "changes" | "diff" | "delta" | "modify" | "modified" | "update"
        | "updated" => "change".to_string(),
        "find" | "search" | "lookup" | "query" | "discover" => "search".to_string(),
        "auth" | "authenticate" | "authentication" | "login" | "signin" | "signon" => {
            "auth".to_string()
        }
        "test" | "tests" | "spec" | "specs" | "coverage" => "test".to_string(),
        "bug" | "defect" | "error" | "failure" | "panic" | "regression" => "bug".to_string(),
        "cache" | "cached" | "memo" | "memoize" | "memoized" => "cache".to_string(),
        "config" | "configuration" | "setting" | "settings" => "config".to_string(),
        "db" | "database" | "sqlite" | "store" | "storage" | "persist" | "persistence" => {
            "storage".to_string()
        }
        "doc" | "docs" | "document" | "documentation" | "readme" => "doc".to_string(),
        "embed" | "embedding" | "embeddings" | "semantic" | "vector" | "vectors" => {
            "embedding".to_string()
        }
        "file" | "files" | "path" | "paths" | "source" | "sources" => "source".to_string(),
        "graph" | "node" | "nodes" | "edge" | "edges" | "flow" | "flows" => "graph".to_string(),
        "http" | "server" | "serve" | "route" | "routes" => "server".to_string(),
        "ignore" | "gitignore" | "ariadneignore" | "exclude" | "skip" => "ignore".to_string(),
        "index" | "indexed" | "indexing" | "fts" | "fts5" => "index".to_string(),
        "install" | "installer" | "setup" | "hook" | "hooks" => "install".to_string(),
        "json" | "mcp" | "tool" | "tools" | "agent" | "agents" => "agent".to_string(),
        "rank" | "ranking" | "score" | "scored" | "scoring" | "boost" | "boosted" => {
            "rank".to_string()
        }
        "read" | "reader" | "parse" | "parser" | "extract" | "extraction" => "extract".to_string(),
        "review" | "risk" | "impact" | "blast" | "radius" => "review".to_string(),
        "symbol" | "symbols" | "function" | "functions" | "method" | "methods" => {
            "symbol".to_string()
        }
        "terminal" | "tui" | "ui" | "viewer" | "view" => "ui".to_string(),
        "watch" | "daemon" | "poll" | "polling" => "watch".to_string(),
        other => other.to_string(),
    }
}

fn code_stem(token: &str) -> String {
    let mut stem = singularize_token(token);
    for suffix in ["ing", "ed", "er", "or", "able", "ible", "tion", "ions"] {
        if stem.len() > suffix.len() + 3 && stem.ends_with(suffix) {
            stem.truncate(stem.len() - suffix.len());
            break;
        }
    }
    stem
}

fn token_pieces(token: &str) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 3 {
        return Vec::new();
    }
    let mut pieces = Vec::new();
    for len in [3usize, 4, 5] {
        if chars.len() >= len {
            pieces.push(chars[..len].iter().collect());
            pieces.push(chars[chars.len() - len..].iter().collect());
        }
    }
    pieces.sort();
    pieces.dedup();
    pieces
}

fn semantic_concepts(tokens: &[String]) -> Vec<&'static str> {
    let has = |words: &[&str]| tokens.iter().any(|token| words.contains(&token.as_str()));
    let mut concepts = Vec::new();
    if has(&["embed", "embedding", "semantic", "vector"]) {
        concepts.push("embedding");
    }
    if has(&["delete", "remove", "drop", "purge"]) && has(&["file", "source", "node", "edge"]) {
        concepts.push("delete-source");
    }
    if has(&["test", "spec", "coverage"]) && has(&["risk", "review", "change", "diff"]) {
        concepts.push("review-coverage");
    }
    if has(&["embed", "embedding", "semantic", "vector"]) && has(&["search", "rank", "score"]) {
        concepts.push("semantic-search");
    }
    if has(&["mcp", "tool", "agent", "json"]) {
        concepts.push("agent-interface");
    }
    if has(&["config", "mcp", "setup"]) || has(&["watch", "daemon", "hook", "install"]) {
        concepts.push("automation");
    }
    if has(&["graph", "node", "edge", "flow"]) && has(&["rank", "impact", "path", "community"]) {
        concepts.push("graph-reasoning");
    }
    concepts
}

fn char_ngrams(token: &str, min_n: usize, max_n: usize) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    let mut out = Vec::new();
    for n in min_n..=max_n {
        if chars.len() < n {
            continue;
        }
        for i in 0..=chars.len() - n {
            out.push(chars[i..i + n].iter().collect());
        }
    }
    out
}

fn push_signed_hashed_feature(vector: &mut [f32], feature: &str, weight: f32) {
    let hash = stable_hash64(feature.as_bytes(), 0xcbf29ce484222325);
    let index = (hash as usize) % vector.len();
    let sign = if stable_hash64(feature.as_bytes(), 0x9e3779b97f4a7c15) & 1 == 0 {
        1.0
    } else {
        -1.0
    };
    vector[index] += sign * weight;
}

fn stable_hash64(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

fn encode_embedding(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_embedding(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % std::mem::size_of::<f32>() != 0 {
        return None;
    }
    let mut vector = Vec::with_capacity(blob.len() / std::mem::size_of::<f32>());
    for chunk in blob.chunks_exact(std::mem::size_of::<f32>()) {
        vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(vector)
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (l, r) in left.iter().zip(right.iter()) {
        dot += l * r;
        left_norm += l * l;
        right_norm += r * r;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
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
    fn rebuild_fts_index_reports_indexed_rows() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "ns::alpha_search"));
        g.add_node(Node::new(NodeKind::Class, "ns::BetaSearch"));
        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();

        let count = s.rebuild_fts_index().unwrap();
        assert_eq!(count, 2);
        assert_eq!(s.fts_stats().unwrap(), 2);
        assert!(!s.fts_search("alpha", 10).unwrap().is_empty());
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

    #[test]
    fn semantic_search_finds_related_terms() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "pkg::remove_sources"));
        g.add_node(Node::new(NodeKind::Function, "pkg::build_graph"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();
        s.rebuild_embeddings(DEFAULT_EMBEDDING_MODEL).unwrap();

        let hits = s.semantic_search("delete source", 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0, "pkg::remove_sources");
    }

    #[test]
    fn hash_v2_captures_code_aware_semantic_concepts() {
        let mut g = Graph::new();
        g.add_node(Node::new(NodeKind::Function, "pkg::rebuild_embeddings"));
        g.add_node(Node::new(NodeKind::Function, "pkg::install_mcp_config"));
        g.add_node(Node::new(NodeKind::Function, "pkg::compute_flows"));

        let mut s = Store::open_in_memory().unwrap();
        s.save(&g).unwrap();
        s.rebuild_embeddings(DEFAULT_EMBEDDING_MODEL).unwrap();

        let semantic_hits = s
            .semantic_search("vector semantic search ranking", 5)
            .unwrap();
        assert!(!semantic_hits.is_empty());
        assert_eq!(semantic_hits[0].0, "pkg::rebuild_embeddings");

        let agent_hits = s.semantic_search("agent json tool setup", 5).unwrap();
        assert!(!agent_hits.is_empty());
        assert_eq!(agent_hits[0].0, "pkg::install_mcp_config");
    }
}
