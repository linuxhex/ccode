# 变更清单

## 文档产出

| 文件 | 类型 | 说明 |
|------|------|------|
| `docs/requirements/source-comparison/analysis.md` | 新增 | 6 款工具全维度对比分析 |
| `docs/requirements/source-comparison/plan.md` | 新增 | 基于对比的改进计划（3 Phase） |
| `docs/requirements/source-comparison/changes.md` | 新增 | 本文件 |
| `docs/requirements/source-comparison/workflow-state.json` | 新增 | 工作流状态 |
| `docs/requirements/source-comparison/tui-comparison.md` | 新增 | TUI 交互对比分析（修正版） |
| `docs/requirements/source-comparison/deep-comparison.md` | 新增 | Phase B/C 深度对比：沙箱安全、Sub-Agent 生命周期、错误处理、流式/Prompt、配置/插件 |
| `docs/requirements/source-comparison/phase-d-comparison.md` | 新增 | Phase D 深度对比：记忆系统、工具执行管线、上下文压缩 + 优化方案 |
| `docs/requirements/source-comparison/phase-e1-comparison.md` | 新增 | Phase E1 深度对比：Plugin / Hook / Skill 系统 + 改进方案 |
| `docs/requirements/source-comparison/phase-f-comparison.md` | 新增 | Phase F 深度对比：编辑工具与补丁算法 / 会话持久化恢复回溯 / 代码检索与 repo map + 跨维度合并差距清单 |

## 源码获取记录

| 工具 | 路径 | 来源 |
|------|------|------|
| Claude Code | `ref-agents/claude-code` | 已有（公开仓库，闭源，仅文档+CHANGELOG） |
| Codex | `ref-agents/codex` | 已有（github.com/openai/codex, Apache 2.0） |
| OpenCode | `ref-agents/opencode` | 已有（github.com/anomalyco/opencode） |
| deepcode-cli | `ref-agents/deepcode-cli` | 新克隆（github.com/lessweb/deepcode-cli） |
| Qoder(ccode) | 当前工程 | 本地 |
| Harness CLI | `ref-agents/harness-cli` | 新克隆（github.com/harness/cli, Apache 2.0） |

## Phase A 代码改动（已完成）

### A1: Side Effect 声明 + Scope 评估

| 文件 | 改动 |
|------|------|
| `ccode-tools/.../ccode_build/bash/mod.rs` | `BashToolInput` 添加 `side_effects: Option<Vec<String>>` 字段 |
| `ccore/src/tools/permission.rs` | 添加 `SideEffectScope` 枚举 + `evaluate_side_effects()` 方法 |
| `ccode-tools/.../opencode/bash/mod.rs` | 添加 `side_effects: None` |
| `ccode-tools/src/types/tool_io.rs` | 添加 `side_effects: None` |
| `ccode-tools/src/registry/types.rs` | 添加 `side_effects: None` |
| `ccode-workspace/src/permission/types.rs` | 添加 `side_effects: None` |
| `ccode-shell/.../tool_calls.rs` | 添加 `side_effects: None` |
| `ccode-shell/.../tool_dispatch.rs` | 添加 `side_effects: None` |

### A2: Context Fragment 化

| 文件 | 改动 |
|------|------|
| `ccode-agent/src/prompt/context.rs` | 添加 `fragments()` 方法 + `PromptFragment` / `FragmentCategory` / `CachePriority` / `CompactionBehavior` 类型 |

### A3: Prompt Cache 断点

| 文件 | 改动 |
|------|------|
| `ccore/src/agent/subagent.rs` | 添加 `cache_control` 断点 + `prompt_cache_key` |

### A4: Agent 检测

| 文件 | 改动 |
|------|------|
| `ccode-telemetry/src/context.rs` | 添加 `HostingAgent` 枚举 + `detect_hosting_agent()` |

## TUI 优化改动（已完成）

### Reduced Motion 无障碍支持

| 文件 | 改动 |
|------|------|
| `ccode-pager-render/src/appearance/config.rs` | 添加 `AnimationConfig::reduced_motion` + `is_reduced()` 方法 |
| `ccode-pager/src/scrollback/blocks/thinking.rs` | Thinking block 动画检查 `reduced_motion` |

### Warming Spinner 颜色

| 文件 | 改动 |
|------|------|
| `ccode-pager/src/acp/tracker.rs` | `ToolRunning` 添加 `started_at: Instant` 字段 |
| `ccode-pager/src/views/turn_status.rs` | 添加 `warmed_tool_color()` + `lerp_color()` 函数 |
| `ccode-pager/src/notifications/title.rs` | 更新 `ToolRunning` 构造/匹配 |
| `ccode-pager/src/views/dashboard/row.rs` | 更新 `ToolRunning` 构造 |
| `ccode-pager/src/app/subagent.rs` | 更新 `ToolRunning` 构造/匹配 |

