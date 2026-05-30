use crate::types::ToolDef;
use crate::types::ToolError;
use async_trait::async_trait;
use regex_lite::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
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

/// Resolve a path relative to the workspace root, enforcing sandbox restrictions.
/// Returns the canonicalized absolute path, or a ToolError.
pub fn resolve_sandboxed_path(
    workspace_root: &Path,
    path_str: &str,
) -> Result<PathBuf, ToolError> {
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
    let full_path = workspace_root.join(requested);
    let canonical = full_path
        .canonicalize()
        .map_err(|_| ToolError::ExecutionFailed(format!("File not found: {}", path_str)))?;
    // Verify we're still inside workspace root
    if !canonical.starts_with(workspace_root) {
        return Err(ToolError::SandboxViolation(
            "Path escapes workspace root".into(),
        ));
    }
    Ok(canonical)
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
        resolve_sandboxed_path(&self.workspace_root, path_str)
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
                },
                "offset": {
                    "type": "integer",
                    "description": "1-based starting line (default: 1, read from start)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max lines to read (default: read to end)"
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

        let offset = args["offset"].as_u64().unwrap_or(1) as usize;
        let limit = args["limit"].as_u64().map(|l| l as usize);

        // offset is 1-based: offset=1 means no skip
        let lines: Vec<&str> = content.lines().skip(offset.saturating_sub(1)).collect();

        let result = if let Some(limit) = limit {
            if limit == 0 {
                String::new()
            } else {
                lines.into_iter().take(limit).collect::<Vec<_>>().join("\n")
            }
        } else {
            lines.join("\n")
        };

        Ok(result)
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
        "Execute a shell command from the allowed list."
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

// ---------------------------------------------------------------------------
// ListFilesTool
// ---------------------------------------------------------------------------

/// List files and directories within the workspace.
pub struct ListFilesTool {
    workspace_root: PathBuf,
}

/// Simple glob-like filename matching.
fn matches_glob(filename: &str, glob: &str) -> bool {
    if let Some(inner) = glob.strip_prefix('*').and_then(|s| s.strip_suffix('*')) {
        filename.contains(inner)
    } else if let Some(suffix) = glob.strip_prefix('*') {
        filename.ends_with(suffix)
    } else if let Some(prefix) = glob.strip_suffix('*') {
        filename.starts_with(prefix)
    } else {
        filename == glob
    }
}

/// Recursively list directory contents.
fn list_directory(
    root: &Path,
    dir: &Path,
    depth: u32,
    max_depth: u32,
    glob: Option<&str>,
    include_size: bool,
) -> Vec<String> {
    let mut result = Vec::new();

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(reader) => reader.filter_map(|e| e.ok()).collect(),
        Err(e) => {
            let rel = dir.strip_prefix(root).unwrap_or(dir);
            result.push(format!("! {} ({})", rel.display(), e));
            return result;
        }
    };

    entries.sort_by_key(|a| a.file_name());

    for entry in &entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();

        if file_name_str == ".clausura" {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                result.push(format!("! {} ({})", rel.display(), e));
                continue;
            }
        };

        if file_type.is_dir() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            result.push(format!("{}/", rel.display()));

            if depth < max_depth {
                result.extend(list_directory(
                    root,
                    &path,
                    depth + 1,
                    max_depth,
                    glob,
                    include_size,
                ));
            }
        } else {
            if let Some(g) = glob {
                if !g.is_empty() && !matches_glob(&file_name_str, g) {
                    continue;
                }
            }

            let rel = path.strip_prefix(root).unwrap_or(&path);
            if include_size {
                match std::fs::metadata(&path) {
                    Ok(meta) => {
                        result.push(format!("{} ({} B)", rel.display(), meta.len()));
                    }
                    Err(e) => {
                        result.push(format!("! {} ({})", rel.display(), e));
                    }
                }
            } else {
                result.push(rel.display().to_string());
            }
        }
    }

    result
}

