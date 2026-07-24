//! cmd_tool and cmd_mcp_server.

use anyhow::Result;
use serde_json::json;
use std::path::Path;

pub fn cmd_tool(db: &Path, operation: &str, params: &str) -> Result<()> {
    let params: serde_json::Value = serde_json::from_str(params)?;
    let response = super::response::tool_response(db, operation, &params)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

pub fn cmd_mcp_server(db: &Path) -> Result<()> {
    use super::mcp::{ariadne_mcp_tool_schema, mcp_error, read_mcp_message, write_mcp_message};

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();
    while let Some(message) = read_mcp_message(&mut reader)? {
        let request: serde_json::Value = serde_json::from_str(&message)?;
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let id = request.get("id").cloned();

        if method == "notifications/initialized" {
            continue;
        }

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "ariadne", "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": [ariadne_mcp_tool_schema()] }
            }),
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                let name = params
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name != "graph" {
                    mcp_error(id, -32602, "unknown tool")
                } else {
                    let operation = args
                        .get("operation")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("status");
                    let tool_params = args.get("params").cloned().unwrap_or_else(|| json!({}));
                    match super::response::tool_response_cached(db, operation, &tool_params) {
                        Ok(result) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&result)?
                                }]
                            }
                        }),
                        Err(e) => mcp_error(id, -32000, &e.to_string()),
                    }
                }
            }
            _ => mcp_error(id, -32601, "method not found"),
        };
        write_mcp_message(&mut stdout, &response)?;
    }
    Ok(())
}
