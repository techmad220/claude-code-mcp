//! End-to-end tests for Claude Code MCP Server
//!
//! Tests the full MCP protocol flow: initialize, list tools, and call tools

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::path::PathBuf;

/// Helper to spawn the MCP server and communicate with it
struct McpTestClient {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl McpTestClient {
    fn new() -> Self {
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-code-mcp"));

        let mut child = Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to spawn MCP server");

        let stdin = child.stdin.take().expect("Failed to get stdin");
        let stdout = BufReader::new(child.stdout.take().expect("Failed to get stdout"));

        Self { child, stdin, stdout }
    }

    fn send_request(&mut self, request: &serde_json::Value) -> serde_json::Value {
        let request_str = serde_json::to_string(request).unwrap();
        writeln!(self.stdin, "{}", request_str).expect("Failed to write request");
        self.stdin.flush().expect("Failed to flush");

        let mut response_line = String::new();
        self.stdout.read_line(&mut response_line).expect("Failed to read response");

        serde_json::from_str(&response_line).expect("Failed to parse response")
    }
}

impl Drop for McpTestClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}


// ===== Protocol Tests =====

#[test]
fn test_initialize() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        }
    });

    let response = client.send_request(&request);

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(response["result"].is_object());
    assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(response["result"]["serverInfo"]["name"], "claude-code-mcp");
}

#[test]
fn test_tools_list() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    });

    let response = client.send_request(&request);

    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["result"]["tools"].is_array());

    let tools = response["result"]["tools"].as_array().unwrap();
    let tool_names: Vec<&str> = tools.iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();

    assert!(tool_names.contains(&"list_sessions"));
    assert!(tool_names.contains(&"search_sessions"));
    assert!(tool_names.contains(&"get_session"));
    assert!(tool_names.contains(&"get_session_context"));
    assert_eq!(tools.len(), 4);
}

#[test]
fn test_tool_schemas() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    });

    let response = client.send_request(&request);
    let tools = response["result"]["tools"].as_array().unwrap();

    // Check list_sessions schema
    let list_sessions = tools.iter().find(|t| t["name"] == "list_sessions").unwrap();
    assert!(list_sessions["inputSchema"]["properties"]["limit"].is_object());

    // Check search_sessions schema requires query
    let search_sessions = tools.iter().find(|t| t["name"] == "search_sessions").unwrap();
    let required = search_sessions["inputSchema"]["required"].as_array().unwrap();
    assert!(required.contains(&serde_json::json!("query")));

    // Check get_session schema requires session_id
    let get_session = tools.iter().find(|t| t["name"] == "get_session").unwrap();
    let required = get_session["inputSchema"]["required"].as_array().unwrap();
    assert!(required.contains(&serde_json::json!("session_id")));
}

#[test]
fn test_unknown_method_error() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "unknown/method"
    });

    let response = client.send_request(&request);

    assert!(response["error"].is_object());
    assert_eq!(response["error"]["code"], -32601);
    assert!(response["error"]["message"].as_str().unwrap().contains("Method not found"));
}

#[test]
fn test_parse_error() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-code-mcp"));

    let mut child = Command::new(&binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn MCP server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Send invalid JSON
    writeln!(stdin, "{{invalid json").unwrap();
    stdin.flush().unwrap();

    let mut response_line = String::new();
    stdout.read_line(&mut response_line).unwrap();

    let response: serde_json::Value = serde_json::from_str(&response_line).unwrap();

    assert!(response["error"].is_object());
    assert_eq!(response["error"]["code"], -32700);
    assert!(response["error"]["message"].as_str().unwrap().contains("Parse error"));

    let _ = child.kill();
}

#[test]
fn test_notification_initialized() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "notifications/initialized"
    });

    let response = client.send_request(&request);

    // Should return success (empty object)
    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["error"].is_null());
}

// ===== Tool Call Tests =====

#[test]
fn test_list_sessions_tool() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "list_sessions",
            "arguments": {"limit": 5}
        }
    });

    let response = client.send_request(&request);

    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["result"]["content"].is_array());
    // Result is either sessions or error about ~/.claude not found
}

#[test]
fn test_search_sessions_tool() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "search_sessions",
            "arguments": {"query": "rust async", "limit": 5}
        }
    });

    let response = client.send_request(&request);

    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["result"]["content"].is_array());
}

