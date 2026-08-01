# 需求分析：死代码接线 — 从"图纸"到"能力"

## 需求概述
将 ccore 中已实现但零调用的模块接到 ThinkerNode 的实际执行路径上，使 ccode Agent 的真实能力与代码能力一致。

## 业务背景

### 问题根因
ccode 当前有大量编译通过的模块，在运行时**零调用**——getter 没人调、循环只打日志、检索器无人实例化。这导致：
- Agent 真实能力 ≈ Codex CLI 不稳定分叉
- 文档宣称的能力在运行时不存在
- 评分虚高（之前按"代码存在"打分，实际应按"运行时贡献"打分）

### 死代码清单（经静态分析确认）

| 模块 | 文件 | 代码行 | 运行时贡献 | 死因 |
|------|------|--------|-----------|------|
| GoalLoop | agent/goal_loop.rs | ~460 | **0** | thinker.rs:862 只 `tracing::info!` 就 fall-through，从未 use/实例化 |
| ScheduleLoop | agent/schedule_loop.rs | ~300 | **0** | thinker.rs:867 只 `tracing::info!` 就 fall-through，从未 use/实例化 |
| ProactiveLoop | agent/proactive_loop.rs | ~350 | **0** | thinker.rs:870 只 `tracing::info!` 就 fall-through，从未 use/实例化 |
| IntentRetriever | memory/intent_retriever.rs | ~340 | **0** | 全仓零实例化（仅自身测试内用） |
| RepoMap | memory/repo_map.rs | ~700 | **0** | 全仓零实例化（仅自身测试内用） |
| CodeBlockParser | memory/function_embed.rs | ~600 | **0** | 全仓零实例化（仅自身测试内用） |
| VectorStore | memory/vector_store.rs | ~350 | **0** | 全仓零实例化（仅自身测试内用） |
| KernelSession | ccore_integration.rs:523 | ~180 | **0** | 自身文件外零引用 |
| McpBridge(ccore_integration) | ccore_integration.rs:743 | ~80 | **0** | call_tool 返回 Err stub |
| meta_cognitive | kernel/ 持有 | ~300 | **0** | getter `.meta_cognitive()` 全仓零调用 |
| experiential | kernel/ 持有 | ~200 | **0** | getter `.erl()` 全仓零调用 |
| decentralized | kernel/ 持有 | ~200 | **0** | getter `.coordinator()` 全仓零调用 |

### 存活代码清单（经确认运行时可达）

| 模块 | 运行时贡献 | 调用位置 |
|------|-----------|---------|
| WorkingMemory | **高** | ThinkerNode.listen/feel/build_sample_request/run_compaction_pipeline |
| LoopStateMachine | **中** | ThinkerNode.handle_message（DoomLoop/MaxTurns 事件驱动状态变迁） |
| DoomLoopDetector | **高** | ThinkerNode.handle_message（检测+逃脱策略） |
| EpisodicMemoryBridge | **中** | ThinkerNode.listen（search_relevant 注入长期记忆）+ stop（extract_and_store） |
| ShortTermMemory | **中** | ThinkerNode.listen/feel |
| SlidingWindow | **中** | ThinkerNode.update_context_window |
| CompactionPipeline | **高** | ThinkerNode.run_compaction_pipeline（3 层压缩） |
| ToolNode 权限链 | **高** | tool.rs:269 needs_confirmation + permission_rules:196 check_tool_call |

## 本服务职责

| 职责项 | 说明 |
|--------|------|
| 接线三大循环 | ThinkerNode 处理 /goal /schedule /proactive 时真调 GoalLoop/ScheduleLoop/ProactiveLoop |
| 接线 Context Engine | ThinkerNode.build_sample_request 真调 IntentRetriever/RepoMap/VectorStore |
| 接线高级认知 | Kernel getter 在 ThinkerNode 中被调用 |
| 接线 KernelSession | shell 实际走 KernelSession 启动 Kernel |
| 验证 ToolNode 权限链 | 确认 Shell 安全检查真执行 |

## 依赖关系

### 调用方（谁调用我）
| 调用方 | 接口 | 用途 |
|--------|------|------|
| ThinkerNode | handle_message | 用户输入/LLM响应/工具结果 |
| ToolNode | check_tool_call | 权限链+Shell安全 |
| shell SessionActor | KernelSession | 启动Kernel |

### 被调用方（我调用谁）
| 模块 | 方法 | 用途 |
|------|------|------|
| GoalLoop | step() | 目标驱动循环 |
| ScheduleLoop | tick() | 定时循环 |
| ProactiveLoop | scan() | 闲置扫描 |
| IntentRetriever | expand()+search() | 意图扩展+依赖链检索 |
| RepoMap | get_relevant_context() | 仓库依赖图 |
| VectorStore | search() | 向量检索 |
| meta_cognitive | evaluate() | 元认知评估 |
| experiential | extract_heuristics() | 经验学习 |
| KernelSession | send_input() | shell→Kernel 桥接 |

## 改动范围
- [ ] 修改 ThinkerNode：use + 实例化 + 真调三大循环和 Context Engine
- [ ] 修改 ThinkerNode：接入高级认知 getter
- [ ] 修改 KernelSession：shell 实际调用
- [ ] 修改 McpBridge：call_tool 真调 MCP Server
- [ ] 验证 ToolNode 权限链：check_shell_safety 真执行

## 风险与注意
- ⚠️ 接线后需要确保每个被调用的方法内部逻辑完整，不是 stub
- ⚠️ 三大循环需要 loop 驱动机制，不能只调一次 step() 就结束
- ⚠️ Context Engine 需要 Index 先 build 才能 search，build 可能耗时
- ⚠️ KernelSession 涉及 shell 和 ccore 两个 crate 的交叉修改
- 💡 优先级原则：P0=三大循环+Context Engine（直接影响能力评分），P1=高级认知（间接），P2=KernelSession（架构）
