# Clausura 端到端实践指南

> 适用版本: clausura 1.0.0 (commit: 475c790)
> 仓库: https://github.com/clausura/clausura

## 场景设定

假设你维护一个 Rust Web 项目 `acme/api-server`。团队提交了一个 PR，里面混了两个典型问题：

1. **SQL 注入漏洞** -- 代码用 `format!` 拼接 SQL 字符串，没有参数化查询
2. **生产代码里用了 `unwrap()`** -- 虽然不是每次都会崩溃，但不符合团队规范

你的目标是在 PR 合并到 `main` 之前自动拦截这些问题。Clausura 作为 CI 中的一个门禁步骤，跑 LLM Agent 审查 diff，产出结构化结果，然后由确定性规则引擎决定是否放行。

## 第一步：编写任务配置

在项目根目录创建 `.clausura.yaml`：

```yaml
version: "1"
task:
  name: pr-code-review
  description: "Pull request code review for api-server"
  model: gpt-4o
  vendor: openai
  prompt_template: |
    You are reviewing a pull request for a Rust web application at {{repo}}.
    The current commit is {{commit_sha}} on branch {{branch}}.

    Review the git diff for:
    1. SQL injection vulnerabilities (concatenated SQL strings, missing parameterization)
    2. Use of `unwrap()` in non-test production code
    3. Missing error handling

    For each issue, output a JSON finding with:
    - rule_id: "no-sql-injection" for SQL injection, "no-unwrap-in-production" for unwrap in prod code
    - severity: "error" for SQL injection, "warning" for unwrap
    - message: clear description of what's wrong
    - location: file path and line numbers
    - evidence: the problematic code snippet
  token_budget: 16000
  timeout_secs: 120
  ambiguity_policy: fail_closed
  gating:
    - rule: no-sql-injection
      description: "SQL injection is a blocker - must use parameterized queries"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: no-unwrap-in-production
      description: "unwrap() in production code should be avoided"
      min_severity: warning
      max_findings: 0
      action: fail
```

关键设计点：

- `gating` 下定义了两个规则，rule id 需要和 LLM 输出的 `rule_id` 对应
- `no-sql-injection` 是零容忍，发现一个就 `fail`
- `no-unwrap-in-production` 也是零容忍，但 severity 设为 warning
- `ambiguity_policy: fail_closed` 意味着 LLM 输出格式不对时，宁可阻断也不要放过

## 第二步：本地验证

在推送 CI 之前，先在本地验证配置和效果。

```bash
# 1. 验证配置文件语法
clausura run --validate-config
```

预期输出：

```
[1/2] Loading configuration...
[2/2] Validating configuration...
OK: Configuration is valid
```

如果配置有问题，会返回到 exit code 3，并输出具体错误。

```bash
# 2. 预览执行计划（不调用 LLM）
clausura run --dry-run
```

输出样例：

```
[1/2] Loading configuration...
[2/2] Planning execution...

Task Plan:
  Name: pr-code-review
  ID: task-pr-code-review
  Model: gpt-4o
  Vendor: openai
  Token budget: 16000
  Timeout: 120s
  Gating rules: 2
```

dry-run 不会调用 LLM，只展示解析后的配置，适合用来确认 `gating` 规则是否按预期加载。

```bash
# 3. 实际执行（需要 API key）
export CLAUSURA_API_KEY=sk-...
clausura run
```

## 第三步：CI 集成

### GitHub Actions

```yaml
name: Code Review

on:
  pull_request:
    branches: [main]

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 2  # 需要 git history 才能做 diff

      - uses: clausura/clausura@v1
        with:
          config: .clausura.yaml
          api_key: ${{ secrets.OPENAI_API_KEY }}
          model: gpt-4o
          vendor: openai
          token_budget: 16000
          timeout: 120
```

Clausura 的 GitHub Action 会自动检测 `GITHUB_ACTIONS` 环境变量，提取仓库名、PR 号、commit SHA 和分支名，填充到 `prompt_template` 的模板变量中。

### GitLab CI

```yaml
code-review:
  image: ghcr.io/clausura/clausura:latest
  script:
    - clausura run --config .clausura.yaml
  variables:
    CLAUSURA_API_KEY: $OPENAI_API_KEY
    CLAUSURA_MODEL: "gpt-4o"
```

### Jenkins

```groovy
stage('Code Review') {
    environment {
        CLAUSURA_API_KEY = credentials('llm-api-key')
    }
    steps {
        sh 'clausura run'
    }
}
```

三种 CI 平台走的是同一个二进制，区别只在于环境变量的传递方式。Clausura 自动识别 CI 类型，无需额外配置。

## 第四步：预期输出

### 成功场景（无问题发现）

如果你的 PR 是干净的，运行后日志类似：

```
[1/4] Loading configuration...
[2/4] Initializing agent...
[3/4] Executing task...
  pr-code-review
[4/4] Processing results...
Success: Task completed successfully
  Findings: 0 | Exit: 0 | Tokens: 2340 | Duration: 5500ms
```

`exit 0` 表示所有门禁规则通过。SARIF 文件（默认输出到 `clausura-output.sarif`）内容如下：

```json
{
  "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
  "version": "2.1.0",
  "runs": [
    {
      "tool": {
        "driver": {
          "name": "Clausura",
          "informationUri": "https://github.com/clausura/clausura"
        }
      },
      "results": []
    }
  ]
}
```

### 失败场景（发现 SQL 注入）

