//! MCP (Model Context Protocol) client integration.
//!
//! Spawns external MCP server processes, discovers their tools,
//! and makes them available to the agent through the standard `Tool` trait.
//!
//! Protocol: JSON-RPC 2.0 over stdio transport, MCP spec 2025-06-18.

use crate::tools::ToolRegistry;
use crate::types::{McpServerConfig, ToolDef, ToolError};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Timeout for the initialize handshake + tools/list discovery.
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// JSON-RPC version string used in every request.
const JSONRPC_VERSION: &str = "2.0";

/// Protocol version we negotiate with MCP servers.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

// ---------------------------------------------------------------------------
// McpClientManager
// ---------------------------------------------------------------------------

/// Manages the lifecycle of all configured MCP server connections.
///
/// Dropping the manager kills all child processes (via `kill_on_drop(true)`).
pub struct McpClientManager {
    clients: Vec<McpClient>,
}

impl McpClientManager {
    /// Start all configured MCP servers, run the handshake, and discover their tools.
    ///
    /// Servers that fail to start or handshake are silently skipped (a warning is
    /// logged). Returns `None` when the config list is empty.
    pub async fn start(
        servers: &[McpServerConfig],
        tool_timeout_secs: u64,
    ) -> Option<Self> {
        if servers.is_empty() {
            return None;
        }

        let mut clients = Vec::with_capacity(servers.len());

        for cfg in servers {
            match McpClient::connect(cfg, tool_timeout_secs).await {
                Ok(client) => {
                    tracing::info!(
                        server = %cfg.name,
                        tool_count = client.tools.len(),
                        "MCP server connected"
                    );
                    clients.push(client);
                }
                Err(e) => {
                    tracing::warn!(
                        server = %cfg.name,
                        error = %e,
                        "Failed to connect to MCP server — skipping"
                    );
                }
            }
        }

        if clients.is_empty() {
            return None;
        }

        Some(Self { clients })
    }

    /// Register all discovered MCP tools into the given `ToolRegistry`.
    pub fn register_all(&self, registry: &mut ToolRegistry) {
        for client in &self.clients {
            let shared = Arc::new(Mutex::new(client.clone_channel()));
            for tool in &client.tools {
                let adapter = McpToolAdapter {
                    server_name: client.name.clone(),
                    tool_name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                    client: shared.clone(),
                };
                registry.register(adapter);
            }
        }
    }

    /// Return the number of connected servers.
    #[cfg(test)]
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    /// Return the total number of discovered tools across all servers.
    #[cfg(test)]
    pub fn tool_count(&self) -> usize {
        self.clients.iter().map(|c| c.tools.len()).sum()
    }
}

// ---------------------------------------------------------------------------
// McpClient
// ---------------------------------------------------------------------------

/// A live connection to a single MCP server over stdio.
struct McpClient {
    /// Server name from config.
    name: String,
    /// Tools discovered via `tools/list`.
    tools: Vec<ToolDef>,
    /// Command and args to re-spawn channel-clone connections.
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// Per-call timeout for tool execution.
    tool_timeout_secs: u64,
}

impl McpClient {
    /// Spawn the server, perform initialize handshake, and discover tools.
    async fn connect(
        cfg: &McpServerConfig,
        tool_timeout_secs: u64,
    ) -> Result<Self, McpError> {
        let env: Vec<(String, String)> = cfg
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Spawn a temporary connection just for the handshake + tools/list.
        // We'll re-spawn per-call connections via clone_channel() to keep
        // the implementation simple and avoid shared-state complexity.
        let mut child = spawn_server(&cfg.command, &cfg.args, &env)?;

        // Perform handshake with timeout.
        tokio::time::timeout(
            Duration::from_secs(CONNECT_TIMEOUT_SECS),
            Self::handshake(&mut child),
        )
        .await
        .map_err(|_| McpError::Timeout("MCP handshake timed out".into()))??;

        let tools = tokio::time::timeout(
            Duration::from_secs(CONNECT_TIMEOUT_SECS),
            Self::list_tools(&mut child),
        )
        .await
        .map_err(|_| McpError::Timeout("MCP tools/list timed out".into()))??;

        // Kill the handshake process — each tool call spawns its own.
        // kill_on_drop(true) will clean up when child is dropped.
        drop(child);

        if tools.is_empty() {
            return Err(McpError::Protocol(
                "MCP server returned no tools".into(),
            ));
        }

        Ok(Self {
            name: cfg.name.clone(),
            tools,
            command: cfg.command.clone(),
            args: cfg.args.clone(),
            env,
            tool_timeout_secs,
        })
    }

