# 需求分析（P1 显著提升体验）

## 需求概述
> 补齐 ccode 对标 Claude Code 和 Codex CLI 的 4 项 P1 差距：Context Collapse 增强、ML 分类器 Auto Mode、Hook async+asyncRewake、DesignSync 工具。显著提升用户体验，缩小与竞品的体验差距。

## 业务背景
- P0 补齐后，ccode 在压缩/记忆/MCP 方面达到竞品水平，但权限决策、Hook 灵活度、设计能力仍有差距
- Claude Code 有 7 种权限模式 + ML 分类器 Auto Mode，ccode 只有 3 种 + 规则引擎，缺少真正的自主权限决策
- Claude Code 的 Hook 支持 async+asyncRewake（后台执行+错误时唤醒模型），ccode 的 Hook 只支持同步执行
- Claude Code 有 DesignSync 工具（设计系统同步到 claude.ai/design），ccode 缺少设计系统相关能力
- Context Collapse 在 P0 已实现基础版，P1 需增强为生产级（摘要质量、保留策略、增量压缩）

## 本项目职责

| 职责项 | 说明 |
|--------|------|
| Context Collapse 增强 | 生产级摘要压缩：保留策略优化、增量压缩、摘要质量评估 |
| ML 分类器 Auto Mode | 基于历史审批数据训练轻量分类器，自动决策权限审批 |
| Hook async+asyncRewake | Hook 后台异步执行 + 错误时唤醒模型重试 |
| DesignSync 工具 | 设计系统同步：组件预览→claude.ai/design 推送 |

## 详细设计

### 1. Context Collapse 增强

**对标**：Claude Code 的 Context Collapse + MicroCompact 联动

**增强点**：
- **保留策略优化**：
  - 关键决策（用户确认/否决的选择）→ 始终保留
  - 错误纠正（用户纠正 Agent 的内容）→ 标注为「反向知识」始终保留
  - 约束条件（用户明确指定的限制）→ 始终保留
  - 已排除方案（用户否决的方案）→ 保留以防止重复
  - 闲聊/试错/重复 → 可安全丢弃
- **增量压缩**：
  - 非全量重压缩，只压缩新增的轮次
  - 已压缩的摘要不再重复压缩
  - 压缩结果带版本号，支持回滚到上一版本
- **摘要质量评估**：
  - 压缩后对比关键信息保留率（决策/约束/纠正是否都在摘要中）
  - 保留率 < 90% 时告警，触发重新压缩
  - 摘要 token 预算：不超过原始 token 的 20%

### 2. ML 分类器 Auto Mode

**对标**：Claude Code 的 full-auto + ML 分类器

**架构**：
```
权限决策流程（增强版）：
  1. pre-filtering（工具级快速拒绝：已知危险命令直接 deny）
  2. PreToolUse hook（用户自定义拦截）
  3. Rule evaluation（.ccode/rules/ 路径规则 + allow/deny/ask 列表）
  4. ML classifier（Auto Mode 下：基于历史数据预测 allow/deny）
  5. Permission handler（ML 无信心时弹出确认对话框）
  6. PostToolUse hook（审计+反馈+记录审批结果供 ML 学习）
```

**ML 分类器设计**：
- **特征**：tool_name, command_pattern, file_path_pattern, project_type, user_history
- **模型**：轻量决策树或逻辑回归（不需要 GPU，本地推理 <1ms）
- **训练数据**：用户历史审批记录（`~/.ccode/permission_history.jsonl`）
- **信心阈值**：
  - 信心 > 0.95 → 自动 allow
  - 信心 < 0.05 → 自动 deny
  - 0.05 ≤ 信心 ≤ 0.95 → 弹出确认（ask）
- **冷启动**：无历史数据时退化为规则引擎（现有 permission_rules）
- **在线学习**：每次用户审批后更新模型参数

**新增权限模式**：
- `auto-edit`：编辑自动允许，执行需确认（同 Claude Code）
- `full-auto`：ML 分类器自动决策（同 Claude Code）
- `plan`：只读规划，不执行（同 Claude Code）

### 3. Hook async+asyncRewake

**对标**：Claude Code 的 async + asyncRewake Hook 特性

**async Hook**：
- Hook 在后台异步执行，不阻塞 Agent 主循环
- 适用于：审计日志、通知发送、指标收集等不需要即时结果的场景
- 配置：`"async": true` 在 Hook 定义中

**asyncRewake**：
- async Hook 执行失败（exit code != 0）时，唤醒模型重新处理
- 唤醒方式：将 Hook 的错误输出作为 `additional_context` 注入模型下一轮
- 适用于：代码格式检查（Hook 失败→模型看到格式错误→自动修复）
- 配置：`"async": true, "asyncRewake": true`

**实现要点**：
- `HookRunner` 新增 `run_async` 方法
- async Hook 的结果通过 channel 发送到主循环
- asyncRewake 触发时，将错误信息注入 `interjection`（复用 ccode-interjection）
- Hook 超时：async Hook 有独立超时（默认 30s），超时不唤醒模型

### 4. DesignSync 工具

**对标**：Claude Code 的 DesignSync（/design-sync skill）

**能力**：
- 将本地组件库（React/Vue/Svelte）同步到 claude.ai/design 设计系统
- 支持组件预览卡片的注册和更新
- 支持设计 token（颜色/间距/字体）的同步
- 支持预览 HTML 文件的推送

**实现要点**：
- `DesignSyncClient`：与 claude.ai/design API 通信
- `ComponentScanner`：扫描本地组件目录，提取组件元数据
- `PreviewGenerator`：为每个组件生成预览 HTML
- `TokenExtractor`：从 CSS/SCSS/Tailwind 配置提取设计 token
- 工具暴露：`design_sync` 工具，Agent 可调用同步设计系统

**注意**：DesignSync 需要 claude.ai 账户认证，非 Claude 用户可跳过。

## 复用现有模块

| 现有模块 | 复用方式 |
|----------|----------|
| ccode-compaction | 增强 context_collapse 模块 |
| ccode-hooks | 扩展 HookRunner 支持 async，扩展事件类型 |
| ccode-interjection | asyncRewake 唤醒时复用消息注入 |
| ccode-config | 新增权限模式配置 |
| ccode-sandbox | Auto Mode 下沙箱策略调整 |
| ccode-http | DesignSync API 通信 |

## 技术选型

| 领域 | 选型 | 理由 |
|------|------|------|
| ML 分类器 | linfa (Rust ML 库) | 纯 Rust，轻量，支持决策树/逻辑回归 |
| Hook async | tokio::spawn + channel | 复用现有异步运行时 |
| DesignSync | HTTP API + claude.ai OAuth | 对标 Claude Code |
| 摘要质量 | LLM 自评估 | 压缩后让 LLM 检查关键信息保留率 |

## 风险与注意

- ⚠️ ML 分类器冷启动阶段可能频繁弹出确认，需提供"学习模式"引导用户快速积累审批数据
- ⚠️ async Hook 的错误处理需谨慎，避免无限唤醒循环（asyncRewake 最大重试 3 次）
- ⚠️ DesignSync 依赖 claude.ai 外部服务，网络不可用时需优雅降级
- ⚠️ Context Collapse 增量压缩需维护压缩版本状态，Agent 崩溃后需能恢复
- 💡 ML 分类器可渐进上线：先收集审批数据不决策，数据充足后再启用自动决策
