# 需求分析：对标 Claude Code 补齐三大差距

## 需求概述
补齐 ccode 对标 Claude Code 的三个核心差距：MCP Server 模式、流式 Token 渲染、Hook 系统接线，将评分从 76 分提升至 90+。

## 业务背景
当前 ccode 与 Claude Code 评分对比（10 分制 × 13 维度）：

| 维度 | ccode | Claude Code | 差距 |
|------|-------|-------------|------|
| MCP Server | 2 | 9 | -7（最大短板） |
| 流式渲染 | 4 | 9 | -5（用户体验核心） |
| Hook 接线 | 5 | 8 | -3（工程完整性） |

补齐后预期：MCP Server 8、流式渲染 7、Hook 接线 8，总分提升约 12 分 → 88 分。

## 逐项分析

### 一、MCP Server 模式（2→8）

**现状**：
- ccode-mcp 已有 **MCP 客户端**实现（rmcp SDK + stdio/SSE transport + servers.rs 管理子进程）
- ccode-hub-mcp 有 JSON-RPC 消息处理和工具注册接口
- **缺失**：ccode 自身作为 MCP **Server** 暴露给 IDE，让 IDE 能发现和调用 ccode 的工具

**对标 Claude Code**：
- Claude Code 在 MCP Server 模式下通过 stdio 暴露工具给 IDE
- 工具包括：read、write、edit、bash、glob、grep 等
- JSON-RPC 2.0 协议：initialize → tools/list → tools/call → 结果返回
- 支持 SSE transport（HTTP 长连接）作为备选

**实现方案**：
- 在 ccore 中新增 `mcp_server` 模块，使用 rmcp 的 Server 端 API
- ToolNode 注册工具时同步注册到 MCP Server
- 支持两种 transport：stdio（CLI 场景）和 SSE（IDE 场景）
- Kernel 启动时可选启动 MCP Server（--mcp-server 标志）

### 二、流式 Token 渲染（4→7）

**现状**：
- ccode-shell 有 streaming_capture.rs 和 sampler_turn.rs 处理 LLM 流式响应
- ccode-pager 有 agent_view/render.rs 做最终渲染
- **缺失**：render 逻辑是批处理模式，等全部响应完成后才刷新；无 token-by-token 实时显示

**对标 Claude Code**：
- Claude Code 使用 Ink（React for CLI）做增量渲染
- 每个 SSE chunk 到达时立即触发 re-render
- 工具调用和文本交错显示：文本流式输出 → 工具调用块 → 工具结果 → 文本继续

**实现方案**：
- 在 ccode-pager 的 AgentView 中增加 StreamingRenderer
- 维护一个增量缓冲区，每个 token append 后触发 ratatui 局部刷新
- 工具调用块用独立组件渲染，支持"进行中"动画
- 响应完成后切换到完整渲染模式（当前行为）

### 三、Hook 系统接线（5→8）

**现状**：
- ccode-hooks 有完整实现：dispatcher.rs 有 pre_tool_use/post_tool_use
- 支持 permission chain、hook rewrite（updatedInput/additionalContext）
- 支持 command/http/timeout 等多种 runner
- **缺失**：ccore 的 ToolNode 工具执行前后未调用 Hook dispatcher

**对标 Claude Code**：
- Claude Code 在每个工具调用前触发 pre-tool-use hooks
- hook 可返回 allow/deny/rewrite
- 工具调用后触发 post-tool-use hooks
- hook 可修改工具输入、注入上下文

**实现方案**：
- 在 ccore 的 ToolNode 中注入 HookDispatcher 引用
- 工具执行前调用 dispatch_pre_tool_use → 检查 decision
- 工具执行后调用 dispatch_post_tool_use → 记录结果
- 从 ccode-shell 的 HookRegistry 传递到 ccore 的 ToolNode

## 依赖关系

| 组件 | 依赖 |
|------|------|
| MCP Server | rmcp SDK（已有）、ccore/tools/bridge.rs（工具桥接） |
| 流式渲染 | ccode-pager（ratatui）、ccode-shell/sampler_turn.rs（token 流） |
| Hook 接线 | ccode-hooks（已有 dispatcher）、ccore/node/tool.rs（工具节点） |

## 风险与注意
- MCP Server 需要确保 rmcp 的 Server 端 API 稳定（当前 rmcp 主要用于 Client 端）
- 流式渲染需避免频繁 full-redraw 导致终端闪烁，使用 ratatui 的局部刷新
- Hook 接线需确保 dispatcher 的 fail-open 策略在 ccore 中也生效，避免 hook 超时阻塞工具执行
- 三项改动都涉及 ccore ↔ ccode-shell 的接口变更，需保持向后兼容
