# Clausura 自动 Compact（对话摘要压缩）的价值与成本调研

Status: research / proposal（本分支仅调研，无代码改动）
Branch: `research/auto-compact`
Date: 2026-08-04

## 1. 结论先行（TL;DR）

- **现状**：Clausura 超预算时的处理是"截断 + 归档 + 提示"。即直接丢弃旧消息（`truncate`），
  把被丢的消息归档到 `.clausura/archives/`，并在对话里注入一条"上下文被裁剪，可用
  `read_file` 查看归档"的 User 提示。这是**有损**的：模型会忘掉早期迭代里已经发现的
  findings、已经看过的文件、正在验证的假设。
- **auto-compact 的定义**：在触发截断时，额外用一次 LLM 调用把将被丢弃的消息**总结**成一段
  紧凑摘要，把摘要注入对话（替代裸丢弃），原始消息仍然归档。模型在后续迭代里"记得"
  发生过什么，代价是每次 compact 多一次 LLM 调用（token 成本 + 延迟 + 非确定性）。
- **价值判断**：价值**真实但集中**在两类场景——(a) 工具调用多、对话长的任务；(b) token_budget
  配得比模型上下文小很多的任务。对短任务、大预算（128k+ 上下文）任务，收益趋近于零。
- **成本判断**：实现成本中等（约 3–5 人日，若先做"确定性 findings 台账"降级版约 1 人日）；
  每次 compact 的 token/延迟开销占比小（约 +3–10% 计费 token，+5–20s 延迟），
  但引入了一块新的非确定性表面，与"deterministic gating"哲学需要谨慎调和。
- **建议**：做成**默认关闭的可选开关** `auto_compact: true`，且**失败必须优雅降级**到现有
  截断+归档逻辑（compact 失败不能让任务失败）。若追求最低成本，可先做 Phase 0 的
  "确定性 findings 台账"（不调 LLM，把对话里已有的 findings JSON 汇总成台账注入），
  再评估是否需要 Phase 1 的 LLM 摘要。

---

## 2. 现状梳理（代码事实）

### 2.1 触发条件与截断算法

- `token_budget` 默认 **32000**（`crates/clausura-core/src/config.rs` `default_token_budget`，
  可经 `CLAUSURA_TOKEN_BUDGET` / `--token-budget` / YAML 覆盖）。
- `ContextManager`（`crates/clausura-core/src/context.rs`）：
  - `count_tokens`：`provider.count_tokens(content)` 之和 + 每条消息 1 token 的固定开销。
  - token 计数是**启发式**：OpenAI 系 `len/3`，Anthropic `len/3.5`，Custom `len/4`
    （`provider.rs`）。不是 tiktoken，只用于预算控制，不用于计费。
  - `should_truncate`：用量 > **80%** 预算时触发。
  - `truncate`：二分查找，保留"系统消息 + 能装进 **75%** 预算的尾部 N 条"，**保证
    assistant(tool_calls)→tool 成对不被拆开**，其余全部丢弃。
  - `keep_last_n` 是纯保留尾部，**没有任何摘要/压缩**。

### 2.2 截断后的处理（`crates/clausura-core/src/agent.rs`）

截断后：

1. 被丢弃消息写入 `.clausura/archives/context-dump-{task_id}-{seq}.log`（JSON Lines，
   逐条可回放）。
2. 在**系统消息之后（index 1）**注入一条 User 提示：
   "⚠️ Context was trimmed... N earlier messages are archived at ... Use `read_file` to
   inspect if you need context from earlier iterations."
3. 若截断后仍超预算 → break，整个 run 标记 `truncated = true`，走
   `extract_findings_lenient`（尽力而为）→ 默认 `on_incomplete: fail` 退出码 2。

### 2.3 相关约束（实现 compact 时必须遵守的既有规则）

- **消息配对**：OpenAI/Anthropic API 拒绝"有 tool_calls 的 assistant 后面没有对应 tool
  结果"。截断与注入都必须维持这一不变式（现有代码在 `keep_last_n` 与 hint 注入位置都
  专门处理过）。
