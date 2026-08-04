# Clausura 自动 Compact（对话摘要压缩）的价值与成本调研

Status: implemented on this branch (originally research / proposal)
Branch: `research/auto-compact`
Date: 2026-08-04

> **Update (2026-08-04):** Phase 1 (LLM 摘要 compact) 已在本分支实现：
> `auto_compact`（默认 false）+ `max_compactions`（默认 3）已落地，含配额守卫与
> 失败降级。详见 §7.1 与 §9。本报告其余部分保留为设计依据与成本记录。
>
> **Update 2 (2026-08-04)**：新增**磁盘 findings ledger**（`findings_ledger`，默认 true）——
> 运行中每轮响应出现的 findings 追加到 `.clausura/archives/findings-ledger-{task_id}.jsonl`，
> 终局 Stop 时确定性合并回最终 findings（去重键 rule_id+location+message，最终响应优先）。
> 这是 compact 的**无损互补**：compact 保上下文连续性，ledger 保结果完整性，零 LLM 成本、
> 确定性、可审计。顺带修复了 ToolCalls 分支丢弃 assistant content 的信息丢失点。

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

## 3. 三者关系：token_budget / max_total_tokens / auto_compact

这三个概念历史上就被混淆过（v1.2.0 之前把累计计费 token 误当成上下文预算，导致
正常 run 被误判 incomplete，见 commit `1f88d76`）。建议在文档与 README 里把它们
显式区分开。

### 3.1 各自职责边界

| 概念 | 控制什么 | 默认值 | 触发后的行为 |
|------|---------|--------|-------------|
| `token_budget` | **单次请求**的上下文大小上限（system + 消息历史，含将来注入的摘要） | 32000 | 用量 >80% 触发截断，压回 75% 以内；压不住 → incomplete |
| `max_total_tokens` | **整次运行**累计计费 token 上限（所有 LLM 调用 `usage.total_tokens` 之和） | None（无上限） | 累计达到 → 停止 agent 循环 → incomplete |
| `auto_compact`（提案） | 在 `token_budget` 触发截断时，**用摘要替换裸丢弃**的处理策略 | false（默认关） | 触发一次 LLM 摘要调用，把被丢消息总结注入对话 |

一句话：`token_budget` 管"单次请求多大"，`max_total_tokens` 管"总共花多少"，
`auto_compact` 管"预算逼近时旧消息怎么处理"。前两者是**约束**，compact 是**策略**。

### 3.2 交互关系（关键点）

1. **触发链**：`auto_compact` **只由 `token_budget` 触发**（>80% 阈值），与
   `max_total_tokens` 无触发关系。`max_total_tokens` 到达时无论上下文多空都停——
   compact 救不了它。
2. **计费链**：compact 调用是真实 LLM 调用，其 usage 应**计入** `max_total_tokens`
   （默认），否则用户会看到"没到配额却多花钱"。但计入后必须**防死循环**：调用前检查
   剩余配额，若不足以完成一次 compact，应跳过 compact 直接走降级截断——否则
   compact 自己把配额打满，反而比不 compact 更早中断 run。
3. **预算链**：compact 的产物（摘要）**回填 `token_budget`**——摘要注入后仍须通过
   `should_truncate` 检查；摘要大小应设上限（建议 ≤ `token_budget` 的 10%）。
   compact 不改变 `token_budget` 本身，只是让"被丢掉的记忆"以低成本形式留在预算内。
4. **终止语义不变**：两种终止（`token_budget` 压不住 / `max_total_tokens` 到达）都
   标记 incomplete，`on_incomplete` 策略不变。compact 只降低"截断后失忆 → 重复工具
   调用 → 长度/迭代超限"的**概率**，不改变任何终止条件本身。
5. **总量净效应不确定**：
   - 不 compact：后续请求 input 更小，但失忆导致重复工具调用 → 请求数变多；
   - compact：后续请求 input 多一段摘要，但记忆连续 → 请求数减少；
   - 净 token 可能是节省（长任务），也可能是支出（短任务恰好触发一次）。
   这正是"默认关闭 + 用户按场景开启"的原因。

### 3.3 判定顺序（每次迭代）

```
迭代开始
├─ max_total_tokens 已达? ──是──▶ break → incomplete
├─ token_budget 用量 >80%? ──否──▶ 正常 chat_with_tools
│   └─是
│       ├─ auto_compact 开 & 剩余配额够一次 compact & 未超 compact 次数上限?
│       │    是 → 摘要调用（计入 max_total_tokens）→ 摘要注入 index 1 → 归档原始消息
│       │    否 → 现有截断 + 提示 + 归档（降级）
│       └─ 截断/摘要后仍 >80%? ──是──▶ break → incomplete
└─ 正常推进
```

### 3.4 配置层面的显式化建议

- 文档用表格明确"谁管什么"，并给典型组合示例：
  `token_budget: 32000, max_total_tokens: 200000, auto_compact: true`——单请求 ≤32k、
  整轮 ≤200k、预算逼近时摘要续命。
- 明确说明一个**反直觉现象**：`max_total_tokens` 设得接近 `token_budget`（如 32000）时，
  `auto_compact` 基本不会生效（配额被 agent 自身调用先耗尽），属正常，不是 bug。

---

## 4. 什么是"给 Clausura 加 auto-compact"

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

## 5. 价值分析（Value）

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

