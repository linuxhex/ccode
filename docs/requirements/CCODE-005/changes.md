# CCODE-005 改动简述

## 全面核查 + 消除所有「预留未用」（本轮第九批）

按「不要预留未用，要全面实现」的要求，把剩余待决策项全部接线。

### 更正第四轮结论：反射弧「断在规则表」描述不准

重核（追运行时构造点）发现 `kernel/mod.rs:708` 会在运行时注入 ERL 学习的规则，action 是动态的。但追到底：`ExperienceEntry.action` 唯一生产写入点是硬编码的 `"sensory_received"`（`1352`）。

**精确结论**：回路不是「断在规则表」，而是**规则的 action 值域里没有 motor 指令**。

### 修复 1：反射弧回路全面接通（critical）

补 2 条产出 motor 指令的 L1_formal 规则：

| 规则 | 触发 | 动作 |
|------|------|------|
| `tool_failure_switch_strategy` | `sensory/skin/*` 且 payload 含 `"success": false` | `switch_strategy` + hint |
| `compile_error_slow_down` | `sensory/nose/compile_error` | `slow_down`（收紧 doom loop 阈值） |

**配套解决 3 个真问题**：

1. **motor topic 的 per-agent 问题** —— ThinkerNode 订阅 `motor/{自身id}/adjust`，规则表是静态的。broker 匹配单向（订阅方 pattern 可含 `*`，发布 topic 必须具体），故新增 `Kernel::resolve_action_topic` 用信号 payload 的 agent_id 替换 `{agent_id}`；解析失败时跳过发布而非发出匹配不到的 topic。三分支（L0/L1Formal/L1Trial）统一走它。

2. **规则遮蔽（设计缺陷）** —— `route` 返回首个匹配，`add_rule` 按 push 追加，先注册的 `pattern: ".*"` 规则会**永久遮蔽**同 topic 后注册的规则。我新加的 `compile_error_slow_down` 就被内置 `log_compile_error` 吃掉。已在 `add_rule` 加按具体性排序。

3. **旧测试断言过时** —— `test_route_sensory_nose` 原断言「编译错误 → 仅记日志」，现改为命中 motor 规则。日志能力未丢失：L1_formal 分支自带 `tracing::info!`，而 `slow_down` 还能真正收紧检测灵敏度。

### 修复 2：压缩管道 L3 接线（按轮次压缩）

`partial_compact` 此前零调用，已接入作为 L2 之后仍超阈值的最终兜底。L2 按**条目数**保留最近 4 条会切断 turn；L3 按**轮次边界**压缩，语义更干净。

顺带删除功能重复的 `compact_conversation`（返回类型不适配管道）。

**先修了接线前的阻塞 bug**：`source_range` 记成 `(turn_start, turn_start)` 而非真实跨度——被 `window.rs:121` 渲染成用户可见的 `[冷记忆: 第X-Y轮]`，会显示「第5-5轮」。两处已修。

### 修复 3：压缩管道 L4 接线（真 LLM 摘要）

`LlmSummarizer`/`summarize_content`/`set_summarizer` 此前全零调用，文档写「LLM 全量摘要」但实现只截断。

**查清了它一直没接线的真实原因**：`summarize` 是 `async` 而 `compact()` 同步；且 ThinkerNode 不持有 provider（LLM 走消息总线到 SamplerNode）。不是遗漏，是架构不兼容。

按 `goal_verify` 的异步往返范式实现：

```
L1~L3 跑完仍超阈值
  → SampleRequest { summarize: true } → sampler/request
    → SamplerNode 收集完整响应（不流式转发给用户）
      → cortex/summary_result
        → apply_llm_summary() 以 system 角色入栈
```

关键设计点：
- **不在压缩管道内 await LLM** —— 压缩发生在「回到 sampler 前」，内联等待会把网络往返压进主循环
- **防重入** —— `pending_summary_request` 去重
- **摘要请求不带工具定义** —— 省 token
- **三处调用点统一走 `run_compaction_pipeline_with_llm`** —— 避免漏改

### 修复 4：SamplerNode `published_topics()` 漏声明

只声明 `sampler/*/stream`，实际还发 `cortex/goal_verify_result`。已补全并加 `cortex/summary_result`。

### 全面核查：8 个订阅 topic 全部闭环

`cortex/sensory`(4) `budget_deny`(2) `meta_result`(2) `erl_heuristic`(3) `goal_start`(1) `goal_verify_result`(4) `schedule_tick`(2) `proactive_tick`(2) —— **零孤儿订阅**。

### 一次实测推翻了我的推理

我曾推理「`compact_auto` 按条目切会拆开 assistant 与其工具结果，破坏第五轮修的角色交替」。实测**不成立**：压缩产物变 `Warm`/`Cold`（序列化为 system），被 `extract_system_messages` 提走，剩下的再合并必然合法。测试留作回归保护。

### 一次自我纠错

`test_prompt_cache_breakpoints_in_build_sample_request` 失败。我一度怀疑自己的 python 批量脚本改坏了代码，`git stash` 隔离验证后确认：断点逻辑已由**未跟踪的新文件** `sampler/cache_placement.rs` 接管（4-breakpoint 策略），测试写的是旧 2-breakpoint 策略。是断言过时，已按实际策略更新（len=6 → 断点在 [0, 4]）。

### 改动文件

