//! `Store` struct and database operations.

use crate::core::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

use super::query::{edge_row_from_sql, node_row_from_sql};
mod schema;
pub use schema::SCHEMA;

pub const DEFAULT_EMBEDDING_MODEL: &str = "ariadne-hash-v2";
pub const DEFAULT_EMBEDDING_DIM: usize = 384;

type EmbeddingNodeRow = (i64, String, String, String, Option<String>, Option<String>);

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
        // Migrate: add source_text column for v1→v2.
        Self::migrate_v1(&conn)?;
        Ok(Self { conn })
    }

    fn migrate_v1(conn: &Connection) -> Result<()> {
        // Check if source_text column exists by trying to query it.
        let has_column = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('nodes') WHERE name='source_text'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0);
        if has_column == 0 {
            conn.execute("ALTER TABLE nodes ADD COLUMN source_text TEXT", [])?;
            conn.execute("ALTER TABLE node_versions ADD COLUMN source_text TEXT", [])?;
            conn.execute("UPDATE meta SET value='2' WHERE key='schema_version'", [])
                .ok();
        }
        Ok(())
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

        // Batch node inserts with a prepared statement.
        let mut node_stmt = tx.prepare(
            "INSERT INTO nodes (kind, name, qualified_name, source_uri,
                                line_start, line_end, properties,
                                valid_from, valid_to, source_text)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
        )?;
        let mut id_map: HashMap<u32, i64> = HashMap::new();
        for (nid, node) in graph.nodes() {
            let kind = serde_json::to_value(node.kind)?
                .as_str()
                .unwrap_or("")
                .to_string();
            let props = serde_json::to_string(&node.properties)?;
            node_stmt.execute(params![
                kind,
                node.name,
                node.qualified_name,
                node.source_uri,
                node.line_start,
                node.line_end,
                props,
                node.valid_from,
                node.valid_to,
                node.source_text.clone(),
            ])?;
            id_map.insert(nid.0, tx.last_insert_rowid());
        }
        drop(node_stmt);

        tx.execute(
            "INSERT INTO nodes_fts(rowid, kind, name, qualified_name)
             SELECT id, kind, name, qualified_name FROM nodes",
            [],
        )?;

        // Batch edge inserts with a prepared statement.
        let mut edge_stmt = tx.prepare(
            "INSERT INTO edges (src_id, dst_id, kind, confidence, conf_class,
                                properties, valid_from, valid_to)
             VALUES (?,?,?,?,?,?,?,?)",
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
            edge_stmt.execute(params![
                src_db,
                dst_db,
                kind,
                conf_score,
                conf_class,
                props,
                edge.valid_from,
                edge.valid_to,
            ])?;
        }
        drop(edge_stmt);

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
            let confidence = super::query::parse_confidence(&conf_class, conf);
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
                    properties, valid_from, valid_to, source_text
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
                    row.get::<_, Option<String>>(8)?,
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
                if seen.insert(super::query::edge_identity(
                    &row.src_qname,
                    &row.dst_qname,
                    row.edge.kind,
                )) {
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
                    properties, valid_from, valid_to, source_text
             FROM nodes
             UNION ALL
             SELECT kind, qualified_name, source_uri, line_start, line_end,
                    properties, valid_from, valid_to, source_text
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
                    row.get::<_, Option<String>>(8)?,
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

    /// Load a graph including archived (closed-out) rows from the version
    /// tables, so temporal diffs can see nodes/edges that are no longer
    /// active. Archived rows are inserted first and active rows last, so
    /// for a qualified name present in both the active state wins — which
    /// keeps purely-removed rows visible while preferring the live row for
    /// rows that still exist.
    pub fn load_temporal(&self) -> Result<Graph> {
        let mut graph = Graph::new();

        let mut nodes = self.temporal_nodes()?;
        // Sort so archived rows (valid_to set) come before active rows.
        nodes.sort_by_key(|r| r.node.valid_to.is_none());
        for row in nodes {
            graph.add_node(row.node);
        }

        let mut edges = self.temporal_edges()?;
        edges.sort_by_key(|r| r.edge.valid_to.is_none());
        for row in edges {
            let (Some(src), Some(dst)) = (
                graph.find_by_qname(&row.src_qname),
                graph.find_by_qname(&row.dst_qname),
            ) else {
                continue;
            };
            graph.add_edge(src, dst, row.edge);
        }

        Ok(graph)
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
        let fts_query = super::query::build_fts5_query(query);
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
        // Collect node data before starting the transaction.
        let nodes_data: Vec<EmbeddingNodeRow> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, kind, name, qualified_name, source_uri, source_text
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
                    row.get::<_, Option<String>>(5)?,
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
        for (row_id, kind, name, qname, source_uri, source_text) in nodes_data {
            let text = super::embedding::embedding_source_text(
                &kind,
                &name,
                &qname,
                source_uri.as_deref(),
                source_text.as_deref(),
            );
            let vector = super::embedding::semantic_embedding(&text);
            tx.execute(
                "INSERT INTO embeddings(node_id, model, vector) VALUES (?1, ?2, ?3)",
                params![
                    row_id,
                    DEFAULT_EMBEDDING_MODEL,
                    super::embedding::encode_embedding(&vector)
                ],
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

        let query_vector = super::embedding::semantic_embedding(query);
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
            let Some(vector) = super::embedding::decode_embedding(&blob) else {
                continue;
            };
            let score = super::embedding::cosine_similarity(&query_vector, &vector);
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