#[test]
fn test_search_sessions_missing_query() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "search_sessions",
            "arguments": {}
        }
    });

    let response = client.send_request(&request);

    let content = &response["result"]["content"][0]["text"];
    assert!(content.as_str().unwrap().contains("required"));
    assert!(response["result"]["isError"] == true);
}

#[test]
fn test_get_session_tool() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "get_session",
            "arguments": {"session_id": "nonexistent-session-id"}
        }
    });

    let response = client.send_request(&request);

    assert_eq!(response["jsonrpc"], "2.0");
    // Should return "not found" error
    let content = &response["result"]["content"][0]["text"];
    assert!(content.as_str().unwrap().to_lowercase().contains("not found")
        || content.as_str().unwrap().contains("Failed"));
}

#[test]
fn test_get_session_missing_id() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "get_session",
            "arguments": {}
        }
    });

    let response = client.send_request(&request);

    let content = &response["result"]["content"][0]["text"];
    assert!(content.as_str().unwrap().contains("required"));
    assert!(response["result"]["isError"] == true);
}

#[test]
fn test_get_session_context_tool() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "get_session_context",
            "arguments": {"session_id": "nonexistent-session"}
        }
    });

    let response = client.send_request(&request);

    assert_eq!(response["jsonrpc"], "2.0");
    // Should return error about session not found
    let content = &response["result"]["content"][0]["text"];
    assert!(content.as_str().unwrap().to_lowercase().contains("not found")
        || content.as_str().unwrap().contains("Failed"));
}

#[test]
fn test_unknown_tool() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "nonexistent_tool",
            "arguments": {}
        }
    });

    let response = client.send_request(&request);

    let content = &response["result"]["content"][0]["text"];
    assert!(content.as_str().unwrap().contains("Unknown tool"));
    assert!(response["result"]["isError"] == true);
}

// ===== Multiple Request Tests =====

#[test]
fn test_multiple_requests() {
    let mut client = McpTestClient::new();

    // First: initialize
    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let init_response = client.send_request(&init_request);
    assert!(init_response["result"].is_object());

    // Second: list tools
    let tools_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    let tools_response = client.send_request(&tools_request);
    assert!(tools_response["result"]["tools"].is_array());

    // Third: call a tool
    let call_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "list_sessions",
            "arguments": {"limit": 10}
        }
    });
    let call_response = client.send_request(&call_request);
    assert!(call_response["result"]["content"].is_array());

    // Verify IDs match
    assert_eq!(init_response["id"], 1);
    assert_eq!(tools_response["id"], 2);
    assert_eq!(call_response["id"], 3);
}

#[test]
fn test_null_id_request() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "method": "tools/list"
    });

    let response = client.send_request(&request);

    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["id"].is_null());
    assert!(response["result"]["tools"].is_array());
}

// ===== Server Info Tests =====

#[test]
fn test_server_version() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });

    let response = client.send_request(&request);

    let version = response["result"]["serverInfo"]["version"].as_str().unwrap();
    assert!(!version.is_empty());
    // Should match Cargo.toml version
    assert_eq!(version, "0.1.0");
}

#[test]
fn test_capabilities() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });

    let response = client.send_request(&request);

    let capabilities = &response["result"]["capabilities"];
    assert!(capabilities["tools"].is_object());
    assert_eq!(capabilities["tools"]["listChanged"], false);
}

// ===== Edge Cases =====

#[test]
fn test_empty_arguments() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "list_sessions"
            // No arguments - should use defaults
        }
    });

    let response = client.send_request(&request);

    // Should succeed with default limit
    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["result"]["content"].is_array());
}

#[test]
fn test_large_limit() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "list_sessions",
            "arguments": {"limit": 1000}  // Above max of 100
        }
    });

    let response = client.send_request(&request);

    // Should succeed (limit is capped internally)
    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["result"]["content"].is_array());
}

#[test]
fn test_string_id() {
    let mut client = McpTestClient::new();

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "string-id-123",
        "method": "tools/list"
    });

    let response = client.send_request(&request);

    assert_eq!(response["id"], "string-id-123");
    assert!(response["result"]["tools"].is_array());
}