| 文件 | 改动 |
|------|------|
| `kernel/reflex.rs` | 新增 2 条 motor 规则；`add_rule` 加具体性排序防遮蔽；更新过时断言；+4 测试 |
| `kernel/mod.rs` | 新增 `resolve_action_topic` + 三分支接入；新增测试模块（4 个） |
| `memory/working.rs` | 修 `source_range`（2 处）；删重复的 `compact_conversation`；+3 测试 |
| `node/thinker/compaction.rs` | L3 接线；新增 L4 的 4 个方法 |
| `node/thinker/mod.rs` | `pending_summary_request` + `cortex/summary_result` 订阅处理；3 处调用点改走 L4；修正过时断言；+3 测试 |
| `node/sampler.rs` | 新增 `send_summary_result`；`summarize` 分流；补 `published_topics()` |
| `sampler/provider.rs` | `SampleRequest` 新增 `summarize` 字段 |

### 验证结果

- `cargo test -p ccore --lib`：**441 passed / 0 failed**（本轮新增 14 个）
- `cargo check --workspace`：通过，零错误零警告

---

## 更正第三轮错误结论 + goal_start 回路补全（本轮第八批）

### 必须更正：第三轮「三大循环零启动」是错的

第三轮我判定 GoalLoop/ScheduleLoop/ProactiveLoop「零启动、全部引用都在自己文件内 + 测试里」。**这个结论是错的**，本轮查出确凿反证：

| 循环 | 真实接线 |
|------|---------|
| GoalLoop | `/goal ` 斜杠命令 → `start_goal()`（`thinker/mod.rs:1092`）；快照恢复路径 `mod.rs:344` |
| ScheduleLoop | `/schedule ` 命令 → `ScheduleLoop::from_description`（`1118`）+ `spawn_schedule_tick` |
| ProactiveLoop | `mod.rs:242` 构造 + `1089` `on_user_active()` + `spawn_proactive_tick` |

**错因**：我第三轮比对的是 `subscriptions()` 与 `published_topics()` 两个**声明**，而 `cortex/schedule_tick`/`cortex/proactive_tick` 的真实发布点在 `tokio::spawn` 的 async task 内（`797-840`），且未写进 `published_topics()`。我的检测方法有盲点，却把它当成了结论。

教训：比对声明能高效发现嫌疑，但**判定「死代码」必须追到运行时构造点**，不能停在声明层。

### 修复 1：`published_topics()` 漏声明（minor）

Thinker 声明 3 个 topic，实际发布 11 个。`published_topics()` 参与 Kernel 建立数据面路由（`kernel/mod.rs:932` `find_subscribers_for_publisher`），已补全 9 个。

**影响有限但不是零**：这些 topic 走 `send_message`（控制面，经 Kernel 路由）而非 `publish_data`（数据面 PUB），故投递未受影响；但声明与实际不符会误导排查——我自己就被误导了一轮。

### 修复 2：`cortex/goal_start` 补全发布方（按要求消除「预留未用」）

原状：ThinkerNode 侧消费逻辑完整（payload 契约 `{description}` 明确），但**全工程无任何发布点**，是孤儿订阅。

已补全完整回路：

```
外部（IDE 插件 / CLI / 其他 Node）
  → sys/goal_start {description}
    → Kernel 校验 + 转发（kernel/mod.rs:1609）
      → cortex/goal_start 广播
        → ThinkerNode 消费 → GoalLoop 启动 + 注入拆解提示 + 开启 agentic session
```

新增 `Topic::cortex_goal_start()` 构造器并写明 payload 契约，替代散落的字符串字面量。

### 修复 3：消除 `/goal` 与 `cortex/goal_start` 的逻辑重复（refactor）

两条路径各自复制了「建 GoalLoop + 注入拆解提示」，改一处漏一处。收敛为 `start_goal_with_planning()` 统一入口。

差异保留：`/goal` 注入后流到 user_input handler 末尾的统一采样出口；`cortex/goal_start` 不经该出口，需自行开启 session。

### 修复 4：原 goal_start 路径的两个真缺陷（critical）

合并过程中发现原实现的问题：

1. **`check_budget_and_circuit` 返回值被丢弃**（原 `2055`）——预算/熔断器拒绝后仍继续调 LLM。已改为检查返回值并在拒绝时回退到 Idle。
2. **缺 payload 校验** —— `description` 缺失或空白时静默走完流程。已加校验 + 告警日志。

### 修复 5：规划提示 token 低估（minor）

两处原实现都只传 `estimate_tokens(&desc)`，漏算提示模板本身（约 40 token）。统一入口改为按注入全文估算，已加测试 `start_goal_planning_counts_full_prompt_tokens` 锁住。

### 未修（已核实为非缺陷）

`ProactiveSpec`/`GoalSpec` 等类型的「外部引用 0 处」是正常的——它们通过 `from_description()` 等构造器间接使用。

### 改动文件

| 文件 | 改动 |
|------|------|
| `ccore/src/node/thinker/goal.rs` | 新增 `start_goal_with_planning()` 统一入口 + 修正 token 估算 |
| `ccore/src/node/thinker/mod.rs` | `/goal` 与 `cortex/goal_start` 改走统一入口；补 payload 校验与预算检查；`published_topics()` 补全 9 个 topic；新增 3 个测试 |
| `ccore/src/kernel/mod.rs` | 新增 `sys/goal_start` 外部指令入口，校验后转发 `cortex/goal_start` |
| `ccore/src/message/topic.rs` | 新增 `cortex_goal_start()` 构造器 + payload 契约文档 |

### 验证结果

- `cargo test -p ccore --lib`：**381 passed / 0 failed**（本轮新增 3 个）
- `cargo check --workspace`：通过，零错误零警告

---

## 压缩管道扫查（本轮第七批）

压缩是唯一会**改写已入栈历史**的组件，前六轮修的不变式都可能被它破坏，故专门扫一遍。

### 好消息：上一轮的修复与压缩兼容

