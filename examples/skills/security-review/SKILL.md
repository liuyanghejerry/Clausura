---
name: security-review
description: 通用安全代码审查。检查 SQL 注入、XSS、硬编码密钥、不安全依赖等常见安全问题。
---

# 安全代码审查

请审查代码变更中的以下安全问题。对每个发现使用精确的 `rule_id`。

## 检查清单

### SQL 注入
- 任何字符串拼接构造的 SQL 查询
- 未使用参数化查询的数据库调用
- rule_id: `sql-injection`
- severity: `error`

### XSS 漏洞
- 直接将用户输入插入 HTML（innerHTML、document.write）
- 未转义的模板变量输出
- rule_id: `xss`
- severity: `error`

### 硬编码凭证
- 代码中的 API key、密码、token
- 配置文件中的明文密钥（排除示例/文档）
- rule_id: `hardcoded-secret`
- severity: `error`

### 输入验证缺失
- API 端点未验证用户输入类型和范围
- 文件上传未检查类型和大小
- rule_id: `missing-validation`
- severity: `warning`

### 不安全依赖
- 使用已知有 CVE 的依赖版本
- 直接从不可信源加载脚本
- rule_id: `insecure-dependency`
- severity: `warning`

## 输出格式

对每个发现按以下 JSON 格式输出，放在 `findings` 数组中：

```json
{
  "rule_id": "sql-injection",
  "severity": "error",
  "message": "在 login.js:15 发现 SQL 注入漏洞：用户输入直接拼接到 SQL 查询中",
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
