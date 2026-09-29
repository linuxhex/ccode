# Agent Token 效率优化 实现计划

**目标：** 对标竞品 agent，优化 ccode 的 token 消耗，实现"更少 token、更好效果"

**架构：** 四项改进并行推进——工具延迟加载减少每轮 schema 体积、工具输出预算控制防止单次爆发、子 agent 摘要返回保护父上下文、稳定前缀强化提升缓存命中率

**技术栈：** Rust, ccore 分布式消息总线架构

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| `ccore/src/tools/bridge.rs` | 修改 | 工具延迟加载：summary/full schema 分离，新增 `tool_search` 执行器 |
| `ccore/src/tools/builtin.rs` | 修改 | 新增 `ToolSearchExecutor` |
| `ccore/src/tools/mod.rs` | 修改 | `ToolDefinition` 增加 `summary` 字段 |
| `ccore/src/node/tool.rs` | 修改 | 工具输出预算控制：截断超大输出 |
| `ccore/src/node/thinker/mod.rs` | 修改 | 子 agent 摘要返回；稳定前缀锁定；cache_control 正确传递 |
| `ccore/src/agent/subagent.rs` | 修改 | 子 agent 完成时生成摘要 |
| `ccore/src/sampler/claude_compat.rs` | 修改 | 序列化 `cache_control` 到 HTTP body |
| `ccore/src/sampler/openai_compat.rs` | 修改 | 序列化 `cache_control`（message 级别） |
| `ccore/src/memory/working.rs` | 修改 | 调用 `lock_stable_prefix` 的时机 |

---

## 任务 1：工具延迟加载（P0）

**目标**：系统提示中只注入工具名 + 一句话摘要（约 50 token/工具），模型需要完整 schema 时通过 `tool_search` 工具按需加载

**文件**：
- 修改：`ccore/src/tools/mod.rs` — `ToolDefinition` 增加 `summary: Option<String>` 字段
- 修改：`ccore/src/tools/bridge.rs` — 注册时分离 summary/full schema；新增 `tool_search` 工具定义
- 修改：`ccore/src/tools/builtin.rs` — 新增 `ToolSearchExecutor`
- 修改：`ccore/src/node/thinker/mod.rs` — `build_sample_request` 中默认只注入 summary 工具列表

**实现要点**：

1. `ToolDefinition` 增加 `summary` 字段，存储一句话描述（如 "执行 shell 命令"）
2. `ToolBridge` 新增 `tool_definitions_summary()` 方法，返回精简版（只有 name + summary，无 parameters）
3. 新增 `tool_search` 工具，参数为 `tool_names: Vec<String>`，返回请求工具的完整 schema
4. `ToolSearchExecutor` 从 `ToolBridge` 查找指定工具的完整定义并返回 JSON
5. `build_sample_request` 默认使用 summary 列表；当模型调用 `tool_search` 后，将返回的完整 schema 追加到 `config.tools` 中（后续轮次保持）
6. 为每个内置工具编写 summary（中文，≤30 字）

**核心逻辑示意**：
```rust
// bridge.rs 新增
pub fn tool_definitions_summary(&self) -> Vec<serde_json::Value> {
    self.entries.values().map(|e| {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": e.definition.name,
                "description": e.definition.summary.as_deref()
                    .unwrap_or(&e.definition.description),
                "parameters": serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tool_names": {
                            "type": "array",
                            "items": {"type": "string"}
                        }
                    }
                })
            }
        })
    }).collect()
}
```

---

## 任务 2：工具输出预算控制（P0）

**目标**：每个工具输出设 token 预算上限（默认 6000 token ≈ 24000 字符），超出部分截断并保留头尾

**文件**：
- 修改：`ccore/src/node/tool.rs` — `handle_tool_call` 执行完成后截断超大输出

**实现要点**：

1. 在 `ToolNode` 中新增常量 `TOOL_OUTPUT_BUDGET_CHARS: usize = 24_000`（约 6000 token）
2. `handle_tool_call` 执行完成后（`bridge.execute` 返回后），检查 `result.output.len()`
3. 超出预算时：保留前 10000 字符 + `\n\n[... 内容已截断，共 N 字符 ...]\n\n` + 后 10000 字符
4. 在截断提示中告知模型可用 `read_file` 查看完整内容
5. 对 `task`（子 agent）和 `tool_search` 工具不截断（它们的输出本身是结构化的）