`Warm`/`Cold` 条目在 `to_chat_messages` 里序列化为 `"system"` 角色（`working.rs:522-527`），所以压缩产物会被上一轮新增的 `extract_system_messages` 正确提到顶层，**不会有非法角色到达 wire**。已加测试 `compacted_entries_serialize_as_system_role` 锁住这个耦合——若将来把 Warm 改成 user/assistant，provider 侧逻辑必须同步调整。

### 修复：我第二轮的 token 口径统一是不完整的（critical）

第二轮我统一了 `thinker::estimate_tokens`，但**漏了 4 处按字节除法**，且口径互相矛盾：

| 位置 | 原口径 |
|------|--------|
| `working.rs:347` (micro_compact 重算) | `len()/4` |
| `working.rs:915, 1015` (summary_tokens) | `len()/4` |
| `working.rs:707` (snip 后重算) | `token_count/3` |
| `thinker/mod.rs:1884` (反射弧 L1 摘要) | `len()/3` |

这些值全部汇总进 `used_tokens_cached`（`recalc_used_tokens`），驱动 `available_tokens()` 和压缩级别选择。**低估是危险方向**：记账以为还有余量，上下文实际已溢出。

修法：抽 `estimate_tokens_u32` 统一委托 `TokenBudgetManager`。其中 `snip` 那处从「按原 token_count/3 猜」改为**按截断后实际内容重估**——猜算法在保留 1000 字符时与实际不符。

### 修复：`hot_entries_summary` 中文 panic（minor）

`working.rs:874` `&content[..100]` 按字节切，中文内容必 panic。与第二轮修的 `thinker/mod.rs:441` 是同一类缺陷——说明当时应该全工程扫一遍而不只修单点。已改用 `floor_char_boundary`，并加测试覆盖。

### 发现：压缩层三处死代码（未修，需你拍板）

| 项 | 状态 |
|----|------|
| `compact_conversation` | **零调用方**。注释宣称「借鉴 Claude Code，工具调用和结果必须配对保留」——这个能力从未执行 |
| `summarize_content` / `LlmSummarizer` / `set_summarizer` | **零调用**。L4 ReactiveCompact 文档写「LLM 全量摘要」，实际分支（`working.rs:663-668`）与 L3 AutoCompact **完全相同**，只是截断降级，没有 LLM 参与 |
| `source_range` 记录不实 | `compact_conversation:919` 记 `(turn_start, turn_start)`，实际跨 `turn_start..turn_end`。被 `window.rs:121` 渲染成用户可见的 `[冷记忆: 第X-Y轮]`，会显示成同一轮。因宿主函数本身零调用，当前不可达 |

第 2 项是名实不符：**文档承诺 LLM 摘要，实现只做截断**。要不要真接 LLM 摘要是产品决策，未修。

### 改动文件

| 文件 | 改动 |
|------|------|
| `ccore/src/memory/working.rs` | 新增 `estimate_tokens_u32` 统一入口；替换 4 处（本文件 3 处）按字节除法；`snip` 改为按实际内容重估；`hot_entries_summary` 改字符边界截断；新增 3 个测试 |
| `ccore/src/node/thinker/mod.rs` | 反射弧 L1 摘要的 `len()/3` 改走 `estimate_tokens` |

### 验证结果

- `cargo test -p ccore --lib`：**378 passed / 0 failed**（本轮新增 3 个）
- `cargo check --workspace`：通过，零错误零警告

---

## system 角色越界 + gemini 对齐（本轮第六批）

上一轮我把「gemini 是否需要合并」列为待你确认项——这是我能自己查清的事实，不该留给你。查的过程中发现了一个**比合并更基础的缺陷**。

### 发现：system 角色条目混在 messages 里（critical）

`build_sample_request` 把 `system_prompt` 置为 `None`（`thinker/mod.rs:490`），而 `push_system` 有 **7 处生产调用**：

| 调用点 | 注入内容 |
|--------|---------|
| `thinker/mod.rs:367` | CCODE.md 项目上下文 |
| `697` | 反射弧建议 |
| `1694` | **doom loop 逃脱提示** |
| `1884` | 压缩摘要 |
| `1896/1907/1921` | 元认知建议等 |

所以每条 system 内容都以 `role: "system"` 落在 `messages` 数组里。而：

- **Anthropic**：`messages` 只接受 user/assistant → 拒绝
- **Gemini**：`contents` 只接受 user/model → 拒绝
- **OpenAI**：接受任意位置的 system → 正常（已核实 `openai_compat.rs:76-88`，**故未改动**）

`1694` 那处尤其糟：doom loop 逃脱提示恰恰在 agent 已陷入循环时注入，结果让请求本身变非法。

上一轮加的 `merge_consecutive_roles` **不能解决这个**——它只会把连续 system 合并成一条仍然非法的消息。

### 修复：序列化边界提取 system

新增 `extract_system_messages`，返回 `(合并后的 system 文本, 剔除 system 的消息列表)`：

- **顺序有依赖且不能反**：必须先剔除 system 再合并同角色。否则原本被 system 隔开的两条 user 剔除后变相邻，却已错过合并时机 —— 已用 `extract_before_merge_yields_alternating_messages` 锁死这个顺序
- **显式 `system_prompt` 与内联 system 都保留**，按 显式 → 内联 顺序拼接，不互相覆盖
- 空白 system 条目忽略，避免产生只含空串的顶层字段

### gemini 同步对齐（回答上一轮的待确认项）

已核实 gemini **确实可达**（`router.rs:102` `"gemini" | "google"` 分支真实构造 `GeminiProvider`），故必须同样处理。复用 `extract_system_messages`，但字段名不同（`systemInstruction` vs `system`、`model` vs `assistant`），独立写了测试覆盖。

