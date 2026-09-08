---
name: security-review
description: 检查 SQL 注入、XSS、硬编码密钥
---

# 安全代码审查

审查代码变更中的安全问题。对每个问题输出一条 finding：

## SQL 注入
- 任何字符串拼接构造的 SQL 查询
- rule_id: `sql-injection`
- severity: `error`

## XSS
- 用户可控内容未经转义注入 DOM（如 innerHTML）
- rule_id: `xss`
- severity: `error`

## 硬编码密钥
- 源码中的 API key、密码、token
- rule_id: `hardcoded-secret`
- severity: `error`

只报告实际存在的问题；没有问题时返回空 findings。

先用 `git_diff`（参数 `{"base": "HEAD~1"}`）查看本次变更。
