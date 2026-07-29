# Skill-as-Task: 社区 Skill 复用

> 状态：已实现 (v1.2.0+)
> 实现模块：`crates/clausura-core/src/skills.rs` + `crates/clausura-core/src/config.rs`

## 动机

Clausura 的核心价值是 "LLM 审查 + 确定性门禁"。引擎（agent loop、rule engine、SARIF 输出）已经成熟，但**内容层（审查知识）完全由用户从零手写**。每个项目都要自己写 prompt，导致：

- 审查质量取决于开发者的领域知识（初级工程师可能遗漏关键检查项）
- 相同的审查逻辑无法跨项目复用
- 团队规范难以沉淀和分发

与此同时，社区（Claude Code / Codex CLI / pi）已经积累了大量的 skill 文件——这些 skill 专注于 "如何审查 / 如何检查"，以 Markdown 格式分发。但它们**不包含 gating rules**，因为 "过不过" 是每个团队自己的决策。

## 核心设计

**Clausura 作为社区 skill 的消费者，而非 skill 格式的定义者。**

```
社区 Skill（Markdown）           Clausura Gating（YAML）
───────────────────────         ──────────────────────
"如何检查 SQL 注入"              →  发现 1 个 error？fail
"如何检查 i18n 缺失"             →  发现 3 个 warning？warn
"如何检查架构分层"               →  发现 5 个 info？ignore
```

- Skill 回答 "查什么、怎么查"
- Gating 回答 "多少算不过"
- 两者完全解耦

## 用户场景

### 场景 1：直接引用社区 skill

```yaml
version: "1"
task:
  name: security-review
  model: gpt-4o
  vendor: openai
  skill_prompts:
    - community/security-review    # 复用社区的审查知识
  gating:                          # 门禁用户自己定
    - rule: sql-injection
      min_severity: error
      max_findings: 0
      action: fail
```

### 场景 2：组合多个 skill

```yaml
task:
  skill_prompts:
    - ./skills/react-best-practices.md   # 本地文件
    - team/no-any-type                   # 团队内部 skill
    - community/i18n-check               # 社区 skill
  gating:
    - rule: no-any-type
      max_findings: 0
      action: fail
    - rule: missing-i18n
      max_findings: 3
      action: warn
```

### 场景 3：skill + 自己的补充 prompt

```yaml
task:
  skill_prompts:
    - community/security-review
  prompt_template: |               # 用户追加的审查项
    另外检查：所有 API 调用必须有 error handling。
  gating:
    - rule: missing-error-handling
      max_findings: 0
      action: fail
```

### 场景 4：渐进式采用

```
第一步：直接用社区 skill，gating 全设 max_findings: 0
第二步：根据实际误报调整 gating 阈值
第三步：沉淀自己的审查经验为内部 skill
```

## Skill 引用解析

| 引用形式 | 解析规则 | 示例 | 状态 |
|----------|---------|------|------|
| 本地文件路径 | 先匹配原样路径，再匹配 workspace 相对路径 | `./skills/check.md`, `/path/to/skill.md` | ✅ 已实现 |
| 命名引用 | 按顺序查找：`.clausura/skills/<name>/SKILL.md` → `~/.clausura/skills/<name>/SKILL.md` | `team/vue-check` | ✅ 已实现 |
| 远程 URL | HTTPS 下载，缓存到 `.clausura/cache/skills/` | `https://.../security.md` | ❌ 未实现 |

## Skill 文件格式

Clausura 不定义自己的格式。社区 skill 的标准格式是 Markdown + 可选 YAML frontmatter：

```markdown
---
name: security-review
description: 安全代码审查，检查 SQL 注入、XSS、硬编码密钥
---

# 审查规则

1. SQL 注入 — 任何字符串拼接的 SQL 查询
...
```

Clausura 读取时：
- 如果有 frontmatter（`---` 包裹），剥离后只取 body 作为 prompt 内容
- 如果没有 frontmatter，整个文件即为 prompt 内容
- 多个 skill 按声明顺序拼接，各自标明来源

## 注入格式

多个 skill 的 prompt 内容合并后注入到 system prompt，格式如下：

```
[Skill: community/security-review]
<skill 内容>

[Skill: team/vue-best-practices]
<skill 内容>

---

<用户 prompt_template>
```

当 `prompt_template` 为空或为默认值 `{{task_description}}` 时，用户模板段省略。

## 实现总结

| 模块 | 改动 |
|------|------|
| `skills.rs` | 新增模块：`resolve_skill()` 三级解析（本地→workspace→命名引用）、`strip_frontmatter()` YAML frontmatter 剥离、`merge_prompts()` 多 skill 拼接 |
| `config.rs` | `YamlTaskConfig` 加 `skill_prompts: Vec<String>` 字段；`resolve_config()` 在加载时解析所有 skill 引用并合并到 `prompt_template` |
| `types.rs` | 无需改动 — skill 内容直接合并到现有的 `prompt_template` 字段 |
| `agent.rs` | 无需改动 — system prompt 构建逻辑不变 |

### 关键实现细节

**`resolve_skill(name_or_path, workspace)`** — 解析顺序：
1. 如果 `name_or_path` 作为路径直接存在 → 加载（支持绝对路径和 cwd 相对路径）
2. 否则拼接到 `workspace` 下 → 如果存在则加载
3. 否则如果不含 `://` 且非绝对路径 → 作为命名引用查找 `.clausura/skills/` 和 `~/.clausura/skills/`
4. 以上均失败 → `ConfigError::FileNotFound`

**`strip_frontmatter(content)`** — 剥离逻辑：
- 检测开头 `---\n`，找到下一个 `\n---\n` 作为结束标记
- 提取 body 部分并 `trim_start`
- 无 frontmatter 或格式不正确时返回原内容（不报错，容错处理）

**`merge_prompts(skill_contents, template)`** — 合并格式：
- 每个 skill 前加 `[Skill: <name>]` 头
- skill 之间以 `\n\n---\n\n` 分隔
- 用户 `prompt_template` 追加在末尾（仅当非空且非 `{{task_description}}` 默认值时）

## 非目标

- ❌ Clausura 不定义自有的 skill 格式
- ❌ 不内置 skill 注册中心/registry
- ❌ skill 中不包含 gating 定义
- ❌ 不支持 skill 的热更新或版本管理（先用文件路径，版本用 git tag）
- ❌ 不支持工具型 skill（依赖外部 CLI/API 的 skill 无法在 Clausura 沙盒中运行）

## 后续演进

- 远程 skill 的下载与缓存（`https://...` URL 支持）
- 远程 skill 的版本锁定（`community/security-review@v1.2`）
- Skill 级别的 tool_allowlist 覆盖
- `--skill` CLI 快捷参数
