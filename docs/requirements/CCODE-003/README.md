# CCODE-003: TUI 性能与交互优化

## 目标
对标 opencode / grok-build，提升 TUI 交互质量和流式渲染性能。

## 已实现改进

### 1. 流式渲染性能优化
- **O(1) 高度估算**：运行中的 entry 使用 `estimate_line_count()` 快速估算高度，避免每个 token 都触发完整的 markdown 渲染（syntect 高亮 + 自动换行）
- **延迟推送**：`push_chunk_deferred()` 替代 `push_chunk()`，跳过逐 token 的渲染
- **缓存跳过**：运行中的 entry 不调用 `invalidate_cache()`，完成后再做最终渲染
- **文件**：`scrollback/block.rs`, `scrollback/blocks/markdown_content.rs`, `scrollback/blocks/agent.rs`, `scrollback/state/mod.rs`, `scrollback/state/layout.rs`

### 2. 上下文工具动作标签
- 状态栏根据工具类型显示语义化标签：
  - Read/View → "Reading ..."
  - Edit/Write → "Writing ..."
  - Grep/Search → "Searching ..."
  - Glob/List → "Listing ..."
  - Agent/Task → "Running agent ..."
  - URL → "Fetching ..."
  - 默认 → "Run ..."
- **文件**：`views/turn_status.rs` (`tool_action_prefix()`)

### 3. 上下文使用量警告
- 当上下文使用超过 80% 时，状态栏显示 `⚠` 警告图标
- 配合已有的颜色渐变（text → warning → error）
- **文件**：`views/context_bar.rs`

### 4. 活动标签优化
- "Responding…" → "Generating…"（更准确描述模型正在生成输出）
- 终端标题栏同步更新
- 标题栏工具标签也使用上下文标签
- **文件**：`views/turn_status.rs`, `notifications/title.rs`

### 5. 编译修复
- 添加 `NodeLauncher::set_skills_count()` 和 `take_task_handles()` 方法
- 修复 `BufferGuard::as_mut()` → `inner_mut()` API 不匹配
- **文件**：`ccore/src/kernel/launcher.rs`, `ccore/src/node/transport.rs`

## 参考
- opencode TUI: `/Users/caomunian/Study/ref-agents/opencode/internal/tui/`
- 关键模式：上下文工作指示器、per-tool 动作标签、状态栏警告

## 测试
- context_bar 单元测试：15/15 通过
- 整体编译：通过（`cargo check -p ccode-pager`）
