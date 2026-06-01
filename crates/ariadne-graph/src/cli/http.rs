use ariadne_graph::store::Store;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;

use super::response::graph_summary_json;

/// Handle HTTP request.
pub fn handle_http(mut stream: TcpStream, db: &Path, algorithm: &str) -> anyhow::Result<()> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if path == "/" {
        write_response(&mut stream, "text/html; charset=utf-8", INDEX_HTML)
    } else if path == "/app.js" {
        write_response(&mut stream, "application/javascript; charset=utf-8", APP_JS)
    } else if path == "/style.css" {
        write_response(&mut stream, "text/css; charset=utf-8", STYLE_CSS)
    } else if path.starts_with("/api/graph") {
        let body = graph_json(db, algorithm, path)?;
        write_response(&mut stream, "application/json", &body)
    } else if path.starts_with("/api/search") {
        let q = query_param(path, "q").unwrap_or_default();
        let body = search_json(db, &q, path)?;
        write_response(&mut stream, "application/json", &body)
    } else {
        write_not_found(&mut stream)
    }
}

/// Write HTTP response.
fn write_response(stream: &mut TcpStream, content_type: &str, body: &str) -> anyhow::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        content_type,
        body.len(),
        body
    )?;
    Ok(())
}

/// Write 404 response.
fn write_not_found(stream: &mut TcpStream) -> anyhow::Result<()> {
    let body = "not found";
    write!(
        stream,
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    Ok(())
}

/// Graph API endpoint.
fn graph_json(db: &Path, algorithm: &str, request_path: &str) -> anyhow::Result<String> {
    use ariadne_graph::query::{leiden, louvain};
    
    let store = Store::open(db)?;
    let graph = store.load()?;
    let node_offset = query_usize(request_path, "offset").unwrap_or(0);
    let node_limit = query_usize(request_path, "limit")
        .unwrap_or(1000)
        .clamp(1, 5000);
    let edge_offset = query_usize(request_path, "edge_offset").unwrap_or(0);
    let edge_limit = query_usize(request_path, "edge_limit")
        .unwrap_or(node_limit.saturating_mul(2))
        .clamp(1, 10000);
    let communities = match algorithm {
        "louvain" => louvain(&graph),
        "leiden" => leiden(&graph),
        _ => leiden(&graph),
    };
    let all_nodes: Vec<_> = graph
        .nodes()
        .map(|(id, n)| {
            let degree = graph.in_neighbors(id).count() + graph.out_neighbors(id).count();
            json!({
                "id": id.0,
                "label": n.name,
                "qname": n.qualified_name,
                "kind": n.kind,
                "source": n.source_uri,
                "degree": degree,
                "community": communities.get(&id).copied().unwrap_or(0),
            })
        })
        .collect();
    let all_edges: Vec<_> = graph
        .edges()
        .map(|(_, src, dst, e)| {
            json!({
                "source": src.0,
                "target": dst.0,
                "kind": e.kind,
                "confidence": e.confidence.score(),
            })
        })
        .collect();
    let total_nodes = all_nodes.len();
    let total_edges = all_edges.len();
    let nodes = paged_values(&all_nodes, node_offset, node_limit);
    let edges = paged_values(&all_edges, edge_offset, edge_limit);
    let returned_nodes = nodes.len();
    let returned_edges = edges.len();
    Ok(json!({
        "nodes": nodes,
        "links": edges,
        "graph_summary": graph_summary_json(&graph),
        "guardrails": {
            "nodes": pagination_json(node_offset, node_limit, returned_nodes, total_nodes),
            "links": pagination_json(edge_offset, edge_limit, returned_edges, total_edges),
        }
    })
    .to_string())
}

/// Search API endpoint.
fn search_json(db: &Path, query: &str, request_path: &str) -> anyhow::Result<String> {
    use ariadne_graph::query::ranked_search;
    
    let store = Store::open(db)?;
    let graph = store.load()?;
    let offset = query_usize(request_path, "offset").unwrap_or(0);
    let limit = query_usize(request_path, "limit")
        .unwrap_or(20)
        .clamp(1, 100);
    let hits: Vec<_> = ranked_search(&graph, query, offset.saturating_add(limit))
        .into_iter()
        .filter_map(|hit| {
            graph.node(hit.id).map(|n| {
                json!({
                    "id": hit.id.0,
                    "score": hit.score,
                    "label": n.name,
                    "qname": n.qualified_name,
                    "kind": n.kind,
                    "signals": hit.signals,
                })
            })
        })
        .collect();
    let total = hits.len();
    let page = paged_values(&hits, offset, limit);
    let returned = page.len();
    Ok(json!({
        "hits": page,
        "graph_summary": graph_summary_json(&graph),
        "guardrails": {
            "hits": pagination_json(offset, limit, returned, total),
        }
    })
    .to_string())
}

/// Paged values.
fn paged_values(values: &[serde_json::Value], offset: usize, limit: usize) -> Vec<serde_json::Value> {
    let start = offset.min(values.len());
    let end = (start + limit).min(values.len());
    values[start..end].to_vec()
}

/// Pagination JSON.
fn pagination_json(offset: usize, limit: usize, returned: usize, total: usize) -> serde_json::Value {
    json!({
        "offset": offset,
        "limit": limit,
        "returned": returned,
        "total": total,
        "has_more": offset.saturating_add(returned) < total,
    })
}

/// Query usize parameter.
fn query_usize(path: &str, name: &str) -> Option<usize> {
    query_param(path, name)?.parse().ok()
}

/// Query parameter.
fn query_param(path: &str, name: &str) -> Option<String> {
    let query = path.split_once('?')?.1;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            return Some(url_decode(value));
        }
    }
    None
}

/// URL decode.
fn url_decode(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '+' => out.push(' '),
            '%' => {
                let hex: String = chars.by_ref().take(2).collect();
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    out.push(byte as char);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Embedded static assets.
const INDEX_HTML: &str = include_str!("../../static/index.html");
const APP_JS: &str = include_str!("../../static/app.js");
const STYLE_CSS: &str = include_str!("../../static/style.css");