- **Anthropic 消息转换**：`provider.rs::convert_messages_to_anthropic` 把 Tool 消息转成
  `tool_result`，且 `tool_use_id` 硬编码为 `"unknown"`——**喂给摘要调用的被丢弃 Tool 消息
  在 Anthropic 侧本来就配对不完整**，这是做摘要时的一个已知脆弱点。
- **计费**：`max_total_tokens` 只统计 agent 循环里的 `chat_with_tools` 调用；若 compact
  调用不计入该上限，用户会看到"没到上限却多花钱"；若计入，则 compact 会挤占 agent 迭代
  的预算，需要明示。
- **快照/断点**：`SnapshotManager` 保存的是消息数组；compact 后的摘要作为 User 消息自然
  进入快照，`--resume` 无需特殊处理（但恢复后的后续 compact 触发仍按同一套逻辑）。
- **工具清单**：read_file、git_diff、shell_exec、list_files、grep（5 个内置工具）。
  摘要调用本身不应暴露任何工具（无 tool_calls）。

---

## 3. 什么是"给 Clausura 加 auto-compact"

在 agent 循环里，把现有的"截断→归档→提示"升级为：

```
触发（>80% 预算）
  ├─ 取将被丢弃的消息段（从 index 1 到保留尾部起点）
  ├─ 1 次 LLM 摘要调用（无工具）：输入 = 被丢消息 + 摘要指令，输出 = 紧凑摘要
  ├─ 摘要注入 index 1（User 角色），替换"纯提示"；被丢消息照旧归档
  └─ 摘要失败/超时 → 回退到现有"提示 + 归档"逻辑（优雅降级，不 fail run）
```

可配置项（建议）：`auto_compact: true/false`（默认 false）、摘要输出预算上限
（如 ≤ token_budget 的 10%）、是否把 compact 计费计入 `max_total_tokens`。

---

## 4. 价值分析（Value）

### 4.1 直接价值：保住"过程记忆"，减少有损截断导致的错误结局

当前截断的**真实损失模式**（都是 CI 里踩过的坑）：

1. **丢失已发现 findings**：任务在第 6 轮迭代触发截断，第 1–5 轮发现的 finding 全部丢出
   上下文。模型在 final Stop 时可能给出空 findings（它真不记得了），或由于提示才去
   `read_file` 翻归档——但"它得先想起来要去翻、还得知道翻哪一段"。结果是：
   - `on_incomplete: pass` 下：空 findings 通过 `max_findings: 0` 门禁 → **假通过**；
   - `on_incomplete: fail` 下：跑完但结果残缺仍退出 2 → **假失败**。
2. **重复劳动 → 迭代耗尽**：截断后模型忘了看过哪些文件、跑过哪些 git diff，
   重新执行相同工具调用，`max_iterations` 被空耗 → `FinishReason::Length` 或迭代上限
   → incomplete。

auto-compact 直接缓解上述两种结局：摘要携带"已发现 findings 列表、已检查文件、当前假设"
进入后续迭代。对话在语义上是连续的，agent 更可能以干净的 `Stop` 结束并产出完整 findings。

### 4.2 间接价值

- **与现有归档机制正交**：归档照旧，摘要只是"上下文内副本"，审计/回放能力不降级。
- **可验证性**：摘要进对话、原始消息进归档，两者可对比，compact 质量可事后审计。
- **门禁确定性不受影响**：gating 仍是纯规则引擎（`rules.rs`），compact 只影响对话内容，
  不影响 findings 的确定性评估——与 "Why JSON findings instead of free-text" 的设计决策
  不冲突（前提：compact 失败永不改变 run 的 pass/fail 语义）。
- **多模型统一收益**：对 token 计数是启发式的三家 provider（OpenAI/Anthropic/Custom）
  都同样受益，不需要模型侧特判。

### 4.3 价值适用的场景边界（诚实评估）

