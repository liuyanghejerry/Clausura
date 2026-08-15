---
name: ts-review
description: TypeScript monorepo 审查：XSS、SQL 注入、密钥、any、console.log
---

# TypeScript monorepo 代码审查

先用 `git_diff`（参数 `{"base": "HEAD~1"}`）查看本次变更。

## 规则

### xss（error）
- 用户可控内容未经转义注入 DOM：`innerHTML` 直接赋值、
  `dangerouslySetInnerHTML` 使用不可信 HTML
- rule_id: `xss`

### sql-injection（error）
- 字符串模板/拼接构造 SQL 查询
- rule_id: `sql-injection`

### hardcoded-secret（error）
- 源码中硬编码的 API key、签名密钥
- rule_id: `hardcoded-secret`

### no-explicit-any（warning）
- 显式 `any` 类型标注或断言（`as any`、`<any>`、泛型默认 any 等）
- rule_id: `no-explicit-any`

### console-log（info）
- 生产代码中的 `console.log` 调试输出
- rule_id: `console-log`

每个 finding 附带 location（文件 + 行号）与证据（代码片段）。
