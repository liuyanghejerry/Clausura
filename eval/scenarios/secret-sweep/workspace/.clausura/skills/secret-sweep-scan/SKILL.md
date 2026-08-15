---
name: secret-sweep-scan
description: 大规模扫描硬编码凭证，忽略占位符
---

# 硬编码凭证扫描

先用 `git_diff`（参数 `{"base": "HEAD~1"}`）查看变更范围，再对全部变更文件
扫描硬编码凭证。

## 判定标准

- rule_id: `hardcoded-secret`
- severity: `error`

**报告**：看起来像真实凭证的值（`sk-...`、`ghp_...`、`AKIA...` 等已知密钥
前缀，或明确的 `api_secret`/`EXPORT_TOKEN` 赋值给非占位符值）。

**忽略（不得报告）**：占位符与示例值，如 `REPLACE_ME`、`your-api-key-here`、
`example-*`、`CHANGE_ME_*`、`placeholder-*`、`dummy-*`、`not-a-real-*`、
`<YOUR_SECRET>`。

文件较多（10 个服务），请系统地覆盖全部文件——grep 一次全仓扫描比逐个
read_file 更高效。
