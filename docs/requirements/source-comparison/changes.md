# 变更清单

## 文档产出

| 文件 | 类型 | 说明 |
|------|------|------|
| `docs/requirements/source-comparison/analysis.md` | 新增 | 6 款工具全维度对比分析 |
| `docs/requirements/source-comparison/plan.md` | 新增 | 基于对比的改进计划（3 Phase） |
| `docs/requirements/source-comparison/changes.md` | 新增 | 本文件 |
| `docs/requirements/source-comparison/workflow-state.json` | 新增 | 工作流状态 |
| `docs/requirements/source-comparison/tui-comparison.md` | 新增 | TUI 交互对比分析（修正版） |

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
