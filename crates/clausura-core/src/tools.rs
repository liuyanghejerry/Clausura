use crate::types::ToolDef;
use crate::types::ToolError;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Tool trait
// ---------------------------------------------------------------------------

/// A tool that the agent can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (used by LLM to invoke)
    fn name(&self) -> &str;
    /// Description of what the tool does
    fn description(&self) -> &str;
    /// JSON Schema for tool parameters
    fn parameters(&self) -> Value;
    /// Execute the tool with given arguments
    async fn execute(&self, args: Value) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// Registry of available tools for the agent.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
    }

    /// Get a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Get all tool definitions for LLM function calling.
    pub fn list_definitions(&self) -> Vec<ToolDef> {
        self.tools
            .values()
            .map(|t| ToolDef {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ReadFileTool
// ---------------------------------------------------------------------------

/// Reads a file relative to the workspace root. Path traversal is rejected.
pub struct ReadFileTool {
    workspace_root: PathBuf,
}

impl ReadFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        // Canonicalize workspace root once so symlinks are resolved consistently
        let canonical_root = workspace_root.canonicalize().unwrap_or(workspace_root);
        Self {
            workspace_root: canonical_root,
        }
    }

    fn resolve_path(&self, path_str: &str) -> Result<PathBuf, ToolError> {
        let requested = Path::new(path_str);
        // Reject absolute paths
        if requested.is_absolute() {
            return Err(ToolError::SandboxViolation(format!(
                "Absolute paths not allowed: {}",
                path_str
            )));
        }
        // Reject paths with ..
        if requested.components().any(|c| c.as_os_str() == "..") {
            return Err(ToolError::SandboxViolation(format!(
                "Path traversal not allowed: {}",
                path_str
            )));
        }
        // Canonicalize to prevent symlink-based escapes
        let full_path = self.workspace_root.join(requested);
        let canonical = full_path
            .canonicalize()
            .map_err(|_| ToolError::ExecutionFailed(format!("File not found: {}", path_str)))?;
        // Verify we're still inside workspace root
        if !canonical.starts_with(&self.workspace_root) {
            return Err(ToolError::SandboxViolation(
                "Path escapes workspace root".into(),
            ));
        }
        Ok(canonical)
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Path is relative to the workspace root."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path relative to workspace root"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path_str = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::ExecutionFailed("Missing 'path' argument".into()))?;
        let resolved = self.resolve_path(path_str)?;
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Read error: {}", e)))?;
        Ok(content)
    }
}

// ---------------------------------------------------------------------------
// GitDiffTool
// ---------------------------------------------------------------------------

/// Runs git diff to get code changes.
pub struct GitDiffTool {
    workspace_root: PathBuf,
}

impl GitDiffTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    async fn run_git(&self, args: &[&str]) -> Result<String, ToolError> {
        let output = tokio::process::Command::new("git")
            .current_dir(&self.workspace_root)
            .args(args)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Git error: {}", e)))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(ToolError::ExecutionFailed(format!(
                "Git failed: {}",
                stderr
            )))
        }
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn description(&self) -> &str {
        "Get the git diff. Use with 'base' to diff against a branch, or 'staged' for staged changes only."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "base": {
                    "type": "string",
                    "description": "Base ref to diff against (e.g., HEAD~1, main)"
                },
                "staged": {
                    "type": "boolean",
                    "description": "Show staged changes only"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let base = args["base"].as_str();
        let staged = args["staged"].as_bool().unwrap_or(false);

        let git_args: &[&str] = if let Some(base_ref) = base {
            &["diff", base_ref]
        } else if staged {
            &["diff", "--cached"]
        } else {
            &["diff"]
        };

        self.run_git(git_args).await
    }
}

// ---------------------------------------------------------------------------
// ShellExecTool
// ---------------------------------------------------------------------------

/// Executes shell commands from a configured allowlist.
pub struct ShellExecTool {
    workspace_root: PathBuf,
    allowlist: Vec<String>,
}

impl ShellExecTool {
    pub fn new(workspace_root: PathBuf, allowlist: Vec<String>) -> Self {
        Self {
            workspace_root,
            allowlist,
        }
    }

    fn check_allowed(&self, command: &str) -> Result<(), ToolError> {
        let cmd_name = command.split_whitespace().next().unwrap_or("");
        if self.allowlist.is_empty() {
            return Err(ToolError::SandboxViolation(
                "No commands allowed (empty allowlist)".into(),
            ));
        }
        if !self.allowlist.contains(&cmd_name.to_string()) {
            return Err(ToolError::SandboxViolation(format!(
                "Command not in allowlist: {}. Allowed: {:?}",
                cmd_name, self.allowlist
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for ShellExecTool {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command from the allowed list (git, grep, find, wc)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| ToolError::ExecutionFailed("Missing 'command' argument".into()))?;

        self.check_allowed(command)?;

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.workspace_root)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Shell error: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            Ok(stdout.to_string())
        } else {
            // Return stderr as the output even on failure (tool result, not error)
            Ok(format!(
                "Exit code: {}\nStderr: {}",
                output.status.code().unwrap_or(-1),
                stderr
            ))
        }
    }
}

/// Create a default set of tools for the given workspace root.
pub fn default_tools(workspace_root: PathBuf) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(ReadFileTool::new(workspace_root.clone()));
    registry.register(GitDiffTool::new(workspace_root.clone()));
    registry.register(ShellExecTool::new(
        workspace_root,
        vec!["git".into(), "grep".into(), "find".into(), "wc".into()],
    ));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_workspace() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        (tmp, path)
    }