### 词级 Diff 高亮

| 文件 | 改动 |
|------|------|
| `ccode-pager/src/scrollback/blocks/tool/edit.rs` | 添加 `compute_word_diff_ranges()` + `compute_hunk_word_highlights()` + `apply_word_highlights()` |

## Phase B 代码改动（已完成）

### B3: 流空闲超时

| 文件 | 改动 |
|------|------|
| `ccore/src/sampler/idle_timeout.rs` | 新建 — `IdleTimeoutStream` 包裹器，默认 300s |
| `ccore/src/sampler/mod.rs` | 注册 `idle_timeout` 模块 |
| `ccore/src/sampler/provider.rs` | `SampleRequest` 添加 `idle_timeout_secs` 字段 |
| `ccore/src/sampler/{openai_compat,claude_compat,gemini}.rs` | 包裹流 + 测试更新 |
| `ccore/src/agent/subagent.rs` | 添加 `idle_timeout_secs: None` |
| `ccore/src/node/thinker/mod.rs` | 添加 `idle_timeout_secs: None` |

### B1: Side Effects → 生产权限

| 文件 | 改动 |
|------|------|
| `ccode-workspace/src/permission/side_effect_eval.rs` | 新建 — 10 种 scope 风险评估 + 10 个测试 |
| `ccode-workspace/src/permission/mod.rs` | 注册 `side_effect_eval` 模块 |
| `ccode-workspace/src/permission/types.rs` | `PermissionCommand::Request` 添加 `side_effects` 字段 |
| `ccode-workspace/src/permission/manager.rs` | actor 中接入 side_effect_eval 快速判定 + 所有构造点更新 |
| `ccode-shell/src/session/acp_session_impl/tool_calls.rs` | 提取 `BashToolInput.side_effects` 传入权限请求 |
| `ccode-shell/tests/test_mcp_permission_persistence.rs` | 添加 `side_effects: None` |

### B2: 4-Breakpoint 缓存策略

| 文件 | 改动 |
|------|------|
| `ccore/src/sampler/cache_placement.rs` | 新建 — 4-breakpoint 放置策略 + 7 个测试 |
| `ccore/src/sampler/mod.rs` | 注册 `cache_placement` 模块 |
| `ccore/src/node/thinker/mod.rs` | 替换硬编码 2-breakpoint → `place_breakpoints()` |
| `ccore/src/agent/subagent.rs` | 同上 |

### B4: 子代理图持久化

| 文件 | 改动 |
|------|------|
| `ccore/src/agent/graph_store.rs` | 新建 — 图存储 + LRU 驻留 + 树查询 + JSON 序列化 + 10 个测试 |
| `ccore/src/agent/mod.rs` | 注册 `graph_store` 模块 |

### B5: 子代理邮箱通信

| 文件 | 改动 |
|------|------|
| `ccore/src/agent/mailbox.rs` | 新建 — 有界邮箱 + Progress/Cancel/Custom 消息 + 11 个测试 |
| `ccore/src/agent/mod.rs` | 注册 `mailbox` 模块 |

### P0: 字节稳定工具 Schema

| 文件 | 改动 |
|------|------|
| `ccore/src/sampler/tool_stability.rs` | 新建 — 规范序列化 + schema hash + 稳定性追踪器 + 10 个测试 |
| `ccore/src/sampler/mod.rs` | 注册 `tool_stability` 模块 |
| `ccore/src/sampler/gemini.rs` | 工具按名称排序（与 Claude/OpenAI 对齐） |
| `ccore/src/node/thinker/mod.rs` | 接入 `ToolStabilityTracker` |

## Phase D 代码改动（已完成）

### D1: 上下文压缩管线

| 文件 | 改动 |
|------|------|
| `ccore/src/node/thinker/mod.rs` | 添加 `stable_prefix_locked` 标志，首次 `build_sample_request` 时调用 `lock_stable_prefix()` |
| `ccore/src/memory/working.rs` | 新增 `compact_reactive_llm()` async 方法 — Level 4 LLM 摘要压缩 + `recalculate_tokens()` 辅助 |
| `ccode-shell/src/session/acp_session_impl/tool_calls.rs` | 添加 `Semaphore(20)` 全局工具并发上限 |

### D2: 运行时自适应