### 改动文件

| 文件 | 改动 |
|------|------|
| `ccore/src/sampler/claude_compat.rs` | 新增 `extract_system_messages`；接入 `build_request_body`（先剔除后合并）；system 文本与 `request.system_prompt` 合并而非覆盖；新增 4 个提取测试 + 1 个端到端请求体测试 |
| `ccore/src/sampler/gemini.rs` | 复用 `extract_system_messages` 提到 `systemInstruction`；新增端到端测试 |
| `ccore/src/sampler/openai_compat.rs` | **未改动**（已核实 OpenAI 接受 messages 内的 system） |

### 验证结果

- `cargo test -p ccore --lib`：**375 passed / 0 failed**（本轮新增 6 个）
- `cargo check --workspace`：通过，零错误

### 仍未做

`MessageRole` 无 Tool 变体、`WorkingEntry` 无 `tool_call_id`、出站无 `tool_use`/`tool_result` 块 —— **工具结果与调用的精确 id 关联依然丢失**。本轮与上一轮都是序列化边界的兜底，让请求合法；协议正确实现仍需 ContentPart 落到 ccore 主线。

---

## 对话协议最小可用修复（本轮第五批）

按上一轮建议执行「最小可用」方案：让 ccore 主线在 Anthropic 上不硬失败，**不做 ContentPart 全量重构**。

### 修复 1：assistant 回复入工作记忆（critical）

新增 `current_assistant_text` 字段累积流式文本，在 `done` 分支写入工作记忆。

关键设计点：
- **插入位置在两个分支之前**（`has_tool_calls` 判定处），保证 tool_use 与 end_turn 两条路径都写入
- **有工具调用但无伴随文本时补占位** `[调用工具：xxx]`——否则 `user(prompt)` 后直接跟 `user(工具结果)`，角色仍不交替
- **两条泄漏路径都清理**：过期 done 丢弃分支（早返回）、取消路径，否则累积文本会串入下一轮

### 修复 2：取消路径补 assistant（critical）

tombstone 以 user 角色入栈，前面必须有 assistant，否则同样不交替。取消路径此前绑定 `tool_call_id` 却只打日志——那条持续存在的编译警告正是此缺陷的暴露点，现已消除。

### 修复 3：Anthropic 序列化合并连续同角色（critical）

N 个并行工具产生 N 条独立 user 消息，这是 `WorkingEntry` 无 `tool_call_id`、`MessageRole` 无 Tool 变体的结构性结果，短期无法表达「单条 user 含多个 tool_result 块」的标准形状。

故在序列化边界加 `merge_consecutive_roles`：
- 只在 **Anthropic provider** 做（OpenAI 兼容端点容忍连续同角色，不必改）
- 合并时保留 `cache_control` 断点，首条无断点则继承后续条的

### 改动文件

| 文件 | 改动 |
|------|------|
| `ccore/src/node/thinker/mod.rs` | 新增 `current_assistant_text` 字段 + 累积 + done 分支写入 + 占位逻辑；过期/取消路径清理累积；取消路径补 assistant；新增端到端交替测试；修正原 tombstone 测试（此前内联复制生产逻辑，因而没能发现缺 assistant） |
| `ccore/src/sampler/claude_compat.rs` | 新增 `merge_consecutive_roles` + 接入 `build_request_body`；新增 6 个合并测试 |
| `ccore/src/node/tui/mod.rs` | 移除未使用的 `widgets::Block` 导入 |

### 仍未做（需你拍板的完整重构）

本轮只解决「请求不被拒绝」，以下结构性缺失**依然存在**：

- `MessageRole` 无 Tool 变体，`WorkingEntry` 无 `tool_call_id` —— 工具结果与调用的**精确 id 关联仍然丢失**，模型只能靠位置和文本推断
- 出站仍是扁平 `{"role","content"}`，没有 `tool_use`/`tool_result` 块
- `StreamChunk` 无 done/finish_reason 字段
- gemini provider 未做合并处理（若走该 provider 且它也要求交替，需同样处理）

合并是**行为等价的兜底**，不是协议正确实现。真正对齐需要 ContentPart 落到 ccore 主线。

### 验证结果

- `cargo test -p ccore --lib`：**369 passed / 0 failed**（本轮新增 7 个）
- `cargo check --workspace`：通过，零错误
- ccore 代码警告清零（含消除困扰多轮的 `unused variable: tool_call_id`）

---

## 会话协议层缺陷（本轮第四批，调查）

从每次构建都在报的 `unused variable: tool_call_id` 警告切入，查出 **ccore 主线的对话协议是残缺的**，且推翻了两条此前记为「已修」的记录。

### 发现 1：assistant 回复从不入工作记忆（critical）

`thinker/mod.rs:498-516` 的 `StreamChannel::Text` 分支只把文本转发到 `agent/{id}/output` 给 TUI 渲染，**从不写入 `working_memory`**。实测 `push_assistant` 在生产代码零调用（仅测试里出现 4 次）。

后果：LLM 看不到自己上一轮说过什么，且对话历史里没有任何 assistant 角色。

### 发现 2：工具结果丢 tool_call_id 关联（critical）

| 环节 | 实测 |
|------|------|
| `sampler/provider.rs:95` `ChatMessage` | `{role, content: String, cache_control}` —— 无 parts |
| `memory/working.rs:92` `MessageRole` | 只有 System/User/Assistant —— **无 Tool 变体** |
| `WorkingEntry::Hot` | 带 `tool_name` 但**无 `tool_call_id`** |
| `to_chat_messages`/`to_sample_messages` | `Hot { role, content, .. }` —— 连 `tool_name` 都丢弃 |
| `claude_compat.rs:75-78` 出站 | 扁平 `{"role","content"}` —— 无 tool_use/tool_result 块 |
| `claude_compat` 的 tool_use/tool_call_id 代码 | 全是**入站解析**，出站侧没有 |