    // -----------------------------------------------------------------------
    // ReadFileTool tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_read_file_success() {
        let (_tmp, root) = setup_workspace();
        let test_file = root.join("test.txt");
        std::fs::write(&test_file, "hello world").unwrap();

        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt"}))
            .await
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn test_read_file_rejects_traversal() {
        let (_tmp, root) = setup_workspace();
        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "../etc/passwd"}))
            .await;
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
    }

    #[tokio::test]
    async fn test_read_file_rejects_absolute() {
        let (_tmp, root) = setup_workspace();
        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "/etc/passwd"}))
            .await;
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
    }

    #[tokio::test]
    async fn test_read_file_missing_path_arg() {
        let (_tmp, root) = setup_workspace();
        let tool = ReadFileTool::new(root);
        let result = tool.execute(serde_json::json!({})).await;
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let (_tmp, root) = setup_workspace();
        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "nonexistent.txt"}))
            .await;
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    // -----------------------------------------------------------------------
    // GitDiffTool tests
    // -----------------------------------------------------------------------

    async fn init_git_repo(root: &Path) {
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(root)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .output()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_git_diff_basic() {
        let (_tmp, root) = setup_workspace();
        init_git_repo(&root).await;

        std::fs::write(root.join("file.txt"), "v1").unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&root)
            .output()
            .await
            .unwrap();

        std::fs::write(root.join("file.txt"), "v2").unwrap();

        let tool = GitDiffTool::new(root);
        let result = tool
            .execute(serde_json::json!({"staged": false}))
            .await
            .unwrap();
        assert!(
            result.contains("v1") || result.contains("v2") || result.contains("file.txt"),
            "Expected diff to mention changes, got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_git_diff_with_base() {
        let (_tmp, root) = setup_workspace();
        init_git_repo(&root).await;

        std::fs::write(root.join("file.txt"), "v1").unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&root)
            .output()
            .await
            .unwrap();

        std::fs::write(root.join("file.txt"), "v2").unwrap();

        let tool = GitDiffTool::new(root);
        let result = tool
            .execute(serde_json::json!({"base": "HEAD"}))
            .await
            .unwrap();
        assert!(!result.is_empty(), "Expected non-empty diff against HEAD");
    }

    #[tokio::test]
    async fn test_git_diff_staged() {
        let (_tmp, root) = setup_workspace();
        init_git_repo(&root).await;

        std::fs::write(root.join("file.txt"), "v1").unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&root)
            .output()
            .await
            .unwrap();

        std::fs::write(root.join("file.txt"), "v2").unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .output()
            .await
            .unwrap();

        let tool = GitDiffTool::new(root);
        let result = tool
            .execute(serde_json::json!({"staged": true}))
            .await
            .unwrap();
        assert!(
            result.contains("v1") || result.contains("v2"),
            "Expected staged diff to mention file content, got: {}",
            result
        );
    }

    // -----------------------------------------------------------------------
    // ShellExecTool tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_shell_exec_allowed_command() {
        let (_tmp, root) = setup_workspace();
        let allowlist = vec!["git".into(), "grep".into()];
        let tool = ShellExecTool::new(root, allowlist);

        let result = tool
            .execute(serde_json::json!({"command": "git status"}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_shell_exec_denied_command() {
        let (_tmp, root) = setup_workspace();
        let allowlist = vec!["git".into(), "grep".into()];
        let tool = ShellExecTool::new(root, allowlist);

        let result = tool
            .execute(serde_json::json!({"command": "rm -rf /"}))
            .await;
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
    }

    #[tokio::test]
    async fn test_shell_exec_empty_allowlist() {
        let (_tmp, root) = setup_workspace();
        let allowlist: Vec<String> = vec![];
        let tool = ShellExecTool::new(root, allowlist);

        let result = tool.execute(serde_json::json!({"command": "ls"})).await;
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
    }

    #[tokio::test]
    async fn test_shell_exec_missing_arg() {
        let (_tmp, root) = setup_workspace();
        let allowlist = vec!["git".into()];
        let tool = ShellExecTool::new(root, allowlist);

        let result = tool.execute(serde_json::json!({})).await;
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    // -----------------------------------------------------------------------
    // ToolRegistry tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_tool_registry_register_and_get() {
        let (_tmp, root) = setup_workspace();
        let mut registry = ToolRegistry::new();
        registry.register(ReadFileTool::new(root));

        let defs = registry.list_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "read_file");

        let tool = registry.get("read_file");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "read_file");

        let missing = registry.get("nonexistent");
        assert!(missing.is_none());
    }

    #[test]
    fn test_tool_registry_multiple_tools() {
        let (_tmp, root) = setup_workspace();
        let mut registry = ToolRegistry::new();
        registry.register(ReadFileTool::new(root.clone()));
        registry.register(GitDiffTool::new(root));

        let defs = registry.list_definitions();
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"git_diff"));
    }

    // -----------------------------------------------------------------------
    // default_tools tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_tools_contains_all() {
        let (_tmp, root) = setup_workspace();
        let registry = default_tools(root);
        let defs = registry.list_definitions();
        assert_eq!(defs.len(), 3);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"git_diff"));
        assert!(names.contains(&"shell_exec"));
    }

    #[test]
    fn test_tool_registry_default() {
        let registry: ToolRegistry = Default::default();
        let defs = registry.list_definitions();
        assert!(defs.is_empty());
    }
}