| 文件 | 改动 |
|------|------|
| `ccore/src/node/thinker/mod.rs` | 添加 TokenBudgetReminder 注入：上下文 ≥70% 时在最后 user 消息追加预算提醒 |
| `common/ccode-compaction/src/micro_compact.rs` | 新增 `adaptive_max_age(pressure)` — 根据 token 压力线性插值 1.0x→0.2x |
| `ccode-shell/src/session/acp_session_impl/sampler_turn.rs` | 新增 `call_with_transient_retry()` — 瞬态错误指数退避重试（1s base, 2x/attempt） |
| `common/ccode-compaction/src/context_collapse.rs` | 新增 `compaction_quality_score()` — 检查文件路径/错误关键词/代码标识符保留率 |

### D3: 记忆系统增强

| 文件 | 改动 |
|------|------|
| `ccore/src/memory/short_term.rs` | 新增 `EmbeddingProvider` trait + `set_embedding_provider()` 方法 |
| `common/ccode-compaction/src/context_collapse.rs` | 新增 `INCREMENTAL_COLLAPSE_PROMPT` + `incremental_summary()` 增量摘要函数 |
| `ccore/src/memory/world_state.rs` | 新建 — Fragment + FNV-1a 指纹 + Priority 排序 + 变更检测 + 7 个测试 |
| `ccore/src/memory/mod.rs` | 注册 `world_state` 模块 |

## Phase E1 代码改动（已完成）

### E1.1: asyncRewake Hook

| 文件 | 改动 |
|------|------|
| `ccode-hooks/src/config.rs` | `RawHandler` + `HookSpec` 添加 `async_run`、`async_rewake`、`rewake_message` 字段 |
| `ccode-hooks/src/dispatcher.rs` | 测试辅助 `make_command_spec` 更新新字段默认值 |
| `ccode-hooks/src/runner/command.rs` | 7 处 `HookSpec` 测试构造更新 |
| `ccode-hooks/src/runner/http.rs` | 2 处 `HookSpec` 测试构造更新 |
| `ccode-hooks/src/discovery.rs` | 1 处 `HookSpec` 测试构造更新 |

### E1.2: 新增 Hook 事件（6 个）

| 文件 | 改动 |
|------|------|
| `ccode-hooks/src/event.rs` | `HookEventName` 新增 `TaskCreated`/`TaskCompleted`/`FileChanged`/`ConfigChange`/`PermissionRequest`/`CwdChanged` 变体 + `HookPayload` 对应变体 + Display/Deserialize/traits() |
| `ccode-hooks-types/src/lib.rs` | `HookEvent` 新增 6 个 ACP 线路格式变体 + Display |
| `ccode-hooks/src/discovery.rs` | `ALL_EVENTS` 常量添加 6 个新事件；测试穷举匹配更新 |
| `ccode-hooks/src/config.rs` | 测试 `claude_settings_with_unknown_hook_events_skipped_leniently` 更新断言（2→5） |
| `ccode-hooks/src/dispatcher.rs` | `total_variants` 15→21；`hub_hook_kind` 测试添加新事件（`PermissionRequest` hub_forward=false） |
| `ccode-shell/src/extensions/hooks.rs` | `hook_spec_to_info` 添加 6 个新 `HookEventName → HookEvent` 映射 |

### E1.3: if 条件过滤

| 文件 | 改动 |
|------|------|
| `ccode-hooks/src/config.rs` | `RawHandler` + `HookSpec` 添加 `condition: Option<String>` 字段 |

### E1.4: enabledSkills 开关

| 文件 | 改动 |
|------|------|
| `ccode-tools/src/implementations/skills/discovery.rs` | 新增 `filter_skills_by_config()` — 按 enabled/disabled 列表过滤 skill + 名称归一化 |

### E1.5: 插件本地设置

| 文件 | 改动 |
|------|------|
| `ccode-agent/src/plugins/local_settings.rs` | 新建 — `PluginLocalSettings` 结构 + `find_local_setting()` + `split_frontmatter()` + 4 个测试 |
| `ccode-agent/src/plugins/mod.rs` | 注册 `local_settings` 模块 + re-export |

### E1.6: 并行 Hook 调度

| 文件 | 改动 |
|------|------|
| `ccode-hooks/src/dispatcher.rs` | 新增 `dispatch_non_blocking_parallel()` — `tokio::task::JoinSet` 并发执行 observe-only hook |

## 关键发现

经过深入源码分析，Qoder pager 已经拥有与 Claude Code / Codex 同等水平的 TUI 功能：

- ✅ 增量 wrapping（等效 Codex 两区域模型）
- ✅ 帧合并 + 节流（等效 Codex FrameScheduler）
- ✅ 后台写入线程（Claude/Codex 均无）
- ✅ 零字节空闲帧（Claude/Codex 均无）
- ✅ Minimal 模式（Claude/Codex 均无）
- ✅ Sticky headers（Claude/Codex 均无）
- ✅ Diff 渲染 + 语法高亮
- ✅ 权限对话框 + Bash scope 选择
- ✅ 5 主题 + Auto + 量化
- ✅ Vim 模式 + 外部编辑器
- ✅ 折叠工具结果 + OSC 9;4 进度

