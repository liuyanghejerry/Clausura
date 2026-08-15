//! Skill loading with progressive disclosure.
//!
//! Clausura consumes community skill files (Markdown, commonly with YAML
//! frontmatter carrying `name` + `description`). Instead of inlining every
//! skill body into the system prompt (which eats the token budget before the
//! review even starts), only a *catalog* (name + description per skill) is
//! injected. The agent loads a skill's full body on demand through the
//! `read_skill` tool. Gating rules remain fully under user control — skills
//! answer "how to review", gating answers "how many findings is too many".

use crate::types::ConfigError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A resolved skill: metadata (from frontmatter or derived) plus the body
/// text the `read_skill` tool serves on demand.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Skill {
    /// Kebab-case name used by the `read_skill` tool.
    pub name: String,
    /// One-line description shown in the catalog.
    pub description: String,
    /// Full body (frontmatter stripped).
    pub body: String,
}

/// Resolve skill references into `Skill` values.
///
/// Each ref resolves through the same lookup as [`resolve_skill`] (local path,
/// workspace-relative path, or named reference), but reads the *raw* file so
/// the frontmatter `name`/`description` can be captured before the body is
/// stripped. When frontmatter is absent, the name falls back to the ref's
/// basename and the description to the first non-empty body line.
pub fn resolve_skills(skill_refs: &[String], workspace: &Path) -> Result<Vec<Skill>, ConfigError> {
    skill_refs
        .iter()
        .map(|skill_ref| {
            let raw = resolve_skill_raw(skill_ref, workspace)?;
            let meta = frontmatter_meta(&raw);
            let body = strip_frontmatter(&raw);
            let fallback_name = skill_ref
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(skill_ref)
                .trim_end_matches(".md")
                .to_string();
            Ok(Skill {
                name: meta.name.unwrap_or(fallback_name),
                description: meta
                    .description
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| skill_description(&body)),
                body,
            })
        })
        .collect()
}