## 6. 成本分析（Cost）

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
- 需要把 compact 调用计入 `max_total_tokens` 与 `timeout_secs` 的账（关系细节见 §3.2）——否则用户对
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

## 7. 备选方案（Alternatives）

| 方案 | 成本 | 效果 | 点评 |
|------|------|------|------|
| **A. 现状**：截断+归档+提示 | 0 | 有损；incomplete 结局多 | 基线 |
| **B. 确定性 findings 台账（Phase 0）** | ~1 人日 | 中 | **不调 LLM**：从对话中已有的 assistant findings JSON（`extract_findings` 已能解析）提取汇总，注入"截止目前 N 条 finding"台账。直接消灭"丢 findings"这个最大痛点，零额外计费、零非确定性 |
| **C. LLM 摘要 compact（Phase 1）** | 3–5 人日 | 高 | 本调研主体方案；可选开关 + 优雅降级 |
| **D. 只压缩工具输出** | 中 | 中 | 工具输出是上下文膨胀主因；已有 32KB/1000 行截断，可再降。但模型可能因此重新请求 → 需配合台账 |
| **E. 换真实 tokenizer / 调大预算** | 低 | 中 | tiktoken 让触发更可预测；128k 预算直接让截断少发生。是**今天最便宜的缓解手段**，应作为 compact 的前置/并行项 |
| **F. 混合**：B（台账）默认开 + C（摘要）可选 | 1 + 3–5 人日 | 最高 | 建议终态：台账兜底记忆，摘要增强连续性 |

---

## 8. 建议与实施路线

> **已实现 (2026-08-04)**：§7.1 的 Phase 1 已落地为可选开关（默认关闭，行为零变化）：
> `auto_compact: true/false`、`max_compactions: u32`（默认 3，0 = 关闭），Env
> `CLAUSURA_AUTO_COMPACT` / `CLAUSURA_MAX_COMPACTIONS`。实现遵循本报告全部关键约束：
> 触发阈值与截断一致、摘要注入 index 1（User 角色）、摘要上限按**截断阈值下的剩余空间**
> 动态计算（`80% 预算 − 保留尾部 token − hint 固定文本`，另设 10% 预算封顶；剩余空间不足
> 200 token 时跳过 compact）——保证摘要注入后上下文仍低于 80% 阈值，成功 compact 不会
> 反手把 run 标记 incomplete；compact 计费计入 `max_total_tokens` 且调用前有配额守卫、
> 失败/超时降级为现有截断+提示、摘要输入对被丢 Tool 消息做文本化以绕过 Anthropic
> `tool_use_id` 配对问题。
> Phase 0（确定性 findings 台账）已以**磁盘 ledger** 形式实现（`findings_ledger`，默认 true，
> 见 §7.2 验收点），并与 compact 形成无损互补：截断/compact 丢掉的早期 findings 由
> ledger 在终局回读合并，不再依赖摘要或模型自觉。

### 7.1 建议

1. **先做 Phase 0（确定性 findings 台账，~1 人日）**：零 LLM 成本、零非确定性，
   直接修复"截断丢 findings → 假通过/假失败"这一最痛问题。落地简单：在截断注入点，
   用现有 `extract_findings` 从保留/丢弃消息中提取 findings 摘要成台账注入。
2. **Phase 1（LLM 摘要 compact）做成默认关闭的可选开关**：
   - 配置：`auto_compact: true`（默认 false，行为零变化、向后兼容）；
   - 触发：与截断同一阈值（>80% 预算），一次任务最多 N 次（防死循环）；
   - 摘要上限：≤ token_budget 的 10%，且摘要后仍须通过 `should_truncate` 检查；
   - 记账：compact 调用单独记 usage，默认**计入** `max_total_tokens`，日志明示；
     调用前须检查剩余配额，不足则跳过 compact 走降级截断（见 §3.2 计费链）；
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

**已实现 (Update 2) —— findings ledger 验收：**

- 早期迭代的 findings（与 tool call 同响应）在后续被截断/compact 后，终局 Stop 仍能合并回最终结果；
- 合并确定性：同 rule_id+location+message 去重，最终响应优先，ledger 独有项追加；
- `findings_ledger: false` 时行为与旧版完全一致（不写文件、不合并）；
- 成功退出（exit 0）时 ledger 与归档一并清理；失败时保留可审计；
- ToolCalls 分支保留 assistant content（修复信息丢失点），中间轮 findings 草稿得以落盘。

---

## 9. 参考（代码位置）

- `crates/clausura-core/src/context.rs` — 预算跟踪、截断算法、归档
- `crates/clausura-core/src/agent.rs` — agent 循环、截断注入、auto-compact（`try_compact`、
  `dropped_to_text`、`compact_request_messages`、`truncate_summary_to_budget`）、findings 提取
- `crates/clausura-core/src/provider.rs` — provider trait、启发式 token 计数、Anthropic 消息转换、
  MockProvider 摘要队列
- `crates/clausura-core/src/config.rs` / `types.rs` — `TaskContract`（新增 `auto_compact` /
  `max_compactions`）、默认值、YAML/Env 解析与校验
- `crates/clausura-core/src/snapshot.rs` — 快照/断点（compact 摘要随消息自然持久化）
- `docs/guide/overview.md`、`docs/guide/troubleshooting.md`、`README.md` — 现状文档与
  "context exhausted" 排障路径
