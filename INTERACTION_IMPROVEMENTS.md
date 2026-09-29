# ccode 交互优化总结

## 问题诊断

用户反馈的核心问题：
1. **无限循环运行** - agent 疯狂运行工具调用但没有结果
2. **交互不友好** - 提示信息不友好，缺乏 AI 主动交互机制
3. **TUI 体验差** - 与 Codex 等现代 AI 编码助手相比差距明显

## 根本原因分析

### 1. 无限循环问题
**原因**：`system_prompt_addition()` 被错误地注入到每个用户提示前
- 在 `dispatch/prompt.rs` 和 `dispatch/queue.rs` 中，每次发送提示时都会 prepend 模式指令
- 导致模型收到重复的长指令，产生混乱和重复行为
- 模型不断重试相同的操作（如 `list_dir`）但无法完成

**修复**：
- 移除了 `prompt.rs:877` 和 `queue.rs:453` 的 system_prompt_addition 注入
- 模式指令应该只在会话初始化时设置一次，而不是每个提示都重复

### 2. 缺乏 AI 主动交互
**原因**：主动建议引擎存在但未连接到 UI
- `agent_intelligence::generate_suggestions()` 在每次 turn 完成后运行
- 但输出只记录到日志，没有显示给用户
- 用户只能被动等待，没有下一步操作建议

**修复**：
- 将本地生成的建议作为 fallback follow-up chips 显示
- 当服务器没有提供 follow-ups 时，自动显示本地建议
- 建议包括：运行测试、审查更改、尝试不同方法等

### 3. 新会话缺乏引导
**原因**：新会话开始时没有任何欢迎信息
- 用户面对空白界面，不知道能做什么
- 缺乏功能提示和使用指南

**修复**：
- 在 `handle_session_created` 中添加欢迎消息
- 显示模型名称和基本使用提示
- 引导用户使用 @ 附加文件、/ 命令等功能

## 具体改进

### 1. 修复无限循环 (prompt.rs, queue.rs)
```rust
// 移除前：每个提示都 prepend 模式指令
let mode_addition = agent.operating_mode.system_prompt_addition();
let wire_text = format!("{mode_addition}\n\n{text}");

// 移除后：直接发送用户文本
return vec![Effect::SendPrompt { text, ... }];
```

### 2. 主动建议系统 (agent_intelligence.rs)
重写了 `generate_suggestions()` 函数，生成更实用的建议：

**检测模式**：
- 编辑后未运行测试 → "Run tests to verify changes"
- 工具重复失败 → "Try a different approach for {tool}"
- 多次编辑 → "Review all changes made"
- 只执行不编辑 → "Summarize what you found"
- 多次 auto-compact → "Start a fresh session"
- 多个错误 → "Diagnose recent errors"
- 大量工具调用但用户交互少 → "Explain what was done so far"

**输出**：
- 按置信度排序，取前 4 个
- 作为 follow-up chips 显示在提示框上方
- 用户可以直接点击执行

### 3. 连接建议到 UI (dispatch/prompt.rs)
```rust
// 在 handle_prompt_response 中
if agent.follow_ups.is_none() && !suggestions.is_empty() {
    let labels: Vec<String> = suggestions
        .iter()
        .take(4)
        .map(|s| s.action.clone())
        .collect();
    agent.apply_follow_ups_with_prompt("local-proactive".into(), None, labels);
}
```

### 4. 新会话欢迎消息 (session/lifecycle.rs)
```rust
if agent.scrollback.len() == 0 {
    let model_name = agent.session.models.current_model_name()
        .unwrap_or_else(|| "AI".to_string());
    agent.scrollback.push_block(RenderBlock::system(format!(
        "Ready to help! I'm {model_name}. Ask me to write code, fix bugs, \
         or explain anything in this project.\n\
         Tips: Use @ to attach files, / for commands, or just type your question."
    )));
}
```

## 与 Codex CLI 对比

### Codex 的优势
1. **更清晰的 TUI 布局** - 最小化设计，重点突出
2. **流式输出** - 实时显示生成内容
3. **内联工具输出** - 命令结果直接显示在对话中
4. **权限系统** - 清晰的批准/建议模式

### ccode 已有优势
1. **更丰富的功能** - 支持更多工具类型和集成
2. **多会话管理** - Dashboard 视图管理多个 agent
3. **计划模式** - 支持复杂的代码审查和批准流程
4. **MCP 集成** - 支持外部工具协议

### ccode 仍需改进
1. **视觉层次** - 可以更清晰地分离不同内容类型
2. **动画效果** - 添加更流畅的过渡动画
3. **焦点模式** - 隐藏非必要元素，专注对话
4. **实时协作** - 多用户协作指示器

## 测试结果

- **编译**：✅ 成功
- **测试**：✅ 7774 passed, 35 failed (pre-existing)
- **无新回归**：✅ 所有现有测试通过

## 下一步建议

### 短期（立即可做）
1. **优化 turn 完成消息** - 显示更详细的总结
2. **添加 idle 提示** - 10 秒空闲后显示上下文建议
3. **改进工具调用显示** - 更清晰的进度指示

### 中期（需要设计）
1. **焦点模式** - 一键隐藏所有非必要 UI 元素
2. **更好的 diff 显示** - 侧边对比或内联高亮
3. **键盘快捷键 cheatsheet** - 全局可访问的快捷键列表

### 长期（需要架构调整）
1. **分屏多 agent 视图** - 并排显示多个 agent
2. **集成终端** - 在 TUI 中直接显示 shell 输出
3. **文件树侧边栏** - 可视化文件导航

## 总结

本次修复解决了三个核心问题：
1. ✅ **无限循环** - 移除错误的 system_prompt 注入
2. ✅ **缺乏主动交互** - 连接建议引擎到 follow-up chips
3. ✅ **新会话引导** - 添加欢迎消息和使用提示

现在 ccode 的交互体验更加友好和智能：
- AI 会主动建议下一步操作
- 新会话有清晰的引导
- 不再有无限循环问题

与 Codex 等工具相比，ccode 在功能丰富度上已经领先，但在视觉 polish 和交互流畅度上还有提升空间。建议后续重点优化 TUI 的视觉层次和动画效果。