/// Build the catalog section injected into the system prompt.
///
/// Only names + descriptions are inlined; bodies are served by `read_skill`.
pub fn build_skill_catalog(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        "Available review skills:".to_string(),
        "Before reporting findings, load every skill relevant to the task with".to_string(),
        "the `read_skill` tool (by name) and apply its instructions.".to_string(),
        String::new(),
    ];
    for skill in skills {
        lines.push(format!("- {} — {}", skill.name, skill.description));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Resolve a skill reference to its prompt body (frontmatter stripped).
///
/// Resolution order:
/// 1. Absolute path or path relative to cwd that exists as-is.
/// 2. Path relative to the workspace root.
/// 3. Named reference — looked up in `.clausura/skills/<name>/SKILL.md`
///    (project-level) then `~/.clausura/skills/<name>/SKILL.md` (user-level).
pub fn resolve_skill(name_or_path: &str, workspace: &Path) -> Result<String, ConfigError> {
    resolve_skill_raw(name_or_path, workspace).map(|raw| strip_frontmatter(&raw))
}

/// Like [`resolve_skill`] but returns the raw file content (frontmatter
/// included), so callers can read the skill metadata.
fn resolve_skill_raw(name_or_path: &str, workspace: &Path) -> Result<String, ConfigError> {
    let path = Path::new(name_or_path);
    if path.exists() {
        return load_skill_file_raw(path);
    }
    let workspace_path = workspace.join(name_or_path);
    if workspace_path.exists() {
        return load_skill_file_raw(&workspace_path);
    }

    if !name_or_path.contains("://") && !name_or_path.starts_with('/') {
        return resolve_named_skill_raw(name_or_path, workspace);
    }

    Err(ConfigError::FileNotFound(format!(
        "Skill not found: {name_or_path}"
    )))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn load_skill_file_raw(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path)
        .map_err(|e| ConfigError::FileNotFound(format!("{}: {e}", path.display())))
}

fn resolve_named_skill_raw(name: &str, workspace: &Path) -> Result<String, ConfigError> {
    let skill_rel = format!("{}/SKILL.md", name.trim_end_matches('/'));

    let search_paths: Vec<PathBuf> = vec![
        workspace.join(".clausura").join("skills").join(&skill_rel),
        dirs::home_dir()
            .unwrap_or_default()
            .join(".clausura")
            .join("skills")
            .join(&skill_rel),
    ];

    for p in &search_paths {
        if p.exists() {
            return load_skill_file_raw(p);
        }
    }

    Err(ConfigError::FileNotFound(format!(
        "Named skill '{name}' not found. Looked in: {}",
        search_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Strip YAML frontmatter delimited by `---` at the start of the content.
/// Returns the body without the frontmatter block. If no frontmatter is
/// found or the closing `---` is missing, the original content is returned
/// unchanged.
pub(crate) fn strip_frontmatter(content: &str) -> String {
    match split_frontmatter(content) {
        Some((_, body)) => body,
        None => content.to_string(),
    }
}

/// Split `content` into (frontmatter block, body). Returns `None` when there
/// is no valid frontmatter block.
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_open = &trimmed[3..];
    let rest = after_open.strip_prefix('\n').unwrap_or(after_open);
    let pos = rest.find("\n---")?;
    // pos + 4 skips "\n---" itself
    let body = rest[pos + 4..].trim_start().to_string();
    if body.is_empty() {
        return None;
    }
    Some((rest[..pos].to_string(), body))
}

/// Parse frontmatter into name/description (when the original content still
/// has one; the pre-stripped body carries no metadata).
#[derive(Deserialize, Default)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

fn frontmatter_meta(original: &str) -> SkillFrontmatter {
    match split_frontmatter(original) {
        Some((fm, _)) => serde_yaml::from_str(&fm).unwrap_or_default(),
        None => SkillFrontmatter::default(),
    }
}

/// Derive a description from the body: first non-empty line that is not a
/// heading, truncated. If every line is a heading, use the first line with
/// its leading `#`s stripped.
fn skill_description(body: &str) -> String {
    let first = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .or_else(|| {
            body.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| l.trim_start_matches('#'))
        })
        .unwrap_or("")
        .trim();
    let mut desc = first.to_string();
    if desc.len() > 160 {
        desc.truncate(157);
        desc.push_str("...");
    }
    desc
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- strip_frontmatter --------------------------------------------------

    #[test]
    fn test_strip_frontmatter_basic() {
        let input = "---\nname: test\ndescription: A test skill\n---\n\n# Body\nCheck for bugs.";
        let body = strip_frontmatter(input);
        assert_eq!(body, "# Body\nCheck for bugs.");
    }

    #[test]
    fn test_strip_frontmatter_no_newline_after_open() {
        let input = "---\nname: test\n---\nbody";
        let body = strip_frontmatter(input);
        assert_eq!(body, "body");
    }

    #[test]
    fn test_strip_frontmatter_no_opening() {
        let input = "# Just a markdown file\nNo frontmatter.";
        let body = strip_frontmatter(input);
        assert_eq!(body, input);
    }

    #[test]
    fn test_strip_frontmatter_no_closing() {
        let input = "---\nname: test\n# Body but no closing";
        let body = strip_frontmatter(input);
        // No closing delimiter → returned unchanged
        assert_eq!(body, input);
    }

    #[test]
    fn test_strip_frontmatter_empty_body() {
        let input = "---\nname: test\n---\n";
        let body = strip_frontmatter(input);
        assert_eq!(body, input); // empty body → unchanged
    }

    #[test]
    fn test_strip_frontmatter_with_leading_whitespace() {
        let input = "  \n  ---\nname: test\n---\nbody after whitespace";
        let body = strip_frontmatter(input);
        assert_eq!(body, "body after whitespace");
    }

    #[test]
    fn test_strip_frontmatter_preserves_inner_content() {
        // The body starts right after the closing --- line.
        let input = "---\nname: test\n---\n\n\nline1\nline2";
        let body = strip_frontmatter(input);
        // Two leading blank lines before "line1": trim_start eats them.
        assert_eq!(body, "line1\nline2");
    }

    // -- resolve_skills / build_skill_catalog -------------------------------

    #[test]
    fn test_resolve_skills_reads_frontmatter_metadata() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp
            .path()
            .join(".clausura")
            .join("skills")
            .join("sec-check");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: security-review\ndescription: 检查 SQL 注入、XSS、硬编码密钥\n---\n\n# Body\nCheck for SQL injection.",
        )
        .unwrap();

        let skills = resolve_skills(&["sec-check".to_string()], tmp.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "security-review");
        assert_eq!(skills[0].description, "检查 SQL 注入、XSS、硬编码密钥");
        assert_eq!(skills[0].body, "# Body\nCheck for SQL injection.");
    }

    #[test]
    fn test_resolve_skills_without_frontmatter_falls_back() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("plain.md"), "# Heading\nCheck for bugs.").unwrap();

        let skills = resolve_skills(&["plain.md".to_string()], tmp.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "plain");
        assert_eq!(skills[0].description, "Check for bugs.");
        assert_eq!(skills[0].body, "# Heading\nCheck for bugs.");
    }

    #[test]
    fn test_build_skill_catalog_lists_names_and_descriptions() {
        let skills = vec![
            Skill {
                name: "security-review".into(),
                description: "检查 SQL 注入".into(),
                body: "body a".into(),
            },
            Skill {
                name: "vue-check".into(),
                description: "Vue 最佳实践".into(),
                body: "body b".into(),
            },
        ];
        let catalog = build_skill_catalog(&skills);
        assert!(catalog.contains("- security-review — 检查 SQL 注入"));
        assert!(catalog.contains("- vue-check — Vue 最佳实践"));
        assert!(catalog.contains("read_skill"));
        assert!(!catalog.contains("body a"), "bodies must not be inlined");
        assert!(!catalog.contains("body b"), "bodies must not be inlined");
    }

    #[test]
    fn test_build_skill_catalog_empty() {
        assert_eq!(build_skill_catalog(&[]), "");
    }

    #[test]
    fn test_skill_description_truncates_long_first_line() {
        let body = format!("# T\n{}\nmore", "x".repeat(300));
        let desc = skill_description(&body);
        assert!(desc.len() <= 160);
        assert!(desc.ends_with("..."));
    }

    // -- resolve_skill (integration via temp dirs) --------------------------

    #[test]
    fn test_resolve_local_file_direct() {
        let tmp = TempDir::new().unwrap();
        let skill_path = tmp.path().join("my-skill.md");
        std::fs::write(&skill_path, "# Check for bugs").unwrap();

        let result = resolve_skill(skill_path.to_str().unwrap(), tmp.path()).unwrap();
        assert_eq!(result, "# Check for bugs");
    }

    #[test]
    fn test_resolve_local_file_relative_to_workspace() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("skill.md"), "# Workspace skill").unwrap();

        // cwd is different; only workspace-relative path should match.
        let result = resolve_skill("skill.md", tmp.path()).unwrap();
        assert_eq!(result, "# Workspace skill");
    }

    #[test]
    fn test_resolve_named_skill_project_level() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp
            .path()
            .join(".clausura")
            .join("skills")
            .join("team")
            .join("my-check");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: t\n---\nTeam check body",
        )
        .unwrap();

        let result = resolve_skill("team/my-check", tmp.path()).unwrap();
        assert_eq!(result, "Team check body");
    }

    #[test]
    fn test_resolve_named_skill_not_found() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_skill("nonexistent/skill", tmp.path()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Named skill 'nonexistent/skill' not found"));
    }

    #[test]
    fn test_resolve_missing_file() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_skill("/tmp/does-not-exist-98765.md", tmp.path()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Skill not found"));
    }

    #[test]
    fn test_resolve_named_skill_trailing_slash() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join(".clausura").join("skills").join("trailing");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "body").unwrap();

        // "trailing/" should normalize to "trailing/SKILL.md"
        let result = resolve_skill("trailing/", tmp.path()).unwrap();
        assert_eq!(result, "body");
    }
}
