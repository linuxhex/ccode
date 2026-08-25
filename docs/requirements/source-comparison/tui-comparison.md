# TUI 交互对比分析 — Qoder vs Claude Code / Codex / OpenCode / deepcode-cli

> 分析日期：2026-08-25

---

## 一、架构对比

| 维度 | Qoder (ccode) | Codex | Claude Code | OpenCode | deepcode-cli |
|------|--------------|-------|-------------|----------|--------------|
| **框架** | ratatui + crossterm | ratatui + crossterm | 自有闭源 | BubbleTea (Elm) | React + Ink |
| **帧率控制** | 16ms (~60fps) | 8.3ms (120fps cap) + FrameScheduler actor | 未公开 | 无显式限制 | Ink 管理 |
| **渲染模式** | 直接 buffer 操作 | Renderable trait + FlexRenderable | 全屏无闪烁 | 组件 View() | `<Static>` 不可变历史 |
| **状态模型** | TUINode 单体 + TuiState (迁移中) | App → ChatWidget → BottomPane 三层 | 闭源 | Elm 单体 appModel | React useState hooks |
| **流式渲染** | 每帧重渲染最后一条消息 | **两区域模型** (stable + mutable tail) | 逐行流式 + 100ms 合并 | PubSub 驱动 | onAssistantMessage 回调 |
| **增量 Markdown** | checkpoint 冻结尾 | **StreamingRender** 稳定前缀保留 | 未公开 | 按消息缓存 | Ink Static 一次渲染 |

**核心差距**：
- Codex 的 **两区域流式模型** 是最先进的：stable 区域推入 scrollback 后不再修改，只有 tail 区域可变。Qoder 的 checkpoint 系统类似但缺少 stable region 推入 scrollback 的能力
- Claude Code 的 **100ms 合并** 减少 37% CPU 开销，Qoder 无类似合并机制

---

## 二、交互功能对比

| 功能 | Qoder | Codex | Claude Code | OpenCode | deepcode-cli |
|------|:-----:|:-----:|:-----------:|:--------:|:------------:|
| **Diff 渲染** | ❌ | ✅ 统一 diff + 语法高亮 + 主题感知 | ✅ 词级 diff | ✅ diff 预览 | ✅ Changes 标记 |
| **权限对话框** | ❌ | ✅ ApprovalOverlay | ✅ 全屏对话框 | ✅ overlay + shadow | ✅ 作用域风险着色 |
| **Vim 模式** | ❌ | ✅ 完整 Normal/Insert/Operator/Motion | ✅ 含反向搜索 | ❌ | ❌ |
| **自定义快捷键** | ❌ | ✅ 11 上下文 + chord | ✅ /keybindings | ❌ | ❌ |
| **Focus 视图** | ❌ | ❌ | ✅ Ctrl+O 折叠工具活动 | ❌ | ❌ |
| **多主题** | ❌ 仅暗色 | ✅ 32 主题 + 运行时切换 | ✅ 自定义 + Auto | ✅ 10 主题 | ❌ |
| **外部编辑器** | ❌ | ✅ Ctrl+G | ✅ Ctrl+G | ✅ Ctrl+E | ❌ |
| **反向搜索** | ❌ | ✅ Ctrl+R | ✅ / 反向搜索 | ❌ | ❌ |
| **图片粘贴** | ❌ | ❌ | ✅ Ctrl+V | ✅ Ctrl+F | ✅ Ctrl+V |
| **鼠标支持** | ✅ 点击/双击/拖拽 | ❌ | ✅ 全屏模式 | ❌ | ❌ |
| **可配置状态栏** | ❌ 硬编码 | ✅ /statusline | ✅ JSON 输入 | ❌ | ✅ provider 插件 |
| **终端进度条** | ❌ | ❌ | ✅ OSC 9;4 | ❌ | ❌ |
| **减动模式** | ❌ | ✅ Reduced Motion | ✅ Reduce motion | ❌ | ❌ |
| **屏幕阅读器** | ❌ | ❌ | ✅ 完整无障碍 | ❌ | ❌ |
| **Emoji 自动补全** | ❌ | ❌ | ✅ :heart: | ❌ | ❌ |
| **粘贴折叠** | ❌ | ✅ PasteBurst | ✅ Pasting... 提示 | ❌ | ✅ 标记可折叠 |