impl ListFilesTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        let canonical_root = workspace_root.canonicalize().unwrap_or(workspace_root);
        Self {
            workspace_root: canonical_root,
        }
    }
}

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }

    fn description(&self) -> &str {
        "List files and directories. Path is relative to the workspace root."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to list"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Recursively list subdirectories"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Max recursion depth"
                },
                "glob": {
                    "type": "string",
                    "description": "Filename filter pattern"
                },
                "include_size": {
                    "type": "boolean",
                    "description": "Show file sizes"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path_str = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::ExecutionFailed("Missing 'path' argument".into()))?;

        let resolved = resolve_sandboxed_path(&self.workspace_root, path_str)?;

        if !resolved.is_dir() {
            return Err(ToolError::ExecutionFailed(format!(
                "Not a directory: {}",
                path_str
            )));
        }

        let recursive = args["recursive"].as_bool().unwrap_or(true);
        let max_depth_raw = args["max_depth"].as_u64().unwrap_or(3) as u32;
        let max_depth = if recursive { max_depth_raw.min(3) } else { 0 };
        let glob = args["glob"].as_str();
        let include_size = args["include_size"].as_bool().unwrap_or(false);

        let lines = list_directory(
            &self.workspace_root,
            &resolved,
            0,
            max_depth,
            glob,
            include_size,
        );

        Ok(lines.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// GrepTool
// ---------------------------------------------------------------------------

/// Returns true if the first 8 KB of the file contains a null byte.
fn is_binary_file(path: &Path) -> bool {
    let mut buf = [0u8; 8192];
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false, // can't read → treat as non-binary (will fail later)
    };
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

/// Configuration for grep search operations, bundling the parameters shared
/// across search_file and grep_directory.
struct GrepCfg<'a> {
    root: &'a Path,
    pattern: &'a str,
    is_regex: bool,
    file_types: &'a [String],
    regex: Option<&'a Regex>,
}

/// Search a single file for pattern matches, returning false when max_results
/// is reached.
fn search_file(
    path: &Path,
    cfg: &GrepCfg,
    max_results: &mut usize,
    remaining: &mut usize,
    results: &mut Vec<String>,
) -> bool {
    if is_binary_file(path) {
        return true;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return true,
    };
    let rel = path.strip_prefix(cfg.root).unwrap_or(path);
    for (line_num, line) in content.lines().enumerate() {
        if *max_results > 0 && results.len() >= *max_results {
            *remaining += 1;
            continue;
        }
        let matched = if cfg.is_regex {
            cfg.regex.is_some_and(|re| re.is_match(line))
        } else {
            line.contains(cfg.pattern)
        };
        if matched {
            let truncated = if line.len() > 200 { &line[..200] } else { line };
            results.push(format!(
                "{}:{}: {}",
                rel.display(),
                line_num + 1,
                truncated
            ));
        }
    }
    true
}

fn grep_directory(
    cfg: &GrepCfg,
    dir: &Path,
    max_results: &mut usize,
    remaining: &mut usize,
    results: &mut Vec<String>,
) {
    let to_skip = [".git", "target", ".clausura", "node_modules"];

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(reader) => reader.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };

    entries.sort_by_key(|a| a.file_name());

    for entry in &entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();

        if to_skip.contains(&file_name_str.as_ref()) {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            grep_directory(
                cfg,
                &path,
                max_results,
                remaining,
                results,
            );
        } else {
            if !cfg.file_types.is_empty() {
                let matches_ext = cfg.file_types.iter().any(|ext| {
                    file_name_str.ends_with(ext.as_str())
                });
                if !matches_ext {
                    continue;
                }
            }
            let more = search_file(
                &path,
                cfg,
                max_results,
                remaining,
                results,
            );
            if !more {
                break;
            }
        }
    }
}

/// Searches for text patterns across files in the workspace, supporting both
/// literal and regex matching.
pub struct GrepTool {
    workspace_root: PathBuf,
}

