use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

/// MCP tool schema.
pub fn ariadne_mcp_tool_schema() -> Value {
    json!({
        "name": "ariadne",
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
#[allow(dead_code)]
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

/// Required string parameter.
pub fn required_str<'a>(params: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string param '{}'", key))
}

/// MCP framing.
#[allow(dead_code)]
pub struct McpFraming;

/// MCP message.
#[allow(dead_code)]
pub struct McpMessage {
    pub content_length: usize,
    pub body: String,
}

/// Read MCP message.
pub fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let Some(len) = content_length else {
        return Ok(None);
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8(buf)?))
}

/// Write MCP message.
pub fn write_mcp_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_string(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
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
