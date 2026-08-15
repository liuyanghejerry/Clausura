---
name: python-review
description: Python 遗留代码审查：bare except、SQL 注入、反序列化、密钥、调试输出
---

# Python 遗留代码审查

先用 `git_diff`（参数 `{"base": "HEAD~1"}`）查看本次变更。

## 规则

### bare-except（error）
- 裸 `except Exception:` / `except:` 后静默吞掉错误（仅 `pass` 或无处理）
- 有日志或显式处理的除外
- rule_id: `bare-except`

### sql-injection（error）
- f-string / 字符串拼接构造 SQL
- rule_id: `sql-injection`

### unsafe-deserialization（error）
- `pickle.loads()` 反序列化不可信输入
- rule_id: `unsafe-deserialization`

### hardcoded-secret（error）
- 源码中硬编码的 API key、密码
- rule_id: `hardcoded-secret`

### debug-print（info）
- 生产代码中遗留的 `print()` 调试输出
- rule_id: `debug-print`

每个 finding 附带 location（文件 + 行号）与证据（代码片段）。