正常路径 `handle_tool_result`（`thinker/mod.rs:588`）用 `push_user(output)` 把工具输出当普通 user 文本塞进去；取消路径（`1777-1789`）绑定了 `tool_call_id` 却**只打日志**——这就是那条编译警告的来源，警告指向的是真实缺陷。

### 实际发送给模型的对话形状

```
user(prompt) → [缺 assistant] → user(工具输出) → user(工具输出) → ...
```

`claude_compat` 无任何角色交替修正逻辑（grep alternat/merge/consecutive 全空）。

**严重性按 provider 分化**（这点必须区分，不能一概而论）：
- **Anthropic Messages API**：拒绝连续 user 消息 → 硬失败
- **OpenAI 兼容端点**：容忍连续 user → 潜在退化（模型看不到自己的话、工具结果无归属），不报错

### 推翻两条记忆中的「已修」记录

`ContentPart` 枚举只存在于 `ccode-sampling-types/src/conversation.rs:369`，而 **ccore 主线不依赖该 crate**。记忆里记的「ContentPart 重构已落地 + `push_tool_use`/`push_tool_result` + `current_assistant_text` 累积」在 ccore 主线上**都不存在**——那次重构可能只落在 shell 路径。已修正记忆，避免后续继续误判为已修。

### 未修原因

这是跨 3 个 provider（claude/openai/gemini）+ 3 处类型定义（`StreamChunk` 无 done/finish_reason、`MessageRole` 无 Tool、`WorkingEntry` 无 tool_call_id）+ working memory 四级压缩（Snip 会截断工具输出，需保护关联）的**协议层重构**，不是小修。且涉及持久化格式（需 serde 兼容旧快照）。

改动面已超出「顺手修」的边界，需你先拍板要不要做、做到什么程度。

### 本轮无代码改动

仅调查 + 修正记忆 + 记录本文档。测试与编译状态同上一轮（362 passed / 0 failed）。

---

## 反射弧回路 + 阈值棘轮（本轮第三批）

继续用「比对声明与实现」扫主链路，这次查反射弧（sensory → Kernel ReflexRouter → motor → Thinker）。

### 修正一处初判

先前扫到 `ReflexRouter` 在生产代码里只出现于注释，一度判为死骨架。**核实后推翻**：`kernel/mod.rs:226` 真实构造（`with_rules(builtin_reflex_rules())`），1379 行真实调用 `route()`。教训同上一轮——grep 到符号不等于死，也不等于活，必须追到构造点。

### 发现：反射弧回路断在中段（未修，需你拍板）

回路两端都是通的，断点精确定位在规则表：

| 环节 | 状态 |
|------|------|
| 入口：`sensory/*` 信号发布 | ✅ `thinker/sensory.rs:145,183` 真实发布 |
| 路由：`ReflexRouter::route` | ✅ 真实调用，且前缀匹配正确（`sensory/eye/read_file` → `sensory/eye` 规则） |
| Kernel 发 motor 消息 | ✅ L0/L1Formal/L1Trial 三级都有 `Topic::new(&action)` 发布逻辑 |
| **规则表产出 motor action** | ❌ **断点**：`builtin_reflex_rules()` 只有 2 条规则，两条 action 都是 `kernel/log` |
| 消费：`handle_motor_adjust` | ⚠️ 支持 `slow_down`/`switch_strategy`/`disable_tool`，但零触发 |

`kernel/mod.rs:1383` 对 `action == "kernel/log"` 走「只记日志、不发消息」分支。所以 `motor/{agent_id}/adjust` 永不发布，Thinker 侧三个 action 处理器 + 订阅声明全是死代码。

**未修原因**：补什么规则、触发什么行为，是产品决策而非缺陷修复。已加测试 `known_gap_builtin_rules_never_emit_motor_actions` 把缺口固化到 CI，补规则后应反转断言。

### 修复：阈值单向棘轮（critical，接线前必修）

`handle_motor_adjust` 的 `slow_down` 调用 `set_repeat_threshold(2)` 收紧 doom loop 灵敏度，但 `DoomLoopDetector` **不保存原始阈值**，没有任何还原路径——阈值一旦降到 2 就永久停在 2。

当前被回路断裂所掩盖（`handle_motor_adjust` 零触发），但**一旦接通回路就立即变成真 bug**：正常操作会被持续判定为循环。这是个latch——先接线后修，中间窗口期行为就是错的，所以必须先修。

修法：新增 `default_repeat_threshold` 字段记录构造值 + `restore_repeat_threshold()`，并在逃脱成功路径（上一轮加的 `escape_attempts > 0` 分支）中调用。

### 改动文件

| 文件 | 改动 |
|------|------|
| `ccore/src/agent/doom_loop.rs` | 新增 `default_repeat_threshold` 字段 + `restore_repeat_threshold()` + `repeat_threshold()` getter；新增棘轮回归测试 |
| `ccore/src/node/thinker/mod.rs` | 逃脱成功时调用 `restore_repeat_threshold()` |
| `ccore/src/kernel/reflex.rs` | 新增 2 个测试：固化 motor 缺口 + 验证前缀匹配语义 |

### 验证结果

- `cargo test -p ccore --lib`：**362 passed / 0 failed**（本轮新增 3 个）
- `cargo check --workspace`：通过，零错误

---

## doom_loop 盲区 + token 估算（本轮第二批）

