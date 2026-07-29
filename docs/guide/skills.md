# Skills

Skills are reusable Markdown files that encode **what** to review. They answer "what should I look for and how should I report it?" — while gating rules answer "how many findings is too many?" The two are cleanly separated.

## Why Skills?

Without skills, every project writes its own prompt from scratch. This means:

- Review quality depends on the developer's domain knowledge
- The same review logic can't be reused across projects
- Team conventions are hard to distribute and enforce

Skills solve this: write the review logic once (as a Markdown file), reuse everywhere, and let each team set their own thresholds.

## Using a Skill

Reference skills in `skill_prompts`:

```yaml
task:
  skill_prompts:
    # Local file (relative to workspace root, or absolute)
    - ./skills/security-review.md

    # Named reference (looks in .clausura/skills/<name>/SKILL.md,
    # then ~/.clausura/skills/<name>/SKILL.md)
    - security-review
    - team/vue-best-practices

  # Your own additions go after the skills
  prompt_template: |
    Also check: no console.log in production code.

  gating:
    - rule: sql-injection
      max_findings: 0
      action: fail
```

When the agent runs, skill content is injected into the system prompt before your `prompt_template`:

```
[Skill: security-review]
...skill content...

[Skill: team/vue-best-practices]
...skill content...

---

Your prompt_template content
```

## Skill File Format

A skill is a Markdown file, optionally with YAML frontmatter:

```markdown
---
name: security-review
description: Check for SQL injection, XSS, hardcoded secrets, and insecure dependencies.
---

# Security Code Review

Review the code diff for the following security issues.

## SQL Injection
- Any string concatenation used to build SQL queries
- Non-parameterized database calls
- rule_id: `sql-injection`
- severity: `error`

## Hardcoded Credentials
- API keys, passwords, tokens in source code
- Config files with plaintext secrets (exclude example/docs)
- rule_id: `hardcoded-secret`
- severity: `error`

## Output Format

For each finding, output:

```json
{
  "rule_id": "sql-injection",
  "severity": "error",
  "message": "SQL injection in login.js:15 — user input concatenated into SQL query",
  "evidence": "const sql = \"SELECT * FROM users WHERE name = '\" + user + \"'\"",
  "location": {
    "file": "src/login.js",
    "line_start": 15,
    "line_end": 15,
    "column_start": 13,
    "column_end": 62
  }
}
```
```

**Frontmatter** (`---` delimited YAML at the top) is stripped automatically. Only the Markdown body is injected into the prompt.

**No gating in skills.** Skills define what and how to review, never the pass/fail threshold. That belongs in `.clausura.yaml` under `gating:`.

## Installing Skills

### Project-level (this repo only)

```bash
mkdir -p .clausura/skills/security-review
cp ~/Downloads/security-review-SKILL.md .clausura/skills/security-review/SKILL.md
```

Then reference by name:

```yaml
skill_prompts:
  - security-review
```

### User-level (available to all your projects)

```bash
mkdir -p ~/.clausura/skills/team/vue-check
cp ~/Downloads/vue-check-SKILL.md ~/.clausura/skills/team/vue-check/SKILL.md
```

Then reference by path including the namespace:

```yaml
skill_prompts:
  - team/vue-check
```

## Skill Resolution Order

When you reference a skill by name (not a file path), Clausura searches:

1. `.clausura/skills/<name>/SKILL.md` (project-level, in your workspace)
2. `~/.clausura/skills/<name>/SKILL.md` (user-level, in your home directory)

The first match wins. Project-level skills take precedence.

## Composing Multiple Skills

Skills are appended in declaration order. The agent sees them as a single system prompt:

```yaml
task:
  skill_prompts:
    - community/security-review
    - team/vue-best-practices
    - ./project-specific-checks.md
```

This composes three review perspectives into one agent run. All findings from all skills are evaluated against the same gating rules.

If two skills define the same `rule_id` (e.g., both define `sql-injection`), findings from both are counted together. This is intentional — you get one threshold per concern, regardless of which skill found it.

## Creating Your Own Skill

A good skill file:

1. **Has clear rule_ids** — each check has a unique `rule_id` that gating rules can match
2. **Specifies severities** — teaches the LLM a consistent severity scheme
3. **Shows output format** — includes an example finding so the LLM knows the expected JSON shape
4. **Includes `location` guidance** — tells the LLM to report file/line when possible
5. **Keeps scope narrow** — one skill per domain (security, i18n, Vue conventions, etc.)

### Example: Team Coding Standard Skill

```markdown
---
name: team-standards
description: Enforce our team's coding standards.
---

# Team Coding Standards

## Error Handling
- All async operations must have try-catch
- rule_id: `missing-error-handling`
- severity: `warning`

## Logging
- Use the shared `logger` module, not console.log
- rule_id: `console-log`
- severity: `warning`

## Imports
- No relative imports beyond 2 levels (use path aliases)
- rule_id: `deep-relative-import`
- severity: `info`
```

## Non-Goals

Skills intentionally do **not** support:

- **Gating thresholds** — those live in `.clausura.yaml` where CI config belongs
- **Tool definitions** — skills can't grant new tool permissions; tool allowlist is in config
- **Version management** — use git tags on your skill repository for versioning
- **Hot reloading** — skills are loaded at run time from the filesystem

## Next

→ [See skills in action (scenarios)](scenarios.md)
→ [Browse example skills](https://github.com/liuyanghejerry/Clausura/tree/main/examples/skills)