impl GrepTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        let canonical_root = workspace_root.canonicalize().unwrap_or(workspace_root);
        Self {
            workspace_root: canonical_root,
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for text patterns in files. Supports literal (default) and regex matching. Path is relative to the workspace root. Note: regex mode uses a simplified engine without lookahead, lookbehind, backreferences, or Unicode property escapes."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory to search (relative path)"
                },
                "pattern": {
                    "type": "string",
                    "description": "Text pattern to search for"
                },
                "regex": {
                    "type": "boolean",
                    "description": "Use regex search via regex-lite (default: false)"
                },
                "file_types": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only search files with these extensions, e.g., [\".rs\", \".toml\"]"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 50, max: 200)"
                }
            },
            "required": ["path", "pattern"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path_str = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::ExecutionFailed("Missing 'path' argument".into()))?;
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::ExecutionFailed("Missing 'pattern' argument".into()))?;
        let is_regex = args["regex"].as_bool().unwrap_or(false);

        let file_types: Vec<String> = args["file_types"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let max_results_raw = args["max_results"].as_u64().unwrap_or(50) as usize;
        let max_results = max_results_raw.min(200);

        let resolved = resolve_sandboxed_path(&self.workspace_root, path_str)?;

        let regex = if is_regex {
            match Regex::new(pattern) {
                Ok(re) => Some(re),
                Err(e) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Invalid regex pattern: {pattern} — {e}"
                    )));
                }
            }
        } else {
            None
        };

        let mut remaining = 0usize;
        let mut max = max_results;

        let cfg = GrepCfg {
            root: &self.workspace_root,
            pattern,
            is_regex,
            file_types: &file_types,
            regex: regex.as_ref(),
        };

        let mut results = Vec::new();
        if resolved.is_file() {
            search_file(&resolved, &cfg, &mut max, &mut remaining, &mut results);
        } else if resolved.is_dir() {
            grep_directory(&cfg, &resolved, &mut max, &mut remaining, &mut results);
        } else {
            return Err(ToolError::ExecutionFailed(format!(
                "Not a file or directory: {}",
                path_str
            )));
        };

        let mut output = results.join("\n");

        if remaining > 0 {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&format!(
                "... and {} more matches (use more specific pattern or narrower path)",
                remaining
            ));
        }

        Ok(output)
    }
}