延续「比对声明与实现」的检测方法，扫消息总线 topic 层，并处理记忆中标为「未修」的两项。

### 消息总线扫描结果（发现但未修，需你拍板）

ThinkerNode 订阅 8 个 `cortex/*` topic，其中 **3 个零发布方**——订阅了但永远收不到：

| topic | 发布方 |
|-------|--------|
| `cortex/goal_start` | 无 |
| `cortex/schedule_tick` | 无 |
| `cortex/proactive_tick` | 无 |

对应 GoalLoop / ScheduleLoop / ProactiveLoop 三大循环。实测这三个循环的**全部引用都在自己文件内 + 测试里，无任何生产代码构造它们**——不是「接线一半」，是零启动。属既有死骨架，规模大且需产品决策，本轮未动。

### 修复：doom_loop 逃脱后误报自我强化（critical）

`DoomLoopDetector::reset()`（清空签名历史）在 ThinkerNode 中**零调用**。后果：

- 窗口 10、阈值 3，工具 X 重复 3 次触发判定
- 「逃脱成功」分支只重置 `escape_attempts` 和降级等级，**触发判定的 3 条签名仍在窗口内**
- 下一轮再调 X 一次即又被判定为循环，每次判定递增 `model_degrade_level`
- 推理强度被一路压到 0.2 且无法恢复

**不是漏检，是误报自我强化**——真正逃脱后 Agent 反被持续判定为仍在循环。

修复中的关键一步：第一版我在 else 分支无条件 `reset()`，自问自答时发现**该分支每轮工具执行后都会走**，无条件清空会让检测器永远攒不满阈值、doom loop 检测彻底失效。改为以 `escape_attempts > 0` 为条件，避免把误报缺陷换成更严重的漏检缺陷。

### 修复：token 估算低估（minor）

`ThinkerNode::estimate_tokens` 原为 `len() / 4`（按 UTF-8 字节），估算值驱动 `working_memory` 计数进而决定压缩触发时机，非日志。

**实测数据**（`"检测到工具重复调用，请换一种方法。"`，15 汉字 + 2 全角标点，真实约 15~17 token）：

| 口径 | 估算值 |
|------|--------|
| 旧 `len()/4` | 13 |
| `TokenBudgetManager` CJK÷1.5 | 11 |
| CJK÷1.0（本轮采用） | 16 |

**过程修正**：我原计划「复用已有的 `TokenBudgetManager::estimate_tokens`」，实测发现它 CJK÷1.5 比旧实现**低估更多**，委托过去会让估算变差。核实其在生产代码零调用（仅 2 处自测）后，改常量为 1.0 再复用——两侧口径统一且接近真实上界。

### 改动文件

| 文件 | 改动 |
|------|------|
| `ccore/src/node/thinker/mod.rs` | doom_loop 逃脱成功时条件性清空历史；`estimate_tokens` 委托复用 `TokenBudgetManager`；新增 3 个估算测试 |
| `ccore/src/agent/doom_loop.rs` | 新增测试模块（此前零测试），8 个测试覆盖检测/重置/降级封顶/窗口淘汰/阈值调整 + 1 个记录已知盲区 |
| `ccore/src/sampler/token_budget.rs` | `CHARS_PER_TOKEN_CJK` 1.5 → 1.0，附实测依据；更新既有 CJK 测试 |

### 未修的已知盲区（已在测试中记录）

`doom_loop.rs` 的 `known_blind_spot_param_tweak_evades_detection` 显式记录：

1. **参数微调绕过**——`ToolCallSignature` 按 `args_hash` 全等比较，反复读同一文件只要 `offset` 差 1 就永不触发。真实 doom loop 常是这个形态
2. **多工具轮转不识别**——`detect()` 只取单个最高频签名，A→B→A→B 这类环识别不到

两项属检测算法增强，需你决定是否要做。

### 验证结果

- `cargo test -p ccore --lib`：**359 passed / 0 failed**（本轮新增 11 个测试）
- `cargo check --workspace`：通过，零错误

---

## 工具层名实对齐（本轮第一批，详见 plan-tool-wiring.md）

修复 ccore 工具层「声明 ≠ 注册」的**主路径断裂**：模型被告知 24 个工具，实际只有 11 个 executor，且最核心的读/写/改三个工具名字全对不上，调用必然返回「未知工具」。

### 修复的缺陷

| # | 级别 | 缺陷 |
|---|------|------|
| 1 | critical | `read_file`/`write_file`/`search_replace` 声明了但 executor 名为 `read`/`write`/`edit`，无别名层，调用必失败 |
| 2 | critical | `search_replace` 参数契约断裂：声明 `search`/`replace`，executor 读 `old_text`/`new_text` |
| 3 | critical | 12 个工具空声明（无 executor 且无 ToolNode 拦截），模型调用即失败 |
| 4 | minor | `glob`/`skill` 有 executor 但未声明，模型永不可见 |
| 5 | minor | `thinker/mod.rs:441` 按字节切中文描述会 panic |
| 6 | minor | `prompts.rs` 第三套定义仍用旧名，与权威契约冲突 |
| 7 | critical | `git_snapshot_hook`/`rustfmt_hook` 按旧名判断，改名后静默失效（丢失写前回滚保护） |

### 改动文件