| 场景 | 收益 |
|------|------|
| 长任务（>6 轮工具调用）、diff 大、预算 32k | **高**：这是目前 incomplete 假失败/假通过的重灾区 |
| 短任务、单轮工具调用、预算富余 | 接近零：大概率永远不会触发截断 |
| 128k+ 上下文模型 + 大预算 | 低：上下文空间大，截断触发概率低；真要控成本时另说 |
| 严格审计型 CI（每次运行都要可回放） | 中：归档仍在，摘要可审计，但引入 LLM 摘要这一非确定中间产物 |

---

## 5. 成本分析（Cost）

### 5.1 运行期成本（token / 延迟 / 计费）

以默认预算 32k 为例，触发点 80%（25.6k）、目标 75%（24k）。假设触发时上下文约 30k：

- 被丢弃 ≈ 30k − 24k = **约 6k token**（只多不少，如果一条大工具输出顶进来会更多）。
- 摘要调用：输入 ≈ 6k + 摘要指令 ~0.3k = **~6.3k**；输出按上限 10% 预算封顶 ~3.2k、
  实际通常 **0.5–1k**。单次 compact 合计 **~7–9k 计费 token**。
- 换算成本（粗算）：
  - DeepSeek 级别（$0.27/M in、$1.10/M out）：单次 ~**$0.003**；
  - GPT-4o 级别（$2.5/M in、$10/M out）：单次 ~**$0.02**；
  - 一次跑 2–3 次 compact 的任务：计费 token 增加 **~3–10%**（相对正常 60–200k 总量）。
- 延迟：6k token 输入的摘要调用约 **5–20s**（慢模型更长），且与 agent 迭代串行。
- 需要把 compact 调用计入 `max_total_tokens` 与 `timeout_secs` 的账——否则用户对
  "预算超支/超时"的预期会被悄悄打破。

结论：绝对成本小，但**不是零**，且与任务时长线性叠加；对按次计费/配额紧张的 CI 账户
需要在文档里给清楚预期。

### 5.2 工程质量成本（实现复杂度）

按文件逐项评估：

| 改动点 | 复杂度 | 说明 |
|--------|--------|------|
| `Provider` 增加摘要入口 | 低 | 复用 `chat`（无工具）；但需区分"agent 调用"与"compact 调用"的 usage 记账 |
| `agent.rs` 循环集成 | 中 | 在截断分支先尝试摘要；摘要消息注入 index 1（保持 assistant→tool 配对不变式） |
| 失败降级路径 | 中 | 摘要调用出错/超时/返回空 → 回退到现有"提示+归档"，**绝不能 fail run**；要写测试覆盖 |
| usage 记账 | 中 | 新 `Usage` 条目、计入 `max_total_tokens` 的开关、日志可观测（`tracing`） |
| Anthropic 转换 | 中 | `convert_messages_to_anthropic` 对 Tool 消息 `tool_use_id: "unknown"`，被丢 Tool 消息喂给摘要调用时配对本就残缺——需在喂给摘要前清洗或跳过纯 tool 结果 |
| 配置/校验/文档 | 低 | 新增字段的 serde default、YAML 校验、`docs/guide/*` 与 README 更新 |
| 测试 | 中 | `MockProvider` 需支持串行返回摘要响应；现有 13 处 `add_response` 用例要核对调用顺序；新增"compact 成功/降级/连续 compact"用例 |

净估：**Phase 1 约 3–5 人日**（含测试与文档）。

### 5.3 非确定性 / 正确性风险（最值得警惕的成本）

- **摘要是有损且非确定的**：摘要可能漏掉关键 finding，甚至"脑补"一个不存在的结论，
  后续迭代会信任它。这与 Clausura "bounded, deterministic" 的核心卖点有张力。
- **提示注入强化面**：被摘要的消息本身来自（不可信的）仓库内容，摘要模型的改写可能
  **强化**注入的指令。缓解：摘要只作 User 消息（不是 system）、摘要调用不给工具、
  gating 永不信任 LLM 散文。
- **摘要质量不可测**：没有客观指标；只能靠抽查归档对比。需要日志记录每次 compact 的
  摘要与被丢消息，供事后审计。

### 5.4 维护成本

