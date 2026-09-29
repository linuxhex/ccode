# TUI 交互对比分析（修正版）

## 概述

经过深入源码分析，Qoder 的 pager（生产 TUI）已经非常成熟，拥有与 Claude Code 和 Codex 相当的大部分功能。之前的对比报告存在误判，本文档提供修正后的分析。

## 功能对比矩阵

| 功能 | Claude Code | Codex | Qoder Pager | 状态 |
|------|-------------|-------|-------------|------|
| **Diff 渲染** | 行级 diff | 语法高亮 diff | ✅ 统一/并排 + 渐进语法高亮 | 已实现 |
| **权限对话框** | 内联确认 | ApprovalOverlay | ✅ Bash scope 选择 + MCP scope | 已实现 |
| **流式渲染** | Ink 流式 | 两区域模型 | ✅ Checkpoint + 增量 wrapping | 已实现 |
| **主题系统** | 自动匹配终端 | 32 主题 | ✅ 5 主题 + Auto + 量化 | 已实现 |
| **帧合并** | 100ms 合并 | FrameScheduler 120fps | ✅ Presenter + min_draw_interval | 已实现 |
| **折叠工具结果** | ✅ | ✅ | ✅ Collapsed/Expanded/Truncated | 已实现 |
| **Vim 模式** | ❌ | ✅ | ✅ /vim-mode | 已实现 |
| **外部编辑器** | Ctrl+G | ❌ | ✅ $EDITOR 集成 | 已实现 |
| **文本选择** | ❌ | ✅ | ✅ 视觉 + 持久化 + 拖拽 | 已实现 |
| **鼠标支持** | ❌ | ✅ | ✅ Hover/Click/Scroll | 已实现 |
| **OSC 9;4 进度** | ✅ | ❌ | ✅ 终端标签进度条 | 已实现 |
| **Sticky headers** | ❌ | ❌ | ✅ 用户提示固定 | 已实现 |
| **Minimal 模式** | ❌ | ❌ | ✅ 原生 scrollback | 已实现 |
| **后台写入线程** | ❌ | ❌ | ✅ 非阻塞 I/O | 已实现 |
| **零字节空闲帧** | ❌ | ❌ | ✅ 无变化时不写入 | 已实现 |
| **Focus 视图** | /focus | ❌ | ✅ 文件聚焦 | 已实现 |
| **Reduced motion** | ❌ | ❌ | ✅ `reduced_motion` 配置 | **新增** |

## Qoder 的技术优势

### 1. 增量 Wrapping（已实现）

`MarkdownContent` 的 `ensure_wrapped()` 方法实现了与 Codex 两区域模型等效的优化：

```rust
// 冻结行跟踪
let frozen_count = state.renderer.frozen_lines_count();

// 只重新 wrap 新冻结的行 + 尾部行
// 缓存的冻结行输出保持不变
state.cache_lines.truncate(frozen_wc);
// 追加新冻结的 wrapped 行
// 追加尾部 wrapped 行
```

**复杂度**：O(N) 而非 O(N²)，与 Codex 的 `StreamCore` 等效。

### 2. 帧合并与节流（已实现）

`Presenter` 结构体提供：
- `request_throttled(now, min_draw_interval)` — 帧率限制
- `draw_scheduled_at` — 延迟绘制调度
- `in_flight_target` — 帧序列跟踪

与 Codex 的 `FrameScheduler` actor 模式等效。

### 3. 后台写入线程（Qoder 独有）

`TermWriter` 通过 `mpsc::channel` 将帧字节发送到后台 OS 线程，实现非阻塞 I/O。Claude Code 和 Codex 都没有这个优化。

### 4. 零字节空闲帧（Qoder 独有）

`draw_frame()` 比较新旧缓冲区，如果没有任何单元格变化且光标位置未变，则不写入任何字节。这在空闲时完全消除终端 I/O。

### 5. Minimal 模式（Qoder 独有）

实验性的 scrollback-native 渲染模式，将完成的块直接打印到终端的原生 scrollback，而不是在 alternate screen 中渲染。这是 Claude Code 和 Codex 都没有的创新功能。

## 实际差距

### 1. Reduced Motion 无障碍支持（已修复）

**之前**：没有 `reduced_motion` 配置项来禁用动画。

**修复**：
- 添加 `AnimationConfig::reduced_motion` 字段
- 支持 `REDUCED_MOTION` 环境变量
- 在 thinking block 的 `accent()` 方法中检查 `reduced_motion`
- 当启用时，使用静态 accent 而非动画 wave

**文件**：
- `ccode-pager-render/src/appearance/config.rs`
- `ccode-pager/src/scrollback/blocks/thinking.rs`

### 2. 反向搜索（Ctrl+R）

**状态**：未实现

**描述**：Shell 风格的命令历史搜索。Codex 和 Claude Code 都没有这个功能，但它是 CLI 用户的常见期望。

**优先级**：P2（低）

### 3. 自定义键位映射

**状态**：部分实现

**描述**：Codex 有 `RuntimeKeymap` 支持 11 个上下文和组合键。Qoder 的键位是硬编码的，没有用户可配置的键位映射。

**优先级**：P2（低）— vim 模式已经提供了键位自定义的主要用例。

## 性能对比

| 指标 | Claude Code | Codex | Qoder |
|------|-------------|-------|-------|
| 流式渲染复杂度 | O(N²) Ink re-render | O(N) 两区域 | O(N) checkpoint + 增量 wrap |
| 帧率限制 | 100ms 合并 | 120fps 上限 | 可配置 fps（默认 30） |
| 空闲时 CPU | 低 | 低 | **极低**（零字节帧） |
| 长响应内存 | 高 | 中 | **低**（frozen line cache） |

## 结论

Qoder 的 pager 在 TUI 交互方面已经与 Claude Code 和 Codex 处于同一水平，在多个方面（后台写入线程、零字节空闲帧、Minimal 模式、Sticky headers）甚至领先。

本次优化仅添加了 `reduced_motion` 无障碍支持，这是一个小而重要的可访问性改进。之前的对比报告高估了差距，实际上 Qoder 的 TUI 架构非常先进。

## 实施记录

### 已完成的优化

1. **Reduced Motion 配置**
   - 文件：`ccode-pager-render/src/appearance/config.rs`
   - 添加 `AnimationConfig::reduced_motion` 字段
   - 添加 `AnimationConfig::is_reduced()` 方法（检查配置 + 环境变量）
   - 更新 `RawAnimationConfig` 和 `From` 转换
   
2. **Thinking Block 动画禁用**
   - 文件：`ccode-pager/src/scrollback/blocks/thinking.rs`
   - 在 `accent()` 方法中检查 `ctx.appearance.animation.reduced_motion`
   - 当启用 reduced motion 时使用静态 accent

### 取消的优化

- **流式行提交优化**：调查发现 `MarkdownContent` 已经实现了增量 wrapping 和 frozen line 跟踪，与 Codex 的两区域模型等效。无需额外优化。

## 配置示例

用户可以在 `~/.ccode/pager.toml` 中启用 reduced motion：

```toml
[animation]
reduced_motion = true
```

或者通过环境变量：

```bash
export REDUCED_MOTION=1
ccode
```