---

## 三、Qoder 缺失功能优先级排序

### P0 — 直接影响核心体验

| # | 功能 | 来源 | 影响 | 复杂度 |
|---|------|------|------|--------|
| 1 | **Diff 渲染** | Codex diff_render.rs | 编辑工具无 diff 预览，用户盲审 | 中 |
| 2 | **权限对话框** | Codex approval_overlay.rs | 无审批 UI，安全流程断裂 | 中 |
| 3 | **Focus 视图** | Claude Code /focus | 长会话工具卡片淹没对话 | 低 |

### P1 — 显著提升效率

| # | 功能 | 来源 | 影响 | 复杂度 |
|---|------|------|------|--------|
| 4 | **多主题 + 亮色** | Codex theme_picker.rs | 仅暗色，亮终端不适配 | 中 |
| 5 | **自定义快捷键** | Codex keymap.rs | 硬编码键位无法定制 | 中 |
| 6 | **Vim 模式** | Codex textarea/vim.rs | 重度用户刚需 | 高 |
| 7 | **外部编辑器** | Claude Code Ctrl+G | 长文本编辑不便 | 低 |

### P2 — 锦上添花

| # | 功能 | 来源 | 影响 | 复杂度 |
|---|------|------|------|--------|
| 8 | **反向搜索** | Codex Ctrl+R | 历史命令查找 | 低 |
| 9 | **可配置状态栏** | Claude Code statusline | 信息展示不灵活 | 中 |
| 10 | **终端进度条** | Claude Code OSC 9;4 | 无视觉进度反馈 | 低 |
| 11 | **流式合并** | Claude Code 100ms | 高 token 时 CPU 浪费 | 低 |
| 12 | **减动模式** | Codex motion.rs | 动画敏感用户 | 低 |

---

## 四、Codex 独有架构优势（值得深度借鉴）

### 4.1 两区域流式模型
```
stable region (已推入 scrollback，不可变)
    ↓ scroll_region_up()
tail region (mutable，当前显示)
    ↓ 完成后推入 stable
```
Qoder 的 checkpoint 系统已有类似思路，但缺少 stable → scrollback 的推送管道。

### 4.2 HistoryCell trait
Codex 将对话历史抽象为 `HistoryCell` trait，每种内容类型（exec/patch/message/mcp/plan/approval/hook/session/notice）实现自己的渲染。Qoder 的 `Entry` 模型是扁平的 `Vec<ChatMessage>`，缺少这种多态能力。

### 4.3 FrameScheduler Actor
Codex 的帧请求通过独立的 tokio actor 合并，避免多个组件同时请求渲染时的冗余绘制。Qoder 用简单的 `needs_render` bool，可能在一帧内被多次设置。

### 4.4 Resize Reflow 防抖
Codex 对 terminal resize 触发的 transcript 重排做了防抖（`TRANSCRIPT_REFLOW_DEBOUNCE`），避免快速拖拽窗口时的连续重渲染。Qoder 在宽度变化时直接清除所有缓存。

---

## 五、建议实施路线

### Sprint 1: Diff 渲染 + 权限对话框 (P0)
1. 移植 Codex 的 `diff_render.rs` 核心逻辑（统一 diff + 语法高亮 + 主题感知）
2. 实现 `ApprovalOverlay` 权限对话框（支持 bash/write/edit 三种类型）
3. 在 tool sidebar 中集成 diff 预览

### Sprint 2: Focus 视图 + 外部编辑器 (P0+P1)
1. 实现 Focus 模式：折叠工具卡片为单行摘要，显示编辑 diffstat
2. Ctrl+G 打开 `$EDITOR` 编辑长文本

### Sprint 3: 多主题 + 快捷键 (P1)
1. 主题系统：至少 dark/light/auto 三种，运行时切换
2. 可配置快捷键：context-specific keymap + 用户覆盖

### Sprint 4: Vim + 反向搜索 (P1+P2)
1. Vim 模式：Normal/Insert 两态 + 基础 motion
2. Ctrl+R 反向历史搜索