- 每多一个开关就是一份长期兼容承诺（默认值、行为、文档）。
- 与后续"真正的 tokenizer / 自适应预算"等功能叠加时，触发逻辑会互相纠缠，需要保持
  单一路径（compact 只在"将要截断"这一种情况下发生）。

---

## 6. 备选方案（Alternatives）

| 方案 | 成本 | 效果 | 点评 |
|------|------|------|------|
| **A. 现状**：截断+归档+提示 | 0 | 有损；incomplete 结局多 | 基线 |
| **B. 确定性 findings 台账（Phase 0）** | ~1 人日 | 中 | **不调 LLM**：从对话中已有的 assistant findings JSON（`extract_findings` 已能解析）提取汇总，注入"截止目前 N 条 finding"台账。直接消灭"丢 findings"这个最大痛点，零额外计费、零非确定性 |
| **C. LLM 摘要 compact（Phase 1）** | 3–5 人日 | 高 | 本调研主体方案；可选开关 + 优雅降级 |
| **D. 只压缩工具输出** | 中 | 中 | 工具输出是上下文膨胀主因；已有 32KB/1000 行截断，可再降。但模型可能因此重新请求 → 需配合台账 |
| **E. 换真实 tokenizer / 调大预算** | 低 | 中 | tiktoken 让触发更可预测；128k 预算直接让截断少发生。是**今天最便宜的缓解手段**，应作为 compact 的前置/并行项 |
| **F. 混合**：B（台账）默认开 + C（摘要）可选 | 1 + 3–5 人日 | 最高 | 建议终态：台账兜底记忆，摘要增强连续性 |

---

## 7. 建议与实施路线

### 7.1 建议

1. **先做 Phase 0（确定性 findings 台账，~1 人日）**：零 LLM 成本、零非确定性，
   直接修复"截断丢 findings → 假通过/假失败"这一最痛问题。落地简单：在截断注入点，
   用现有 `extract_findings` 从保留/丢弃消息中提取 findings 摘要成台账注入。
2. **Phase 1（LLM 摘要 compact）做成默认关闭的可选开关**：
   - 配置：`auto_compact: true`（默认 false，行为零变化、向后兼容）；
   - 触发：与截断同一阈值（>80% 预算），一次任务最多 N 次（防死循环）；
   - 摘要上限：≤ token_budget 的 10%，且摘要后仍须通过 `should_truncate` 检查；
   - 记账：compact 调用单独记 usage，默认**计入** `max_total_tokens`，日志明示；
   - 降级：摘要失败/超时 → 回退现状（提示+归档），run 状态不受影响；
   - 注入位置：index 1，User 角色，维持 assistant→tool 配对不变式；
   - 摘要指令：要求**逐条保留 findings**（rule_id/severity/message），禁止发挥。
3. **并行做 E（预算默认值/文档）**：把 `token_budget` 默认值与模型上下文窗口对齐
   （如默认提到 64k），减少 compact 触发频次。

### 7.2 验收要点（若实施）

- 触发 compact 后，后续 `Stop` 响应的 findings 与归档内容对得上（抽查）。
- compact 调用失败、超时、连续触发时，run 的 exit code 与 `on_incomplete` 语义不变。
- `max_total_tokens` 上限在 compact 开启时仍精确生效。
- Anthropic vendor 下被丢 Tool 消息不导致摘要调用 400/配对错乱。
- `--resume` 恢复含摘要的对话后行为一致。
- 归档目录清理逻辑（成功时删除）不受影响。

---

## 8. 参考（代码位置）

- `crates/clausura-core/src/context.rs` — 预算跟踪、截断算法、归档
- `crates/clausura-core/src/agent.rs` — agent 循环、截断注入、findings 提取
- `crates/clausura-core/src/provider.rs` — provider trait、启发式 token 计数、Anthropic 消息转换
- `crates/clausura-core/src/config.rs` / `types.rs` — `TaskContract`、默认值、校验
- `crates/clausura-core/src/snapshot.rs` — 快照/断点（compact 摘要随消息自然持久化）
- `docs/guide/overview.md`、`docs/guide/troubleshooting.md`、`README.md` — 现状文档与
  "context exhausted" 排障路径