**核心逻辑示意**：
```rust
// tool.rs handle_tool_call 中，execute 返回后
let output = result.output.clone();
if output.len() > TOOL_OUTPUT_BUDGET_CHARS
    && effective_request.tool_name != "task"
    && effective_request.tool_name != "tool_search"
{
    let head = &output[..10_000];
    let tail = &output[output.len()-10_000..];
    result.output = format!(
        "{}\n\n[... 内容已截断，原始大小 {} 字符，使用 read_file 查看具体行 ...]\n\n{}",
        head, output.len(), tail
    );
}
```

---

## 任务 3：子 Agent 摘要返回（P1）

**目标**：子 agent 完成后，将完整输出写入文件，生成 ≤500 token 的摘要注入父上下文

**文件**：
- 修改：`ccore/src/node/thinker/mod.rs` — 子 agent 完成处理逻辑
- 修改：`ccore/src/agent/subagent.rs` — `publish_completed` 中增加摘要字段

**实现要点**：

1. `SubAgentNode::publish_completed` 中，将完整输出写入 `.ccode/state/subagent-{id}.md`
2. 生成摘要：取输出的前 200 字符 + 最后 200 字符 + 提取所有文件路径（正则匹配 `/\S+\.\w+`）
3. 摘要格式：`[子Agent {type} 完成] 摘要：{前200字}...{后200字}。涉及文件：{paths}。完整输出：.ccode/state/subagent-{id}.md`
4. ThinkerNode 的 `/event` handler 中，用摘要替代完整输出注入 `working_memory`
5. 摘要 token 预算：≤ 500 token（约 2000 字符）

**核心逻辑示意**：
```rust
// subagent.rs publish_completed 中
let summary = if output.len() > 2000 {
    let paths: Vec<&str> = output.split_whitespace()
        .filter(|w| w.starts_with('/') && w.contains('.'))
        .take(10)
        .collect();
    format!(
        "[子Agent {} 完成] 摘要：{}...{}。涉及文件：{:?}。完整输出：.ccode/state/subagent-{}.md",
        self.definition.agent_type,
        &output[..200], &output[output.len()-200..],
        paths, self.id
    )
} else {
    output.to_string()
};
```

---

## 任务 4：稳定前缀强化 + cache_control 序列化（P1）

**目标**：修复两个已有但未生效的缓存机制——`lock_stable_prefix` 从未调用、`cache_control` 未序列化到 HTTP body

**文件**：
- 修改：`ccore/src/node/thinker/mod.rs` — 初始化后调用 `lock_stable_prefix`；工具定义按名字排序
- 修改：`ccore/src/sampler/claude_compat.rs` — 序列化 `cache_control` 字段
- 修改：`ccore/src/sampler/openai_compat.rs` — 序列化 `cache_control` 字段（message 级别）
- 修改：`ccore/src/node/tool.rs` — `tool/register` 广播前按名字排序

**实现要点**：

1. **工具排序**：`ToolBridge::tool_definitions()` 返回前按 `name` 字母序排序，确保跨轮次/跨会话稳定
2. **锁定前缀**：`ThinkerNode::new` 中，收到 `tool/register` 后调用 `working_memory.lock_stable_prefix()`，保护 system prompt + 工具定义不被压缩
3. **cache_control 序列化**：
   - `claude_compat.rs`：在 `build_request_body` 中，为 `messages` 和 `tools` 数组项写入 `cache_control` 字段
   - `openai_compat.rs`：在 `build_request_body` 中，为 `messages` 写入 `cache_control`（OpenAI 仅支持 message 级别缓存）
4. **Anthropic tools 缓存**：在 Claude provider 中，为 tools 数组最后一个元素添加 `cache_control: {type: "ephemeral"}`

**核心逻辑示意**：
```rust
// claude_compat.rs build_request_body 中
if !request.tools.is_empty() {
    let mut tools: Vec<serde_json::Value> = request.tools.iter().map(|t| {
        serde_json::json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.parameters,
        })
    }).collect();
    // 最后一个工具加 cache_control 断点
    if let Some(last) = tools.last_mut() {
        last.as_object_mut().unwrap().insert(
            "cache_control".to_string(),
            serde_json::json!({"type": "ephemeral"}),
        );
    }
    body["tools"] = serde_json::json!(tools);
}
```

---

**注意**：不需要编写测试用例，测试由后续的逻辑推演替代。
