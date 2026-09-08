# CCODE-004 改动简述

## 改动文件

| 文件 | 改动 |
|------|------|
| `ccore/src/node/tool.rs` | 工具输出预算控制（24000 字符截断）+ tool_search 特殊处理 |
| `ccore/src/node/thinker/mod.rs` | 工具延迟加载 + 稳定前缀锁定 + 子 Agent 摘要返回 + 推理力度分级 |
| `ccore/src/tools/bridge.rs` | 新增 tool_search 工具注册 |
| `ccore/src/tools/compile_feedback.rs` | 多语言编译检查：TypeScript（tsc）、Go（go build）、Python（py_compile）+ 统一调度器 |
| `ccore/src/tools/mod.rs` | 导出多语言编译检查函数 |
| `ccore/src/sampler/claude_compat.rs` | cache_control 序列化 + 工具排序 + tools 缓存断点 |
| `ccore/src/sampler/openai_compat.rs` | cache_control 序列化 + 工具排序 |
| `ccore-advanced/src/agent/meta_cognitive.rs` | DifficultyLevel 新增 to_reasoning_effort() 映射方法 |
| `ccore/src/prompt/agents_md.rs` | AGENTS.md 大小限制（8KB 预算，超出截断） |
| `ccode-cli/src/main.rs` | API 启动预热（异步 HEAD 请求预热 TCP/TLS 连接） |
| `ccode-shell/src/session/acp_session.rs` | 新增 current_turn_edited_files 字段跟踪编辑文件 |
| `ccode-shell/src/session/acp_session_impl/turn.rs` | 多语言编译反馈：跟踪编辑文件路径，按项目类型调度检查器 |
| `ccore/src/tools/bridge.rs` | 新增 PreExecuteHook trait + pre_hooks 字段 + register_pre_hook 方法 |
| `ccore/src/tools/git_snapshot_hook.rs` | Git 快照钩子：Write/Edit 前自动 git commit 创建快照点，支持 undo |
| `ccore/src/tools/builtin.rs` | 注册 GitSnapshotHook 为 pre_hook |
| `ccode-chat/src/compaction_utils.rs` | 9 类结构化摘要验证：validate_structured_summary + is_structurally_complete + is_degenerate_summary 增加结构检查 |
| `ccode-shell/src/session/acp_session.rs` | 新增 speculative_tool_results 字段（Arc<Mutex<HashMap>>）缓存投机执行结果 |
| `ccode-shell/src/session/acp_session_impl/tool_calls.rs` | 投机执行：ToolCallReady 时预执行只读工具 + execute_tool_calls 缓存命中跳过重复执行 |

## 改动说明

### P0+P1 改进（对标竞品核心差距）

1. **工具延迟加载**（对标 Claude Code 延迟工具加载 + Codex 最小工具集）：核心工具保持完整 schema，13 个非核心工具只发一句话摘要，模型通过 tool_search 按需获取完整定义
2. **工具输出预算控制**（对标 Claude Code applyToolResultBudget）：超过 24000 字符的工具输出截断，保留头尾各 10000 字符，UTF-8 安全
3. **子 Agent 摘要返回**（对标 Claude Code 子 agent 摘要返回）：超过 2000 字符的子 agent 输出压缩为摘要（前 200 + 后 200 + 文件路径），完整输出存 .ccode/state/ 文件
4. **稳定前缀 + cache_control**（对标 Codex 无状态请求 + Claude Code cache_edits）：工具按名字排序、首次输入后锁定稳定前缀、Claude/OpenAI provider 正确序列化 cache_control 断点

### P0 改进（竞品差距补全）

5. **推理力度分级**（对标 DeepSeek Harness off/high/max）：将 MetaCognitiveController 的难度评估映射到 reasoning_effort（Trivial→off, Moderate→0.5, Complex→0.8, Extreme→1.0），简单任务省 50%+ 推理 token
6. **AGENTS.md 大小限制**（对标 Codex 渐进加载）：总内容超 8KB 时截断，提示模型用 read_file 查看完整内容，防止大型 AGENTS.md 撑爆系统提示

### 功能性改进（快、好用、准确）

7. **API 启动预热**：CLI 启动时异步 HEAD 请求 /v1/models，预热 TCP/TLS 连接，首次 API 调用减少 200-500ms 延迟
8. **多语言编译反馈**（原仅支持 Rust cargo check）：编辑后按项目类型自动运行对应语言的编译检查——TypeScript（npx tsc --noEmit）、Go（go build ./...）、Python（python -m py_compile），解析编译器输出注入下一轮上下文，帮助模型自我修正编译错误
9. **Git 快照 undo/redo**：Write/Edit 工具执行前自动创建 git commit 快照点（`ccode-snapshot: write src/foo.rs`），用户可通过 `git log --grep=ccode-snapshot` 查看历史，`git revert <hash>` 回退到任意快照点。快照失败不阻塞工具执行
10. **压缩后 9 类结构化摘要验证**：上下文压缩时要求模型输出 9 个 section（Primary Request / Key Concepts / Files / Errors / Problem Solving / User Messages / Pending Tasks / Current Work / Next Step），新增 `validate_structured_summary` 验证器检查 section 覆盖率，`is_degenerate_summary` 增加结构完整性检查（至少 7/9 section 存在），防止模型输出无结构的大段文字导致压缩后丢失用户意图
11. **只读工具投机执行**：模型流式输出时，每个只读工具调用参数完整后立即 `tokio::spawn` 预执行（read_file / grep / list_dir / web_search 等），结果缓存到 `speculative_tool_results`。`execute_tool_calls` 分发前检查缓存，命中则直接使用预执行结果，跳过重复执行。模型生成后续 tool call 的时间被利用来并行执行前面的只读工具，减少端到端延迟。优化：复用已解析的 `raw_input` 避免双重 JSON 解析；缓存命中路径补充 `record_read` / `route_reflex` ccore 副作用；三组遥测事件（`speculative_hit` / `speculative_miss` / `speculative_dispatch_hit`）追踪命中率与预执行耗时
