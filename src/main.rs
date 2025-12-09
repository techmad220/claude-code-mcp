//! MCP Server implementation for Claude Code session history
//!
//! This server exposes Claude Code CLI session history via the Model Context Protocol,
//! allowing Claude.ai (or any MCP client) to search and reference CLI work.

use anyhow::Result;
use serde_json::{json, Value};
#[allow(unused_imports)]
use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

mod protocol;
mod sessions;

use protocol::*;
use sessions::SessionStore;

/// Define available tools
fn get_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_sessions".to_string(),
            description: "List recent Claude Code CLI sessions. Returns session IDs, timestamps, and previews.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of sessions to return (default: 20, max: 100)",
                        "default": 20
                    }
                }
            }),
        },
        Tool {
            name: "search_sessions".to_string(),
            description: "Search Claude Code CLI sessions by keyword. Finds sessions containing the search term in messages.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query to find in session content"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 10, max: 50)",
                        "default": 10
                    }
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "get_session".to_string(),
            description: "Get the full content of a specific Claude Code session by ID. Returns all messages in the session.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The session ID to retrieve"
                    }
                },
                "required": ["session_id"]
            }),
        },
        Tool {
            name: "get_session_context".to_string(),
            description: "Get a condensed context summary of a Claude Code session, suitable for understanding what was worked on without full message history.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The session ID to get context for"
                    }
                },
                "required": ["session_id"]
            }),
        },
    ]
}

/// Handle an incoming JSON-RPC request
async fn handle_request(request: JsonRpcRequest) -> JsonRpcResponse {
    let id = request.id.clone();

    match request.method.as_str() {
        "initialize" => {
            let result = InitializeResult {
                protocol_version: "2024-11-05".to_string(),
                capabilities: ServerCapabilities {
                    tools: ToolsCapability { list_changed: false },
                },
                server_info: ServerInfo {
                    name: "claude-code-mcp".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
            };
            JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
        }

        "notifications/initialized" | "initialized" => {
            // Notifications don't get responses - but we need to return something
            // Use a special marker that main loop can skip
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: Value::Null,
                result: None,
                error: None,
            };
        }

        "tools/list" => {
            let tools = get_tools();
            JsonRpcResponse::success(id, json!({ "tools": tools }))
        }

        "tools/call" => {
            let params = request.params.unwrap_or(json!({}));
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            let result = handle_tool_call(tool_name, arguments).await;
            JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
        }

        _ => JsonRpcResponse::error(
            id,
            -32601,
            format!("Method not found: {}", request.method),
        ),
    }
}

/// Handle a tool call
async fn handle_tool_call(name: &str, arguments: Value) -> ToolResult {
    let store = match SessionStore::new() {
        Ok(s) => s,
        Err(e) => return ToolResult::error(format!("Failed to initialize session store: {}", e)),
    };

    match name {
        "list_sessions" => {
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;

            match store.list_sessions(limit) {
                Ok(sessions) => {
                    let json = serde_json::to_string_pretty(&sessions)
                        .unwrap_or_else(|_| "[]".to_string());
                    ToolResult::text(json)
                }
                Err(e) => ToolResult::error(format!("Failed to list sessions: {}", e)),
            }
        }

        "search_sessions" => {
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;

            if query.is_empty() {
                return ToolResult::error("Query parameter is required");
            }

            match store.search_sessions(query, limit) {
                Ok(sessions) => {
                    let json = serde_json::to_string_pretty(&sessions)
                        .unwrap_or_else(|_| "[]".to_string());
                    ToolResult::text(json)
                }
                Err(e) => ToolResult::error(format!("Failed to search sessions: {}", e)),
            }
        }

        "get_session" => {
            let session_id = arguments
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if session_id.is_empty() {
                return ToolResult::error("session_id parameter is required");
            }

            match store.get_session(session_id) {
                Ok(Some(session)) => {
                    // Format messages for readability
                    let formatted: Vec<_> = session
                        .messages
                        .iter()
                        .map(|m| {
                            json!({
                                "role": m.role,
                                "content": m.content,
                                "timestamp": m.timestamp
                            })
                        })
                        .collect();

                    let result = json!({
                        "id": session.id,
                        "project_path": session.project_path,
                        "messages": formatted
                    });

                    ToolResult::text(
                        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string()),
                    )
                }
                Ok(None) => ToolResult::error(format!("Session not found: {}", session_id)),
                Err(e) => ToolResult::error(format!("Failed to get session: {}", e)),
            }
        }

        "get_session_context" => {
            let session_id = arguments
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if session_id.is_empty() {
                return ToolResult::error("session_id parameter is required");
            }

            match store.get_session_context(session_id) {
                Ok(Some(context)) => {
                    let json = serde_json::to_string_pretty(&context)
                        .unwrap_or_else(|_| "{}".to_string());
                    ToolResult::text(json)
                }
                Ok(None) => ToolResult::error(format!("Session not found: {}", session_id)),
                Err(e) => ToolResult::error(format!("Failed to get session context: {}", e)),
            }
        }

        _ => ToolResult::error(format!("Unknown tool: {}", name)),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    // MCP servers communicate via JSON-RPC over stdio
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => {
                // Check if this is a notification (no id means notification)
                let is_notification = request.id.is_null() ||
                    request.method.starts_with("notifications/");

                let response = handle_request(request).await;

                // Don't send response for notifications
                if is_notification {
                    continue;
                }

                // Skip empty responses (for notifications that slipped through)
                if response.result.is_none() && response.error.is_none() {
                    continue;
                }

                let response_json = serde_json::to_string(&response)?;
                stdout.write_all(response_json.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            Err(e) => {
                let error = JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {}", e));
                let error_json = serde_json::to_string(&error)?;
                stdout.write_all(error_json.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }
    }

    Ok(())
}