假设 PR 引入了这样的代码：

```rust
fn get_user_by_id(conn: &Connection, user_id: &str) -> Result<User, Error> {
    let sql = format!("SELECT * FROM users WHERE id = '{}'", user_id);
    conn.query(&sql)
}
```

LLM 会返回 findings，Clausura 的输出：

```
[1/4] Loading configuration...
[2/4] Initializing agent...
[3/4] Executing task...
  pr-code-review
[4/4] Processing results...
Error: Task failed
  Findings: 2 | Exit: 1 | Tokens: 3450 | Duration: 7200ms
```

`exit 1` 表示有 rules 触发了 `action: fail`。对应的 SARIF 文件：

```json
{
  "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
  "version": "2.1.0",
  "runs": [
    {
      "tool": {
        "driver": {
          "name": "Clausura",
          "informationUri": "https://github.com/clausura/clausura"
        }
      },
      "results": [
        {
          "ruleId": "no-sql-injection",
          "level": "error",
          "message": {
            "text": "SQL injection vulnerability: user input `user_id` is concatenated into SQL query string via `format!`. Use parameterized queries instead."
          },
          "locations": [
            {
              "physicalLocation": {
                "artifactLocation": {
                  "uri": "src/db/users.rs"
                },
                "region": {
                  "startLine": 42,
                  "endLine": 43
                }
              }
            }
          ]
        },
        {
          "ruleId": "no-unwrap-in-production",
          "level": "warning",
          "message": {
            "text": "`unwrap()` used on `parse` result in production code. Use `match` or `?` for proper error handling."
          },
          "locations": [
            {
              "physicalLocation": {
                "artifactLocation": {
                  "uri": "src/handlers/users.rs"
                },
                "region": {
                  "startLine": 78,
                  "endLine": 78
                }
              }
            }
          ]
        }
      ]
    }
  ]
}
```

GitHub Advanced Security 会消费这个 SARIF 文件，把问题直接标注在 PR diff 上 -- 每一行被标记的代码旁边会出现注释，开发者不用看 CI 日志，直接在 PR 页面上就能看到 "SQL injection vulnerability" 之类的提示。

## 第五步：解读结果

| Exit Code | 含义 | 对应操作 |
|-----------|------|---------|
| 0 | 通过 | 合并按钮变绿，CI 放行 |
| 1 | 阻断 | PR 被拦截，开发者修复后重新推送 |
| 2 | 运行时错误 | CI 自身出问题了（超时、API 配额不足、网络错误），需要运维介入 |
| 3 | 配置错误 | 检查 `.clausura.yaml` 格式和必填字段 |

`exit 2` 的典型场景：

- LLM API 返回 429（限流）
- 请求超时（diff 太大或网络延迟）
- `CLAUSURA_API_KEY` 未设置

`exit 3` 的典型场景：

- `version` 字段缺失
- `task.model` 未设置且 `CLAUSURA_MODEL` 环境变量也不存在
- `task.token_budget` 或 `task.timeout_secs` 为 0

## 常见问题

**问：为什么不直接用 GitHub Copilot / CodeRabbit？**

Clausura 是自托管的，模型、规则、提示词完全由你控制。你可以把它接上任何 OpenAI 兼容的 API（包括本地模型），数据不出本地网络。门禁规则是确定性的，不存在 "今天通过了明天通不过" 的情况。

**问：能用本地模型吗？**

可以。设置 `vendor: ollama`、`model: llama3`，然后 `CLAUSURA_API_KEY=ollama` 即可。Clausura 使用 OpenAI 兼容的 API 协议，Ollama 暴露的接口天然兼容。

**问：token 预算怎么定？**

经验值：diff 在 500 行以内用 8000，500 到 2000 行用 16000。保守一点就加 50%。如果超了，LLM 上下文会被截断，Agent 循环也会提前终止 -- 所以预算设大一点比设小一点好。

**问：同一次提交结果会变吗？**

规则引擎是纯确定性的：相同的 findings 走相同的规则，产出相同的 exit code。LLM 输出每次可能有微小差异（措辞不同、是否发现边缘问题），但 `exit_code` 不会出现 0 和 1 之间的摇摆 -- 除非 diff 本身触到了 LLM 的判断边界。如果对稳定性要求极高，可以考虑用 `proceed_with_caution` 策略处理边界情况。

**问：能跳过某些规则吗？**

可以在配置里把规则的 `action` 改成 `warn` 或 `ignore`，这样即使触发也不会阻断 CI。适合渐进式推行新规则的场景：先用 `warn` 跑一段时间，等团队适应了再切到 `fail`。

## 更多场景

上面的例子是代码审查，但 Clausura 的架构不限于此。换了 prompt 和 rules，可以做完全不同的事。

- **国际化翻译检查** -- 扫描源代码文件，检查 `t!("...")` 调用是否在翻译文件里有对应条目。规则设为 `max_findings: 0, action: fail`，确保不会漏掉翻译。
- **依赖版本一致性** -- 跨多个 crate 或仓库检查同一个依赖的版本号是否一致，防止依赖冲突。
- **代码风格合规** -- 虽然不是 linter 的替代品，但可以检查一些 linter 管不到的约定，比如 "TODO 注释必须带 issue 编号"。
- **架构合规** -- 检查 `http::` 模块是否直接引用了 `db::` 模块，强制分层架构。

每个场景只需要换 `.clausura.yaml` 里的 `prompt_template` 和 `gating` 规则，CI 配置不用动。
