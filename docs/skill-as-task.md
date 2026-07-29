# Skill-as-Task: 社区 Skill 复用

> 状态：设计中
> 版本：target v1.x

## 动机

Clausura 的核心价值是 "LLM 审查 + 确定性门禁"。目前引擎（agent loop、rule engine、SARIF 输出）已经成熟，但**内容层（审查知识）完全由用户从零手写**。每个项目都要自己写 prompt，导致：

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

| 引用形式 | 解析规则 | 示例 |
|----------|---------|------|
| 本地文件路径 | 相对 workspace 或绝对路径 | `./skills/check.md`, `/path/to/skill.md` |
| 命名引用 | 按顺序查找：`.clausura/skills/<name>/SKILL.md` → `~/.clausura/skills/<name>/SKILL.md` | `team/vue-check` |
| 远程 URL | HTTPS 下载，缓存到 `.clausura/cache/skills/` | `https://.../security.md` |

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

## 非目标

- ❌ Clausura 不定义自有的 skill 格式
- ❌ 不内置 skill 注册中心/registry
- ❌ skill 中不包含 gating 定义
- ❌ 不支持 skill 的热更新或版本管理（先用文件路径，版本用 git tag）
- ❌ 不支持工具型 skill（依赖外部 CLI/API 的 skill 无法在 Clausura 沙盒中运行）

## 改动范围

| 模块 | 改动 |
|------|------|
| `config.rs` | `YamlTaskConfig` 加 `skill_prompts` 字段；加载时解析并合并到 `prompt_template` |
| `skills.rs` | 新增：skill 文件解析、frontmatter 剥离、多种引用方式的解析 |
| `types.rs` | 无需改动（skill 内容合并到现有 `prompt_template` 字段） |
| `agent.rs` | 无需改动（system prompt 构建逻辑不变） |

## 后续演进

- 远程 skill 的版本锁定（`community/security-review@v1.2`）
- Skill 级别的 tool_allowlist 覆盖
- `--skill` CLI 快捷参数