/// Create the default set of tools for the given workspace root.
/// If allowlist is empty, shell_exec is disabled (no commands allowed).
pub fn default_tools(workspace_root: PathBuf, allowlist: &[String]) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(ReadFileTool::new(workspace_root.clone()));
    registry.register(GitDiffTool::new(workspace_root.clone()));
    registry.register(ShellExecTool::new(workspace_root.clone(), allowlist.to_vec()));
    registry.register(ListFilesTool::new(workspace_root.clone()));
    registry.register(GrepTool::new(workspace_root));
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

    #[tokio::test]
    async fn test_read_file_with_offset() {
        let (_tmp, root) = setup_workspace();
        let test_file = root.join("test.txt");
        std::fs::write(&test_file, "line1\nline2\nline3\nline4\nline5").unwrap();

        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "offset": 3}))
            .await
            .unwrap();
        assert_eq!(result, "line3\nline4\nline5");
    }

    #[tokio::test]
    async fn test_read_file_with_limit() {
        let (_tmp, root) = setup_workspace();
        let test_file = root.join("test.txt");
        std::fs::write(&test_file, "line1\nline2\nline3\nline4\nline5").unwrap();

        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "limit": 2}))
            .await
            .unwrap();
        assert_eq!(result, "line1\nline2");
    }

    #[tokio::test]
    async fn test_read_file_offset_and_limit() {
        let (_tmp, root) = setup_workspace();
        let test_file = root.join("test.txt");
        std::fs::write(&test_file, "line1\nline2\nline3\nline4\nline5").unwrap();

        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert_eq!(result, "line2\nline3");
    }

    #[tokio::test]
    async fn test_read_file_offset_exceeds_file() {
        let (_tmp, root) = setup_workspace();
        let test_file = root.join("test.txt");
        std::fs::write(&test_file, "line1\nline2\nline3").unwrap();

        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "offset": 10}))
            .await
            .unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_read_file_limit_zero() {
        let (_tmp, root) = setup_workspace();
        let test_file = root.join("test.txt");
        std::fs::write(&test_file, "line1\nline2\nline3").unwrap();

        let tool = ReadFileTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "limit": 0}))
            .await
            .unwrap();
        assert_eq!(result, "");
    }

    // -----------------------------------------------------------------------
    // resolve_sandboxed_path tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_resolve_sandboxed_accepts_valid() {
        let (_tmp, root) = setup_workspace();
        let root = root.canonicalize().unwrap();
        let test_file = root.join("test.txt");
        std::fs::write(&test_file, "hello").unwrap();

        let result = resolve_sandboxed_path(&root, "test.txt");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), test_file.canonicalize().unwrap());
    }

    #[tokio::test]
    async fn test_resolve_sandboxed_rejects_absolute() {
        let (_tmp, root) = setup_workspace();
        let result = resolve_sandboxed_path(&root, "/etc/passwd");
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
    }

    #[tokio::test]
    async fn test_resolve_sandboxed_rejects_traversal() {
        let (_tmp, root) = setup_workspace();
        let result = resolve_sandboxed_path(&root, "../outside");
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
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
    // ListFilesTool tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_list_files_basic() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();

        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": ".", "recursive": false}))
            .await
            .unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines.contains(&"a.txt"));
        assert!(lines.contains(&"b.txt"));
    }

    #[tokio::test]
    async fn test_list_files_recursive() {
        let (_tmp, root) = setup_workspace();
        std::fs::create_dir_all(root.join("sub/nested")).unwrap();
        std::fs::write(root.join("top.txt"), "").unwrap();
        std::fs::write(root.join("sub/inner.txt"), "").unwrap();
        std::fs::write(root.join("sub/nested/deep.txt"), "").unwrap();

        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": ".", "max_depth": 2}))
            .await
            .unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert!(lines.contains(&"top.txt"));
        assert!(lines.contains(&"sub/"));
        assert!(lines.contains(&"sub/inner.txt"));
        assert!(lines.contains(&"sub/nested/"));
        assert!(lines.contains(&"sub/nested/deep.txt"));
    }

    #[tokio::test]
    async fn test_list_files_glob_filter() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("main.rs"), "").unwrap();
        std::fs::write(root.join("lib.rs"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();

        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": ".", "glob": "*.rs", "recursive": false}))
            .await
            .unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines.contains(&"main.rs"));
        assert!(lines.contains(&"lib.rs"));
    }

    #[tokio::test]
    async fn test_list_files_include_size() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("data.bin"), "hello").unwrap();

        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": ".", "include_size": true, "recursive": false}))
            .await
            .unwrap();
        assert!(result.contains(" B"), "Expected size suffix, got: {}", result);
    }

    #[tokio::test]
    async fn test_list_files_rejects_absolute() {
        let (_tmp, root) = setup_workspace();
        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "/etc"}))
            .await;
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
    }

    #[tokio::test]
    async fn test_list_files_rejects_traversal() {
        let (_tmp, root) = setup_workspace();
        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "../outside"}))
            .await;
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
    }

    #[tokio::test]
    async fn test_list_files_empty_directory() {
        let (_tmp, root) = setup_workspace();
        std::fs::create_dir(root.join("empty")).unwrap();

        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "empty"}))
            .await
            .unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_list_files_excludes_clausura_dir() {
        let (_tmp, root) = setup_workspace();
        std::fs::create_dir(root.join(".clausura")).unwrap();
        std::fs::write(root.join(".clausura/config.yaml"), "").unwrap();
        std::fs::write(root.join("visible.txt"), "").unwrap();

        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": ".", "recursive": true}))
            .await
            .unwrap();
        assert!(!result.contains(".clausura"), "Output should not contain .clausura:\n{}", result);
        assert!(result.contains("visible.txt"));
    }

    #[tokio::test]
    async fn test_list_files_max_depth() {
        let (_tmp, root) = setup_workspace();
        std::fs::create_dir_all(root.join("a/b/c/d")).unwrap();
        std::fs::write(root.join("a/b/c/d/deep.txt"), "").unwrap();
        std::fs::write(root.join("a/top.txt"), "").unwrap();

        let tool = ListFilesTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": ".", "max_depth": 5}))
            .await
            .unwrap();
        assert!(
            result.contains("a/top.txt"),
            "Expected a/top.txt in output:\n{}",
            result
        );
        assert!(
            result.contains("a/b/c/d/"),
            "Expected a/b/c/d/ (level 3) in output:\n{}",
            result
        );
        assert!(
            !result.contains("a/b/c/d/deep.txt"),
            "Did not expect a/b/c/d/deep.txt (depth > 3) in output:\n{}",
            result
        );
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
        let registry = default_tools(root, &[]);
        let defs = registry.list_definitions();
        assert_eq!(defs.len(), 5);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"git_diff"));
        assert!(names.contains(&"shell_exec"));
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"grep"));
    }

    #[test]
    fn test_tool_registry_default() {
        let registry: ToolRegistry = Default::default();
        let defs = registry.list_definitions();
        assert!(defs.is_empty());
    }

    // -----------------------------------------------------------------------
    // GrepTool tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_grep_literal_basic() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("test.txt"), "hello world\nfoo bar\nhello again").unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "pattern": "hello"}))
            .await
            .unwrap();
        assert!(result.contains("test.txt:1: hello world"));
        assert!(result.contains("test.txt:3: hello again"));
        assert!(!result.contains("foo bar"));
    }

    #[tokio::test]
    async fn test_grep_literal_no_matches() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("test.txt"), "hello world\nfoo bar").unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "pattern": "nonexistent"}))
            .await
            .unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_grep_regex_basic() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("test.txt"), "line1 alpha\nline2 beta\nother gamma").unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "pattern": "line\\d+", "regex": true}))
            .await
            .unwrap();
        assert!(result.contains("test.txt:1: line1 alpha"));
        assert!(result.contains("test.txt:2: line2 beta"));
        assert!(!result.contains("other gamma"));
    }

    #[tokio::test]
    async fn test_grep_regex_invalid() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("test.txt"), "content").unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "test.txt", "pattern": "[invalid", "regex": true}))
            .await;
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("Invalid regex pattern"));
    }

    #[tokio::test]
    async fn test_grep_file_types_filter() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("main.rs"), "fn main() {}\nhello fn").unwrap();
        std::fs::write(root.join("README.md"), "hello readme").unwrap();
        std::fs::write(root.join("Cargo.toml"), "hello cargo").unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({
                "path": ".",
                "pattern": "hello",
                "file_types": [".rs", ".toml"]
            }))
            .await
            .unwrap();
        assert!(result.contains("main.rs"));
        assert!(result.contains("Cargo.toml"));
        assert!(!result.contains("README.md"));
    }

    #[tokio::test]
    async fn test_grep_excludes_dirs() {
        let (_tmp, root) = setup_workspace();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "hello git").unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/output"), "hello target").unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), "hello node").unwrap();
        std::fs::write(root.join("src.txt"), "hello src").unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": ".", "pattern": "hello"}))
            .await
            .unwrap();
        assert!(result.contains("src.txt"), "Should find src.txt, got: {}", result);
        assert!(!result.contains(".git"), "Should exclude .git, got: {}", result);
        assert!(!result.contains("target"), "Should exclude target, got: {}", result);
        assert!(!result.contains("node_modules"), "Should exclude node_modules, got: {}", result);
    }

    #[tokio::test]
    async fn test_grep_skips_binary() {
        let (_tmp, root) = setup_workspace();
        let binary_content: Vec<u8> = vec![0x00, 0x01, 0x02, 0x68, 0x65, 0x6c, 0x6c, 0x6f]; // null + "hello"
        std::fs::write(root.join("data.bin"), binary_content).unwrap();
        std::fs::write(root.join("text.txt"), "hello world").unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": ".", "pattern": "hello"}))
            .await
            .unwrap();
        assert!(result.contains("text.txt"), "Should find text.txt, got: {}", result);
        assert!(!result.contains("data.bin"), "Should skip binary data.bin, got: {}", result);
    }

    #[tokio::test]
    async fn test_grep_max_results_truncation() {
        let (_tmp, root) = setup_workspace();
        let mut content = String::new();
        for i in 1..=100 {
            content.push_str(&format!("line {} match\n", i));
        }
        std::fs::write(root.join("big.txt"), content).unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "big.txt", "pattern": "match", "max_results": 20}))
            .await
            .unwrap();
        let lines: Vec<&str> = result.lines().collect();
        let match_lines = lines.iter().filter(|l| l.contains(":")).count();
        let truncation_line = lines.iter().find(|l| l.starts_with("... and "));
        assert_eq!(match_lines, 20, "Expected 20 match lines, got: {}", result);
        assert!(truncation_line.is_some(), "Expected truncation notice, got: {}", result);
    }

    #[tokio::test]
    async fn test_grep_rejects_traversal() {
        let (_tmp, root) = setup_workspace();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "../outside", "pattern": "test"}))
            .await;
        assert!(matches!(result, Err(ToolError::SandboxViolation(_))));
    }

    #[tokio::test]
    async fn test_grep_single_file() {
        let (_tmp, root) = setup_workspace();
        std::fs::write(root.join("a.rs"), "hello a").unwrap();
        std::fs::write(root.join("b.rs"), "hello b").unwrap();

        let tool = GrepTool::new(root);
        let result = tool
            .execute(serde_json::json!({"path": "a.rs", "pattern": "hello"}))
            .await
            .unwrap();
        assert!(result.contains("a.rs:1: hello a"));
        assert!(!result.contains("b.rs"));
    }
}
