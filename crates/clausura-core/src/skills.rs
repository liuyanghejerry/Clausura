//! Skill prompt loading and merging.
//!
//! Clausura consumes community skill files (Markdown, commonly with YAML
//! frontmatter) and injects their content into the agent's system prompt.
//! Gating rules remain fully under user control — skills answer "how to
//! review", gating answers "how many findings is too many".

use crate::types::ConfigError;
use std::path::{Path, PathBuf};

/// Resolve a skill reference to its prompt body (frontmatter stripped).
///
/// Resolution order:
/// 1. Absolute path or path relative to cwd that exists as-is.
/// 2. Path relative to the workspace root.
/// 3. Named reference — looked up in `.clausura/skills/<name>/SKILL.md`
///    (project-level) then `~/.clausura/skills/<name>/SKILL.md` (user-level).
pub fn resolve_skill(name_or_path: &str, workspace: &Path) -> Result<String, ConfigError> {
    let path = Path::new(name_or_path);
    if path.exists() {
        return load_skill_file(path);
    }
    let workspace_path = workspace.join(name_or_path);
    if workspace_path.exists() {
        return load_skill_file(&workspace_path);
    }

    if !name_or_path.contains("://") && !name_or_path.starts_with('/') {
        return resolve_named_skill(name_or_path, workspace);
    }

    Err(ConfigError::FileNotFound(format!(
        "Skill not found: {name_or_path}"
    )))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn load_skill_file(path: &Path) -> Result<String, ConfigError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        ConfigError::FileNotFound(format!("{}: {e}", path.display()))
    })?;
    Ok(strip_frontmatter(&content))
}

fn resolve_named_skill(name: &str, workspace: &Path) -> Result<String, ConfigError> {
    let skill_rel = format!("{}/SKILL.md", name.trim_end_matches('/'));

    let search_paths: Vec<PathBuf> = vec![
        workspace
            .join(".clausura")
            .join("skills")
            .join(&skill_rel),
        dirs::home_dir()
            .unwrap_or_default()
            .join(".clausura")
            .join("skills")
            .join(&skill_rel),
    ];

    for p in &search_paths {
        if p.exists() {
            return load_skill_file(p);
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
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }

    // Skip the opening "---" and optional newline.
    let after_open = &trimmed[3..];
    let rest = after_open.strip_prefix('\n').unwrap_or(after_open);

    if let Some(pos) = rest.find("\n---") {
        // pos + 4 skips "\n---" itself
        let body = rest[pos + 4..].trim_start();
        if !body.is_empty() {
            return body.to_string();
        }
    }

    // No valid closing delimiter; return original content.
    content.to_string()
}

/// Merge resolved skill contents and a user prompt template into a single
/// system-prompt-ready string. Each skill is delimited with a `[Skill: …]`
/// header; the user's template (if non-empty and not the default placeholder)
/// appears after a `---` separator.
pub fn merge_prompts(
    skill_contents: &[(String, String)], // (skill_ref, body)
    template: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    for (skill_ref, content) in skill_contents {
        parts.push(format!("[Skill: {skill_ref}]\n{content}"));
    }

    let has_user_template = !template.is_empty() && template != "{{task_description}}";
    if has_user_template {
        parts.push(template.to_string());
    }

    parts.join("\n\n---\n\n")
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

    // -- merge_prompts ------------------------------------------------------

    #[test]
    fn test_merge_single_skill_no_template() {
        let skills = vec![("sec/check".into(), "Find SQL injection.".into())];
        let merged = merge_prompts(&skills, "{{task_description}}");
        assert_eq!(merged, "[Skill: sec/check]\nFind SQL injection.");
    }

    #[test]
    fn test_merge_multiple_skills_with_template() {
        let skills = vec![
            ("a".into(), "Check A.".into()),
            ("b".into(), "Check B.".into()),
        ];
        let merged = merge_prompts(&skills, "Also check C.");
        assert_eq!(
            merged,
            "[Skill: a]\nCheck A.\n\n---\n\n[Skill: b]\nCheck B.\n\n---\n\nAlso check C."
        );
    }

    #[test]
    fn test_merge_no_skills_just_template() {
        let skills: Vec<(String, String)> = vec![];
        let merged = merge_prompts(&skills, "Review the diff.");
        assert_eq!(merged, "Review the diff.");
    }

    #[test]
    fn test_merge_empty_everything() {
        let skills: Vec<(String, String)> = vec![];
        let merged = merge_prompts(&skills, "");
        assert_eq!(merged, "");
    }

    // -- resolve_skill (integration via temp dirs) --------------------------

    #[test]
    fn test_resolve_local_file_direct() {
        let tmp = TempDir::new().unwrap();
        let skill_path = tmp.path().join("my-skill.md");
        std::fs::write(&skill_path, "# Check for bugs").unwrap();

        let result = resolve_skill(
            skill_path.to_str().unwrap(),
            tmp.path(),
        )
        .unwrap();
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
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: t\n---\nTeam check body").unwrap();

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
        let skill_dir = tmp
            .path()
            .join(".clausura")
            .join("skills")
            .join("trailing");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "body").unwrap();

        // "trailing/" should normalize to "trailing/SKILL.md"
        let result = resolve_skill("trailing/", tmp.path()).unwrap();
        assert_eq!(result, "body");
    }
}