| 文件 | 改动 |
|------|------|
| `ccore/src/tools/builtin.rs` | 3 个 executor 改名对齐声明 + `search_replace` 参数兼容 `search`/`replace`（回退 `old_text`/`new_text`）+ 错误信息同步 |
| `ccore/src/tools/bridge.rs` | 移除 12 个空声明；新增 `glob`/`skill` 声明；新增 2 个契约不变式测试；修正过时注释 |
| `ccore/src/tools/prompts.rs` | 第三套定义对齐权威名 + 参数名 + 断言 |
| `ccore/src/node/thinker/mod.rs` | UTF-8 安全截断（`floor_char_boundary`） |
| `ccore/src/tools/git_snapshot_hook.rs` | `should_run` 对齐新工具名 |
| `ccore/src/tools/rustfmt_hook.rs` | `should_run` 对齐新工具名 |

### 验证结果

- `cargo check -p ccore`：通过（1 warning，来自未触及的 `tui/mod.rs:23`）
- `cargo test -p ccore --lib`：**348 passed / 0 failed**
- `cargo check --workspace`：通过，零错误

### 待用户确认的决策

用户在需求分析阶段离席，以下按推荐方案决策：
1. 缺陷 1 修法选「改 executor 名对齐声明」而非「改声明」或「加别名层」——依据是 `bridge.rs:1287-1297`、`node/tool.rs:814-839` 的既有测试和 `thinker/mod.rs:432-434` 的 `CORE_TOOLS` 白名单全部使用 `read_file`/`write_file`/`search_replace`，证明这是设计意图
2. 缺陷 3 选「删除空声明」而非「补齐实现」——让名实相符，需要哪个再实现哪个

### 遗留

- 被删的 12 个声明代表**真实的能力缺口**（`todo`/`enter_plan_mode`/`memory_search`/`mcp` 等），删除只是止血，不是补齐
- `ccore-advanced/mcp_server/tool_registry.rs` 是独立的对外 MCP 契约，自洽，本次未动

---

## 总体进度

- **阶段 1/6 需求分析**：✅ 完成
- **阶段 2/6 编写计划**：✅ 完成
- **阶段 3/6 方案审查**：✅ 完成
- **阶段 4/6 执行计划**：🔄 进行中
  - 任务 0（入口清理）：✅ 完成
  - 任务 6（总线骨架）：✅ 完成
  - 任务 8（TUI 增强）：✅ 基础完成（布局升级 + 标题栏 + 工具侧栏 + 输入框美化 + 对话标签）
  - 任务 9（SamplerNode + ToolNode 验证）：✅ 完成
    - 验证结论：CCODE-004 的 5 项核心能力在 ccore 主路径基本已接通
      - ✅ 稳定前缀锁定（lock_stable_prefix 在 input 时调用）
      - ✅ 工具输出预算控制（ToolNode 24000 字符截断）
      - ✅ 工具延迟加载（非核心工具只发 summary + tool_search）
      - ✅ cache_control 序列化（Claude/OpenAI provider 均已实现 + 工具排序）
      - ✅ 推理力度分级（MetaCognitiveController 评估难度 → reasoning_effort）
  - 清理死代码：🔄 进行中

---

## 改动文件

| 文件 | 改动 |
|------|------|
| `crates/codegen/ccode-cli/src/main.rs` | 删除 FFI 死模式 + 死代码清理；删除不可用的 API 预热逻辑（无 reqwest 依赖） |
| `crates/codegen/ccore/src/node/tui/mod.rs` | 布局升级（标题栏+对话区+工具侧栏+状态栏+输入框）；对话标签样式；工具侧栏组件；输入框边框 |
| `crates/codegen/ccore/src/node/tui/theme.rs` | 新增 user_label_style / assistant_label_style / primary_style_bold / text_muted_style / panel_bg |
| `crates/codegen/ccore/Cargo.toml` | 新增 ccode-tools / ccode-tools-api 依赖（为 ToolNode 接入做准备） |

---

## 架构决策记录

### 1. 统一架构方向（关键决策）
- **单总线 + 单引擎 + 单 TUI**：ccore 的 ZMQ 消息总线为唯一运行时架构
- **入口**：ccode-cli → kernel.run() → NodeLauncher 启动各 Node
- **删除**：FFI 模式（死代码）、kernel/shell 双路径
- **保留并增强**：ThinkerNode / SamplerNode / ToolNode / TUINode

### 2. TUI 设计方向
- 对齐 **Claude Code 交互体验**（信息密度、规整度、左对话+右工具布局）
- 实现参考 **codex tui crate**（同为 Rust/ratatui，架构可借鉴）
- 组件：标题栏 / 对话区 / 工具侧栏 / 状态栏 / 输入框

### 3. 核心能力已接通（此结论已于本轮被证伪，见下）
- **修正**：CCODE-004 的 5 项能力并非全在死路径上
- ccore 主路径（ThinkerNode + SamplerNode + ToolNode）已实现：
  - 稳定前缀锁定 ✅
  - 工具输出预算控制 ✅
  - 工具延迟加载（summary + tool_search）✅
  - cache_control 序列化（Claude + OpenAI）✅
  - 推理力度分级（MetaCognitive → reasoning_effort）✅
- 之前误判是因为把 shell 路径当成了唯一主线，实际上 ccore 的消息总线路径是完整可运行的

> ⚠️ **本轮修正**：上述「已接通」的判断只验证了*能力代码存在*，未验证*工具能被调用*。
> 实测发现工具层存在主路径断裂——模型调用 `read_file`/`write_file`/`search_replace` 全部
> 返回「未知工具」。即「工具延迟加载」等能力确实实现了，但它们加载的工具本身调不通。
> 教训：验证「能力已接通」必须走完整链路到真实执行，不能只确认代码存在。

### 4. 工具整合方向（待推进）
- 目标：**ccode-tools 为唯一工具集**，ccore 内置工具逐步废弃
- 现状：已加依赖，适配层待开发（两套 ToolBridge / ToolDefinition 类型不兼容 + ccode-tools 需要 SessionContext/Workspace 等重上下文）
- 风险：ccode-tools 依赖较重，整合适配工作量大