    /// Clone the spawn parameters so each `tools/call` gets its own process.
    /// This avoids shared mutable state on the stdio pipes.
    fn clone_channel(&self) -> McpClientChannel {
        McpClientChannel {
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            timeout_secs: self.tool_timeout_secs,
        }
    }

    // ---- Protocol methods (operate on a live spawned child) ----

    async fn handshake(child: &mut McpChild) -> Result<(), McpError> {
        // 1. Send initialize
        let init_id = child.next_id();
        let init_req = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "clausura",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        });

        child.send(&init_req).await?;
        let _resp: Value = child.recv_response(init_id).await?;

        // 2. Send notifications/initialized (no response expected)
        let notif = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "method": "notifications/initialized",
        });
        child.send(&notif).await?;

        Ok(())
    }

    async fn list_tools(child: &mut McpChild) -> Result<Vec<ToolDef>, McpError> {
        let id = child.next_id();
        let req = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": "tools/list",
            "params": {}
        });

        child.send(&req).await?;
        let resp = child.recv_response(id).await?;

        let tools_array = resp["result"]["tools"]
            .as_array()
            .ok_or_else(|| McpError::Protocol("tools/list: missing 'tools' array".into()))?;

        let mut tools = Vec::with_capacity(tools_array.len());
        for t in tools_array {
            let name = t["name"]
                .as_str()
                .ok_or_else(|| McpError::Protocol("tool missing 'name'".into()))?
                .to_string();
            let description = t["description"].as_str().unwrap_or("").to_string();
            let parameters = t.get("inputSchema").cloned().unwrap_or_else(|| {
                serde_json::json!({"type": "object", "properties": {}})
            });

            tools.push(ToolDef {
                name,
                description,
                parameters,
            });
        }

        Ok(tools)
    }

    /// Call a tool on a re-spawned connection with a timeout.
    /// Each call spawns a fresh process, runs the full handshake, then
    /// sends the single `tools/call` request.
    async fn call_tool(
        channel: &McpClientChannel,
        tool_name: &str,
        args: Value,
    ) -> Result<String, McpError> {
        let mut child = spawn_server(&channel.command, &channel.args, &channel.env)?;

        // Run handshake on the new connection.
        Self::handshake(&mut child).await?;

        let id = child.next_id();
        let req = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            }
        });

        child.send(&req).await?;

        let result = tokio::time::timeout(
            Duration::from_secs(channel.timeout_secs),
            child.recv_response(id),
        )
        .await
        .map_err(|_| McpError::Timeout(format!(
            "MCP tool '{}' timed out after {}s",
            tool_name, channel.timeout_secs
        )))??;

        // Extract the tool result content.
        // MCP spec: result is { content: [{ type: "text", text: "..." }] }
        let content = &result["result"]["content"];
        let text = extract_text_content(content).unwrap_or_else(|| result.to_string());

        Ok(text)
    }
}

