---
name: rust-review
description: Rust 服务审查：panic、SQL 注入、密钥、错误吞没、遗留标记
---

# Rust 服务代码审查

先用 `git_diff`（参数 `{"base": "HEAD~1"}`）查看本次变更。

## 规则

### panic-unwrap（error）
- 对可失败的输入使用 `unwrap()` / `expect()`：用户输入解析、Option 解包等
- 会导致服务 panic 的位置
- 注意：同一个语句里的多个 unwrap 算一个 finding
- rule_id: `panic-unwrap`

### sql-injection（error）
- 字符串拼接构造 SQL（`format!` 拼 SQL、`+` 拼 SQL）
- 应使用参数化查询
- rule_id: `sql-injection`

### hardcoded-secret（error）
- 源码中硬编码的密码、连接串凭据、API key
- rule_id: `hardcoded-secret`

### swallowed-error（error）
- 用 `let _ =` 静默丢弃 Result（无日志、无处理）
- rule_id: `swallowed-error`

### todo-marker（warning）
- 生产代码中遗留的 `todo!()` / `unimplemented!()`
- rule_id: `todo-marker`

每个 finding 附带 location（文件 + 行号）与证据（代码片段）。
