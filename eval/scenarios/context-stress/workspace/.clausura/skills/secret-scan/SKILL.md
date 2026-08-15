---
name: secret-scan
description: 扫描硬编码密钥与凭证
---

# 硬编码凭证扫描

- 源码或数据文件中的 API key、password、token
- rule_id: `hardcoded-secret`
- severity: `error`

只报告实际存在的凭证；没有时返回空 findings。

先用 `git_diff`（参数 `{"base": "HEAD~1"}`）查看本次变更。