/// Extract text from MCP tool result content array.
fn extract_text_content(content: &Value) -> Option<String> {
    let items = content.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if item["type"].as_str() == Some("text") {
            if let Some(text) = item["text"].as_str() {
                parts.push(text.to_string());
            }
        }
    }
    if parts.is_empty() {
        // Fallback: return the raw JSON as a string
        None
    } else {
        Some(parts.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// McpClientChannel
// ---------------------------------------------------------------------------

/// Lightweight clone of the spawn parameters, used to create a fresh
/// process for each tool call.
#[derive(Clone)]
struct McpClientChannel {
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    timeout_secs: u64,
}

// ---------------------------------------------------------------------------
// McpChild — wraps a running server process with JSON-RPC helpers
// ---------------------------------------------------------------------------

/// A live MCP server child process with stdio pipes.
struct McpChild {
    #[allow(dead_code)]
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
}

impl McpChild {
    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn send(&mut self, msg: &Value) -> Result<(), McpError> {
        let mut line = serde_json::to_string(msg)
            .map_err(|e| McpError::Protocol(format!("JSON serialize: {e}")))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        Ok(())
    }

    async fn recv_response(&mut self, expected_id: u64) -> Result<Value, McpError> {
        // Read lines until we get a JSON-RPC response (not a notification).
        loop {
            let line = self
                .stdout_lines
                .next_line()
                .await
                .map_err(|e| McpError::Io(e.to_string()))?
                .ok_or_else(|| McpError::Io("MCP server closed stdout".into()))?;

            let msg: Value = serde_json::from_str(&line)
                .map_err(|e| McpError::Protocol(format!("JSON parse: {e} — line: {line}")))?;

            // Skip notifications (no "id" field).
            if msg.get("id").is_none() {
                continue;
            }

            let msg_id = msg["id"].as_u64().ok_or_else(|| {
                McpError::Protocol("JSON-RPC response missing numeric 'id'".into())
            })?;

            if msg_id != expected_id {
                // This shouldn't normally happen with a fresh connection per call.
                return Err(McpError::Protocol(format!(
                    "Unexpected response id: got {msg_id}, expected {expected_id}"
                )));
            }

            // Check for JSON-RPC error.
            if let Some(err) = msg.get("error") {
                let code = err["code"].as_i64().unwrap_or(-1);
                let message = err["message"].as_str().unwrap_or("unknown error");
                return Err(McpError::RpcError {
                    code,
                    message: message.to_string(),
                });
            }

            return Ok(msg);
        }
    }
}

/// Spawn an MCP server process with stdin/stdout piped, stderr inherited.
fn spawn_server(
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> Result<McpChild, McpError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::inherit());
    cmd.kill_on_drop(true);

    // Build a minimal environment similar to shell_exec's approach.
    cmd.env_clear();
    let mut path = "/usr/local/bin:/usr/bin:/bin".to_string();
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_bin = std::path::Path::new(&home).join(".cargo/bin");
        if cargo_bin.is_dir() {
            path = format!("{path}:{}", cargo_bin.display());
        }
    }
    cmd.env("PATH", &path);
    for name in ["HOME", "TERM", "LANG", "TMPDIR"] {
        if let Ok(val) = std::env::var(name) {
            cmd.env(name, val);
        }
    }
    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| McpError::Io(format!("Failed to spawn '{}': {e}", command)))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpError::Io("No stdin pipe".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| McpError::Io("No stdout pipe".into()))?;

    let stdout_lines = BufReader::new(stdout).lines();

    Ok(McpChild {
        child,
        stdin,
        stdout_lines,
        next_id: 0,
    })
}

// ---------------------------------------------------------------------------
// McpToolAdapter
// ---------------------------------------------------------------------------

/// Wraps an MCP tool so it implements the local `Tool` trait.
///
/// Each `execute` call spawns a fresh MCP server process, sends a single
/// `tools/call` request, and returns the result. This keeps the implementation
/// simple and avoids shared mutable state on stdio pipes.
struct McpToolAdapter {
    server_name: String,
    tool_name: String,
    description: String,
    parameters: Value,
    client: Arc<Mutex<McpClientChannel>>,
}

#[async_trait]
impl crate::tools::Tool for McpToolAdapter {
    fn name(&self) -> &str {
        // Leak a static string to satisfy the lifetime requirement.
        // The tool name is stable for the lifetime of the adapter.
        // We use Box::leak here because Tool::name() returns &str with no
        // lifetime parameter, and we need to construct the name dynamically.
        // Since the adapter lives for the duration of the task, leaking is
        // acceptable.
        &*Box::leak(
            format!("mcp__{}__{}", self.server_name, self.tool_name).into_boxed_str(),
        )
    }

