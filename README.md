# ccode

终端 AI 编程代理，基于 Rust 构建的自主代码智能体。

## 与 Claude Code 对比评估

| 维度 | ccode | Claude Code | 说明 |
|------|-------|-------------|------|
| **Agent Loop** | 9 | 8 | 四级循环工程（Turn/Goal/Schedule/Proactive）+ 三级 Doom Loop 逃脱 |
| **工具系统** | 9 | 7 | 文件事务、编译反馈闭环、ReadTracker、权限链 5 阶段 |
| **子Agent/Spawn** | 7 | 9 | 支持嵌套子代理，但 Team 系统和并发数不如 Claude Code |
| **系统提示** | 8 | 9 | 仿生架构提示，但 MCP 原生集成深度不如 Claude Code |
| **上下文压缩** | 8 | 8 | 三级压缩（Code/Intra/Inter），与 Claude Code 五层管道各有侧重 |
| **错误恢复** | 9 | 7 | 三级 Doom Loop 逃脱（注入提示→禁用工具→降级模型） |
| **权限与安全** | 7 | 9 | 权限链设计优秀，但实战打磨不如 Claude Code |
| **独有特性** | 9 | 6 | ERL 经验反思、反射弧、情景记忆、元认知、目标验证 |
| **Hooks 系统** | 8 | 7 | Gate/Observe 分阶段钩子，事件覆盖更全 |
| **UX/TUI** | 6 | 8 | 终端 UI 基础可用，但交互体验不如 Claude Code |
| **综合评分** | **80** | **74** | 核心差距在实战验证，需聚焦端到端集成测试 |

## 技术栈

- **语言**：Rust
- **消息总线**：ZeroMQ（ROUTER/DEALER + PUB/SUB）
- **终端 UI**：ratatui + crossterm
- **AI SDK**：async-openai + 多 Provider 适配（OpenAI/Claude/Gemini/DeepSeek/GLM）

## 开发

```bash
cargo build
cargo run -p ccode-cli
cargo run -p ccode-pager-bin
cargo clippy --all-targets
```

## 许可证

Apache-2.0