//! SQLite-backed persistence for an Ariadne graph.
//!
//! The schema is intentionally tiny: `nodes`, `edges`, `embeddings`, `meta`.
//! Every node and edge row carries `valid_from` and `valid_to` SHA columns
//! so that temporal queries reduce to a `WHERE` clause and never require
//! a re-parse.

use anyhow::{Context, Result};
use ariadne_core::{Confidence, Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
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

    /// Raw SQL access for temporal and differential queries.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne_core::{EdgeKind, NodeKind};

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
}