    fn description(&self) -> &str {
        &*Box::leak(
            format!(
                "[MCP:{}] {}",
                self.server_name, self.description
            )
            .into_boxed_str(),
        )
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let channel = {
            let guard = self.client.lock().await;
            guard.clone()
        };

        match McpClient::call_tool(&channel, &self.tool_name, args).await {
            Ok(output) => Ok(output),
            Err(McpError::Io(msg)) => Err(ToolError::ExecutionFailed(format!(
                "MCP tool '{}': I/O error: {msg}",
                self.tool_name
            ))),
            Err(McpError::Protocol(msg)) => Err(ToolError::ExecutionFailed(format!(
                "MCP tool '{}': protocol error: {msg}",
                self.tool_name
            ))),
            Err(McpError::Timeout(msg)) => Err(ToolError::ExecutionFailed(format!(
                "MCP tool '{}': {msg}",
                self.tool_name
            ))),
            Err(McpError::RpcError { code, message }) => {
                Err(ToolError::ExecutionFailed(format!(
                    "MCP tool '{}': RPC error [{code}]: {message}",
                    self.tool_name
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum McpError {
    Io(String),
    Protocol(String),
    Timeout(String),
    RpcError { code: i64, message: String },
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Io(msg) => write!(f, "I/O: {msg}"),
            McpError::Protocol(msg) => write!(f, "protocol: {msg}"),
            McpError::Timeout(msg) => write!(f, "timeout: {msg}"),
            McpError::RpcError { code, message } => {
                write!(f, "RPC error [{code}]: {message}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRegistry;
    use serde_json::json;
    use std::io::Write;
    use tempfile::TempDir;

    /// Write a configurable test MCP server in Python.
    ///
    /// `mode` determines the server's behavior:
    /// - `normal`     : standard handshake + tools/list + tools/call loop
    /// - `empty-tools`: tools/list returns `{{"tools": []}}`
    /// - `error-list` : tools/list returns a JSON-RPC error
    /// - `wrong-id`   : tools/list response uses a mismatched id
    /// - `call-error` : tools/call returns a JSON-RPC error
    /// - `call-crash` : exit *after* handshake but *before* sending tools/call result
    /// - `echo-env`   : tools/call reads an env var and echoes it
    fn write_mcp_server(
        dir: &TempDir,
        script_name: &str,
        tool_name: &str,
        tool_desc: &str,
        _mode: &str,
    ) -> std::path::PathBuf {
        let script_path = dir.path().join(script_name);
        let script = format!(
            r#"#!/usr/bin/env python3
import sys, json, os

mode = sys.argv[1] if len(sys.argv) > 1 else 'normal'
tool_name = sys.argv[2] if len(sys.argv) > 2 else 'tool'
tool_desc = sys.argv[3] if len(sys.argv) > 3 else 'desc'

def send(msg):
    sys.stdout.write(json.dumps(msg) + '\n')
    sys.stdout.flush()

def recv():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)

# === MCP protocol loop (each connection triggers one session) ===
while True:
    # ---- initialize ----
    req = recv()
    send({{"jsonrpc":"2.0","id":req['id'],"result":{{"protocolVersion":"2025-06-18","capabilities":{{}},"serverInfo":{{"name":"test","version":"0.1"}}}}}})

    # ---- notifications/initialized ----
    req = recv()

    # ---- actual request: tools/list or tools/call ----
    req = recv()
    method = req['method']
    rid = req['id']

    if method == 'tools/list':
        if mode == 'empty-tools':
            send({{"jsonrpc":"2.0","id":rid,"result":{{"tools":[]}}}})
        elif mode == 'error-list':
            send({{"jsonrpc":"2.0","id":rid,"error":{{"code":-32601,"message":"List not supported"}}}})
        elif mode == 'wrong-id':
            send({{"jsonrpc":"2.0","id":999,"result":{{"tools":[{{"name":"{tool_name}","description":"{tool_desc}","inputSchema":{{"type":"object","properties":{{}}}}}}]}}}})
        else:  # normal
            send({{"jsonrpc":"2.0","id":rid,"result":{{"tools":[{{"name":"{tool_name}","description":"{tool_desc}","inputSchema":{{"type":"object","properties":{{}}}}}}]}}}})

    elif method == 'tools/call':
        if mode == 'call-error':
            send({{"jsonrpc":"2.0","id":rid,"error":{{"code":-32602,"message":"Invalid params"}}}})
        elif mode == 'call-crash':
            sys.exit(0)
        elif mode == 'echo-env':
            val = os.environ.get('MCP_TEST_VAR', '(unset)')
            send({{"jsonrpc":"2.0","id":rid,"result":{{"content":[{{"type":"text","text":val}}]}}}})
        else:
            tool_args = req['params']['arguments']
            send({{"jsonrpc":"2.0","id":rid,"result":{{"content":[{{"type":"text","text":json.dumps(tool_args)}}]}}}})
    else:
        send({{"jsonrpc":"2.0","id":rid,"error":{{"code":-32601,"message":"Unknown method"}}}})
"#
        );
        let mut file = std::fs::File::create(&script_path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = file.metadata().unwrap().permissions();
            perms.set_mode(0o755);
            file.set_permissions(perms).unwrap();
        }
        script_path
    }

    /// Shorthand: create a `McpServerConfig` pointing at a Python test server.
    fn test_server_cfg(
        dir: &TempDir,
        name: &str,
        tool_name: &str,
        tool_desc: &str,
        mode: &str,
    ) -> McpServerConfig {
        let script = write_mcp_server(dir, &format!("server-{name}"), tool_name, tool_desc, mode);
        McpServerConfig {
            name: name.into(),
            command: "python3".into(),
            args: vec![
                script.to_string_lossy().to_string(),
                mode.into(),
                tool_name.into(),
                tool_desc.into(),
            ],
            env: std::collections::HashMap::new(),
        }
    }

    // ── Positive paths ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_mcp_connect_and_discover_tools() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_server_cfg(&tmp, "test-server", "hello", "Say hello", "normal");

        let manager = McpClientManager::start(&[cfg], 30)
            .await
            .expect("manager should start");
        assert_eq!(manager.server_count(), 1);
        assert_eq!(manager.tool_count(), 1);

        let mut registry = ToolRegistry::new();
        manager.register_all(&mut registry);
        let defs = registry.list_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "mcp__test-server__hello");
        assert!(defs[0].description.contains("Say hello"));
    }

    #[tokio::test]
    async fn test_mcp_tool_execute() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_server_cfg(&tmp, "echo-srv", "echo", "Echo arguments", "normal");

        let manager = McpClientManager::start(&[cfg], 30)
            .await
            .expect("manager should start");

        let mut registry = ToolRegistry::new();
        manager.register_all(&mut registry);

        let tool = registry
            .get("mcp__echo-srv__echo")
            .expect("tool should exist");
        let result = tool
            .execute(json!({"message": "hello world"}))
            .await
            .expect("tool execute should succeed");
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["message"], "hello world");
    }

    #[tokio::test]
    async fn test_mcp_multiple_servers() {
        let tmp = TempDir::new().unwrap();
        let cfg_a = test_server_cfg(&tmp, "srv-a", "tool-a", "Tool A", "normal");
        let cfg_b = test_server_cfg(&tmp, "srv-b", "tool-b", "Tool B", "normal");

        let manager = McpClientManager::start(&[cfg_a, cfg_b], 30)
            .await
            .expect("manager should start");
        assert_eq!(manager.server_count(), 2);
        assert_eq!(manager.tool_count(), 2);

        let mut registry = ToolRegistry::new();
        manager.register_all(&mut registry);
        let defs = registry.list_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"mcp__srv-a__tool-a"));
        assert!(names.contains(&"mcp__srv-b__tool-b"));
    }

    #[tokio::test]
    async fn test_mcp_empty_servers_list() {
        let manager = McpClientManager::start(&[], 30).await;
        assert!(manager.is_none());
    }

    #[tokio::test]
    async fn test_mcp_custom_env() {
        let tmp = TempDir::new().unwrap();
        let script = write_mcp_server(&tmp, "env-server", "getenv", "Read env var", "echo-env");
        let mut env = std::collections::HashMap::new();
        env.insert("MCP_TEST_VAR".into(), "from-env".into());

        let cfg = McpServerConfig {
            name: "env-srv".into(),
            command: "python3".into(),
            args: vec![
                script.to_string_lossy().to_string(),
                "echo-env".into(),
                "getenv".into(),
                "Read env var".into(),
            ],
            env,
        };

        let manager = McpClientManager::start(&[cfg], 30)
            .await
            .expect("manager should start");
        let mut registry = ToolRegistry::new();
        manager.register_all(&mut registry);

        let tool = registry.get("mcp__env-srv__getenv").unwrap();
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result, "from-env");
    }

    // ── Error paths — McpClientManager.connect() ────────────────────────────

    #[tokio::test]
    async fn test_mcp_empty_tools_list() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_server_cfg(&tmp, "empty", "x", "x", "empty-tools");
        let manager = McpClientManager::start(&[cfg], 30).await;
        assert!(manager.is_none(), "empty tools list → should be skipped");
    }

