---
name: i18n-check
description: 国际化完整性检查。检查翻译 key 是否缺失、硬编码文本、locale 文件一致性。
---

# i18n 国际化检查

检查代码中的国际化（i18n）问题。Locale 文件通常位于 `locales/` 或 `i18n/` 目录。

## 检查清单

### 硬编码文本
- UI 中直接出现的非英文文本（排除注释和日志）
- 应使用 i18n key 替代的字符串字面量
- rule_id: `hardcoded-text`
- severity: `warning`

### 翻译 Key 缺失
- 代码中引用了但 locale 文件中不存在的 key
- rule_id: `missing-translation-key`
- severity: `error`

### 翻译文件不一致
- 不同 locale 文件之间 key 数量不一致
- 某个 locale 缺少其他 locale 存在的 key
- rule_id: `locale-mismatch`
- severity: `warning`

### 未使用的翻译 Key
- locale 文件中定义了但代码中未引用的 key
- rule_id: `unused-translation-key`
- severity: `info`

## 输出格式

```json
{
  "rule_id": "missing-translation-key",
  "severity": "error",
  "message": "key 'checkout.confirm' 在 en.json 中存在但在 zh-CN.json 中缺失",
  "evidence": "en.json L42: \"checkout.confirm\": \"Confirm order\"",
  "location": { "file": "locales/zh-CN.json", "line_start": 1, "line_end": 1, "column_start": 1, "column_end": 1 }
}
```
