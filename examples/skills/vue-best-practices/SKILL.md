---
name: vue-best-practices
description: Vue 2/3 代码规范检查。包括组件命名、Props 校验、Composition API 规范等。
---

# Vue 代码规范审查

审查 Vue 组件（`.vue` 文件）中违反最佳实践的模式。

## 检查清单

### 组件命名
- 组件名未使用 PascalCase 或多词命名（Vue 官方风格指南要求）
- rule_id: `vue-component-name`
- severity: `warning`

### Props 校验
- Props 未声明 type 或 validator
- rule_id: `vue-missing-prop-validation`
- severity: `warning`

### 避免 `as any`
- TypeScript 中使用 `as any` 绕过类型检查
- rule_id: `no-as-any`
- severity: `warning`

### Options API vs Composition API
- 同一组件中混用 Options API 和 Composition API（`<script setup>` 中不应出现 `data()`、`methods:`）
- rule_id: `vue-mixed-api-style`
- severity: `info`

### 模板中的复杂表达式
- `<template>` 中包含超过一行的 JavaScript 表达式（应提取为 computed）
- rule_id: `vue-complex-template`
- severity: `info`

## 输出格式

```json
{
  "rule_id": "no-as-any",
  "severity": "warning",
  "message": "在 PaymentForm.vue 中发现 as any 类型断言",
  "evidence": "const data = response as any",
  "location": { "file": "src/components/PaymentForm.vue", "line_start": 42, "line_end": 42, "column_start": 13, "column_end": 35 }
}
```