    #[tokio::test]
    async fn test_mcp_error_on_tools_list() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_server_cfg(&tmp, "err-list", "x", "x", "error-list");
        let manager = McpClientManager::start(&[cfg], 30).await;
        assert!(manager.is_none(), "JSON-RPC error → should be skipped");
    }

    #[tokio::test]
    async fn test_mcp_wrong_response_id() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_server_cfg(&tmp, "bad-id", "x", "x", "wrong-id");
        let manager = McpClientManager::start(&[cfg], 30).await;
        assert!(manager.is_none(), "wrong response id → should be skipped");
    }

    #[tokio::test]
    async fn test_mcp_server_not_found_command() {
        let cfg = McpServerConfig {
            name: "ghost".into(),
            command: "nonexistent-command-xyz-12345".into(),
            args: vec![],
            env: std::collections::HashMap::new(),
        };

        let manager = McpClientManager::start(&[cfg], 30).await;
        assert!(manager.is_none(), "should be None for failed server");
    }

    // ── Error paths — McpToolAdapter.execute() ──────────────────────────────

    #[tokio::test]
    async fn test_mcp_rpc_error_on_tool_call() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_server_cfg(&tmp, "err-call", "fail", "Fails", "call-error");

        let manager = McpClientManager::start(&[cfg], 30)
            .await
            .expect("manager should start (discovery phase is normal)");
        let mut registry = ToolRegistry::new();
        manager.register_all(&mut registry);

        let tool = registry.get("mcp__err-call__fail").expect("tool exists");
        let err = tool
            .execute(json!({"x": 1}))
            .await
            .expect_err("expected RPC error");

        let msg = format!("{err}");
        assert!(
            msg.contains("RPC error"),
            "expected RPC error, got: {msg}"
        );
        assert!(msg.contains("-32602"), "error code missing: {msg}");
    }

    #[tokio::test]
    async fn test_mcp_io_error_on_tool_call() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_server_cfg(&tmp, "crash", "boom", "Crashes", "call-crash");

        let manager = McpClientManager::start(&[cfg], 30)
            .await
            .expect("manager should start (discovery phase is normal)");
        let mut registry = ToolRegistry::new();
        manager.register_all(&mut registry);

        let tool = registry.get("mcp__crash__boom").expect("tool exists");
        let err = tool
            .execute(json!({}))
            .await
            .expect_err("expected I/O error");

        let msg = format!("{err}");
        assert!(
            msg.contains("I/O error") || msg.contains("closed stdout"),
            "expected I/O error, got: {msg}"
        );
    }

    // ── extract_text_content edge cases (pure Rust, no server) ──────────────

    #[test]
    fn test_extract_text_content_basic() {
        let content = json!([{"type": "text", "text": "hello"}]);
        assert_eq!(extract_text_content(&content), Some("hello".into()));
    }

    #[test]
    fn test_extract_text_content_multiple_items() {
        let content = json!([
            {"type": "text", "text": "part1"},
            {"type": "text", "text": "part2"}
        ]);
        assert_eq!(extract_text_content(&content), Some("part1\npart2".into()));
    }

    #[test]
    fn test_extract_text_content_non_text_items() {
        // Only non-text items → None (fallback to raw JSON)
        let content = json!([
            {"type": "resource", "resource": {"text": "data"}}
        ]);
        assert_eq!(extract_text_content(&content), None);
    }

    #[test]
    fn test_extract_text_content_mixed_types() {
        // Only text items are collected; non-text items are ignored.
        let content = json!([
            {"type": "text", "text": "result"},
            {"type": "resource", "resource": {"text": "ignored"}}
        ]);
        assert_eq!(extract_text_content(&content), Some("result".into()));
    }

    #[test]
    fn test_extract_text_content_empty_array() {
        let content = json!([]);
        assert_eq!(extract_text_content(&content), None);
    }

    #[test]
    fn test_extract_text_content_not_an_array() {
        let content = json!("just a string");
        assert_eq!(extract_text_content(&content), None);
    }

    #[test]
    fn test_extract_text_content_missing_text_field() {
        let content = json!([{"type": "text"}]);
        // Missing "text" key → contributes nothing → empty parts → None
        assert_eq!(extract_text_content(&content), None);
    }
}