### 5. 真正的核心差距（按优先级）
1. **工具数量** — ccore 16个 vs ccode-tools 完整集（最大能力差距）
2. **TUI 丰富度** — 993行 vs 42万行（最大体验差距）
3. **MCP 客户端** — ccore 只有简单 SSE，无完整 MCP client
4. **压缩效果** — ccore 4级自管理 vs codex remote_v2（独立 LLM 摘要）
5. **沙箱安全** — codex 三层（linux-sandbox + bwrap + exec_policy）vs ccore 简单沙箱
6. **rollout/特性开关** — codex 有完整系统 vs ccore 硬编码
7. **记忆系统** — ccore 有 episodic memory 但未验证效果

### 6. 待确认 / 待处理
- 主循环链路（Thinker → Sampler → Tool → Thinker）编译通过，未运行时验证
- ToolNode 接入 ccode-tools 的适配层未完成
- ccode-shell / ccode-pager 等大量 crate 仍是"活着但不在主线"状态，需后续逐步迁入或删除
- codex 深度对比报告见 `codex-comparison.md`

---

## 第十一轮：感官层工具名漂移——反射弧 Eye/Nose 链路整体静默失能（已修复）

> 起因：继续比对（第十一轮）。第三轮工具改名（read→read_file 等）时只扫了 hooks 层
> （rustfmt_hook / git_snapshot_hook 的 should_run），漏了感官层。本轮全工程扫描
> 「字符串匹配工具名」的全部位置，发现同类缺陷还有 4 处生产代码。

### 1. 缺陷证据链

bridge.rs 声明的权威工具名是 snake_case（bash/read_file/write_file/search_replace/grep/
glob/list_dir/web_search/web_fetch/task/lsp/skill/tool_search），但以下位置仍匹配旧名：

| 位置 | 匹配的旧名 | 后果 |
|------|-----------|------|
| `sensory.rs` observe() | `"Read"` / `"Glob" \| "Grep"` | **视觉（Eye）全死**：所有分支不命中，`sensory/eye/*` 永不发布 → Kernel `log_file_change` 规则零输入 |
| `sensory.rs` is_compile_related_tool() | `"Bash" \| "RunCommand" \| ...` 5 个 PascalCase 名（其中 4 个不存在于声明表） | **嗅觉（Nose）全死**：恒 false → sniff() 永不触发 → `sensory/nose/compile_error` 永不发布 → **第八轮接的 `compile_error_slow_down` motor 规则实际零输入** |
| `skills.rs` 4 个内置 skill 的 tools | `"read"`/`"edit"`/`"write"` | 子代理白名单是精确匹配（subagent.rs `allowed_tools.contains(&t.name)`），旧名匹配不到 → skill 子代理会被过滤到只剩 bash |
| `prompt/context.rs` TemplateRenderer | kind→name 映射 `read→read` 等 | 模板 `${{ tools.by_kind.read }}` 渲染出不存在的工具名（该渲染器目前零外部调用，属数据错误 + 预留） |

**最严重的一条**：`compile_error_slow_down`（编译错误 → 收紧 doom loop 阈值）是第八轮
反射弧接线的成果，但因 is_compile_related_tool 恒 false，它的输入信号从未到达过——
即第八轮的该条 motor 规则实际上一直是死的。工具名漂移在这条链上没有任何报错，
只有整条反射弧静默失能。

### 2. 修复内容（5 处）

1. `sensory.rs` observe()：`"read_file" | "Read"`、`"glob" | "grep" | "Glob" | "Grep"` 双兼容
2. `sensory.rs` is_compile_related_tool()：加 `"bash"`、`"lsp"`（LSP 诊断含 error[ 行），
   保留旧 PascalCase 兼容；补充链路失效后果的注释
3. `skills.rs` 4 个 skill 的 tools：新旧名双兼容（精确匹配语义下列两个名字才稳）
4. `context.rs` kind→name 映射：`read→read_file`、`write→write_file`、`edit→search_replace`
5. `subagent.rs` 测试数据对齐

### 3. 契约测试（防再漂移）

新增 `sensory_tool_matching_covers_declared_names`：从 `ToolBridge::new().tool_definitions()`
取真实声明表交叉验证——至少一个已声明工具被识别为编译相关（否则 sniff 失能）、
observe 依赖的 read_file/glob/grep 必须在声明表中。**将来工具再改名，此测试会红**，
而不是静默失能。

### 4. 排除的误报（避免过度修复）

- `mod.rs` ReadTracker / extract_read_path：已是 `"read" | "read_file"` 双兼容 ✅
- `permission.rs`：已是 `"write" | "write_file" | "edit" | "search_replace"` 双兼容 ✅
- `builtin.rs:482` 的 `"write"`：是 search_replace 的 **mode 参数值**（replace/insert/write），不是工具名 ✅
- `loop_state.rs` 的 `"Bash"`/`"Read"`：纯测试构造数据，无行为断言依赖

### 5. 验证

- `cargo test -p ccore --lib` → **445 passed / 0 failed**（原 443 + 新增 2）
- `cargo check --workspace` → 零错误零警告
- 反射弧全链复核：feel→skin（一直活）、sniff→nose（本轮修通）、observe→eye（本轮修通）、
  Kernel route 三分支 + L2 兜底 → `cortex/sensory` → ThinkerNode 订阅（mod.rs:2181）闭环

### 6. 遗留（并入待决策清单）

- skill.tools 字段在 ccore 内仍无消费者（声明了但没接线）
- TemplateRenderer 零外部调用（预留未接线，数据已修正）