## Phase G 代码改动（进行中）

> 基于 Claude Code 实际源码（~1884 TypeScript 文件）+ grok-build 对比的新一轮改进

### G1: 秘密脱敏（安全 P0）

| 文件 | 改动 |
|------|------|
| `ccode-telemetry/Cargo.toml` | 添加 `regex` 依赖 |
| `ccode-telemetry/src/redact_common.rs` | 实现 `redact_owned()` — 移植 grok-build `sanitizer.rs` 的正则匹配集（API key/AWS/GitHub/GitLab/Slack/Google/PEM/JWT/Bearer/URL 敏感参数/密钥赋值模式）+ 用户路径脱敏 + JSON 遍历脱敏 |

### G3: read_tracker 版本追踪（编辑安全 P1）

| 文件 | 改动 |
|------|------|
| `ccore/src/tools/read_tracker.rs` | `HashSet<String>` → `HashMap<PathBuf, FileStamp>`；新增 `FileStamp` 结构（mtime + len）；`mark_read` 记录文件戳记；`require_read` 严格版本检查（检测外部修改）；新增 `require_read_lenient` 宽松版本（仅检查是否读过）；API 从 `&str` 改为 `&Path` |
| `ccore/src/tools/builtin.rs` | read_file 使用 `mark_file_read_with_stamp` 记录版本；write_file 使用严格 `require_file_read`；search_replace 使用宽松 `require_file_read_lenient`；所有路径改用 `validation.canonical` |
| `ccore/tests/integration/tool_chain.rs` | 更新测试使用 `&Path` |
| `ccore/src/node/thinker/mod.rs` | `mark_read(&path)` → `mark_read_simple(Path::new(&path))` — 适配新 API |

### G7: P1-1 残留修复 + P1-3 路径规范化

| 文件 | 改动 |
|------|------|
| `ccode-mcp/src/credentials.rs` | `with_extension("tmp")` → 唯一临时文件名（保留原扩展名） |
| `ccore/src/persistence/file_storage.rs` | 同上 |

### G2: 文件检查点系统（编辑安全 P0）

| 文件 | 改动 |
|------|------|
| `ccore/src/tools/file_history.rs` | 新建 — `FileHistory` 内存级检查点系统：`capture_before_edit()` 编辑前保存原始内容、版本递增（v1/v2/v3）、LRU 淘汰（MAX_SNAPSHOTS=50）、`restore_version()`/`restore_latest()` 回滚接口、8 个单元测试 |
| `ccore/src/tools/mod.rs` | 注册 `file_history` 模块 |
| `ccore/src/tools/builtin.rs` | `WriteExecutor::execute()` 和 `EditExecutor::execute()` 在原子写入前调用 `capture_before_edit()` 保存原始内容 |

### G4: 编辑工具增强（EOL 保真）

| 文件 | 改动 |
|------|------|
| `ccore/src/tools/builtin.rs` | 新增 `preserve_eol()` 函数；`execute_replace()` 检测原文件 EOL 风格并统一替换文本的行尾；`execute_insert()` 使用检测到的 EOL 风格 join 行；移除 `detect_line_ending()` 的 `#[allow(dead_code)]` |

### G5: Context Engine 死代码清理

| 文件 | 改动 |
|------|------|
| `ccore/src/memory/intent_retriever.rs` | 移除零调用方法：`set_repo_map()`、`register_block()`、`register_blocks()`、`expand_with_dependencies()`、`block_count()`；移除未使用的 `repo_map` 字段和 `RepoMap` import |
| `ccode-shell/src/session/ccore_integration.rs` | 移除零调用包装方法：`register_code_blocks()`、`init_repo_map()`、`repo_map_summary()`；为 `search_by_intent()` 添加占位说明注释 |

### G6: 工具结果预算裁剪（已有等效实现）

经分析，ccode 已实现与 Claude Code `applyToolResultBudget()` 等效的功能：
- ToolNode 层 24K 字符裁剪（head 10K + tail 10K，UTF-8 安全边界）
- 4 级压缩管线（Snip → MicroCompact → Micro → Auto）
- Token 预算管理器 + 70% 提醒注入

无需额外改动。

### 编译检查结果（2026-09-04 最终）

| Crate | 结果 |
|-------|------|
| `ccore --lib` | ✅ 通过 |
| `ccore --tests` | ✅ 通过（file_history 8 个测试全部通过） |
| `ccode-telemetry --lib` | ✅ 通过 |
| `ccode-mcp --lib` | ✅ 通过 |
| `ccode-shell --lib` | ✅ 通过 |
| 全工作区 `--workspace --lib` | ✅ 通过 |
