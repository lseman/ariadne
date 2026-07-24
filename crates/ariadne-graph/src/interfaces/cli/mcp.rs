use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

/// MCP tool schema.
pub fn ariadne_mcp_tool_schema() -> Value {
    json!({
        "name": "graph",
        "description": "One-tool interface to Ariadne's code graph: search, review context, impact, paths, architecture, cycles, core nodes, and more.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "Operation name, e.g. minimal_context, search, detect_changes, review_context, impact, paths, traverse, architecture_overview, cycles, core, bridge_nodes, gaps, flows, affected_flows."
                },
                "params": {
                    "type": "object",
                    "description": "Operation-specific parameters. Add detail_level=minimal|standard|full for compactness control."
                }
            },
            "required": ["operation"]
        }
    })
}

/// MCP servers config.
pub fn mcp_servers_config(exe: &Path, db: &Path) -> Value {
    json!({
        "mcpServers": {
            "ariadne": ariadne_stdio_server_config(exe, db)
        }
    })
}

/// VSCode MCP config.
pub fn vscode_mcp_config(exe: &Path, db: &Path) -> Value {
    json!({
        "servers": {
            "ariadne": {
                "type": "stdio",
                "command": exe,
                "args": ["--db", db, "mcp-server"]
            }
        }
    })
}

/// Ariadne stdio server config.
pub fn ariadne_stdio_server_config(exe: &Path, db: &Path) -> Value {
    json!({
        "command": exe,
        "args": ["--db", db, "mcp-server"]
    })
}

/// Codex MCP toml.
pub fn codex_mcp_toml(exe: &Path, db: &Path) -> String {
    format!(
        r#"# Ariadne MCP server for Codex.
# Add this table to ~/.codex/config.toml if your Codex build does not load project-local snippets.

[mcp_servers.ariadne]
command = {}
args = ["--db", {}, "mcp-server"]
"#,
        toml_string(&exe.display().to_string()),
        toml_string(&db.display().to_string())
    )
}

/// TOML string.
fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// Read an MCP message from the reader.
/// Supports two framing formats:
/// 1. Content-Length framing (standard MCP): `Content-Length: N\r\n\r\n{body}`
/// 2. Newline-delimited JSON (NDJSON): `{json}\n` — used by the TS SDK
pub fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let trimmed = line.trim_end_matches(['\r', '\n']);

    if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
        // Content-Length framing: parse header, read exact body
        let len = len_str.trim().parse::<usize>()?;
        let mut buf = vec![0u8; len];
        // Consume the trailing \r\n after Content-Length header
        let mut sep = String::new();
        reader.read_line(&mut sep)?;
        reader.read_exact(&mut buf)?;
        Ok(Some(String::from_utf8(buf)?))
    } else {
        // NDJSON: the line itself is the JSON message
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }
}

/// Write MCP message as newline-delimited JSON (compatible with both
/// Content-Length clients and NDJSON clients like the TS SDK).
pub fn write_mcp_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_string(value)?;
    // Send NDJSON (\n-terminated) — works with the TS SDK's ReadBuffer
    // and is also accepted by Content-Length readers that parse the JSON body
    writeln!(writer, "{}", body)?;
    writer.flush()?;
    Ok(())
}

/// MCP error.
pub fn mcp_error(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}
