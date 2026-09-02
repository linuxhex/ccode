# Phase E1 深度对比：Plugin / Hook / Skill 系统

> 对比对象：Codex (Qoder 实现) / Claude Code / deepcode-cli
> 分析日期：2026-08-27

---

## 一、Hook 系统对比

### 1.1 总览

| 维度 | Qoder (ccode-hooks) | Claude Code | deepcode-cli |
|------|---------------------|-------------|-------------|
| 事件数量 | **14** | **28** | 无 |
| Hook 类型 | 2（Command / HTTP） | **4**（Command / Prompt / HTTP / Agent） | 无 |
| 调度模式 | 串行（config 顺序） | **并行**（非确定性顺序） | 无 |
| asyncRewake | 无 | **有**（exit code 2 唤醒模型） | 无 |
| Hook 重写 | `updatedInput` + `additionalContext` | `updatedInput` + `additionalContext` | 无 |
| 失败策略 | Fail-open | Fail-open | 无 |
| 信任模型 | 文件夹信任 + disabled-hooks 文件 | 文件夹信任 | 无 |
| `if` 条件过滤 | 无（matcher 仅匹配工具名） | **权限规则语法**（`Bash(git *)`) | 无 |
| 客户端 Hook | ACP 反向 RPC | 无 | 无 |

### 1.2 Qoder 独有优势

- **客户端 Hook（ACP 反向 RPC）**：通过 `ccode.dev/hooks/run`（gate）和 `ccode.dev/hooks/event`（observe）实现 UI 层 Hook 参与决策
- **Turn 生命周期 Hook**：Before/After turn hook 可注入 role + content
- **Permission Chain 集成**：Pre-filter → Hook → Rule engine → User confirmation → Default 五阶段决策链

### 1.3 Claude Code 独有特性

| 特性 | 说明 | 价值 |
|------|------|------|
| **asyncRewake** | 后台 Hook 完成后以 exit code 2 唤醒模型 | 安全审查、长任务完成通知 |
| **Prompt Hook** | LLM 驱动的 Hook 决策 | 智能审查，无需编写脚本 |
| **Agent Hook** | 带工具的 Agentic 验证器 | 复杂多步验证 |
| **`if` 条件** | 权限规则语法过滤 | 减少不必要的 Hook 启动 |
| **并行调度** | 所有匹配 Hook 并行执行 | 降低延迟 |
| **Task/Worktree/File 事件** | 任务创建/完成、worktree 创建/删除、文件变更 | 更细粒度生命周期控制 |
| **ConfigChange** | 配置变更通知 | 动态适配 |
| **Elicitation** | 信息引出事件 | 交互式工作流 |

### 1.4 事件覆盖差距

| 事件类别 | Qoder | Claude Code |
|---------|-------|-------------|
| Session 生命周期 | SessionStart, SessionEnd | SessionStart, SessionEnd |
| Turn 生命周期 | Stop, StopFailure | Stop, StopFailure |
| 工具事件 | PreToolUse, PostToolUse, PostToolUseFailure, PermissionDenied | PreToolUse, PostToolUse, PostToolUseFailure, PermissionDenied, **PermissionRequest** |
| 用户事件 | UserPromptSubmit, Notification | UserPromptSubmit, Notification |
| Sub-agent | SubagentStart, SubagentStop | SubagentStart, SubagentStop |
| 压缩 | PreCompact, PostCompact | PreCompact, PostCompact |
| **任务** | 无 | **TaskCreated, TaskCompleted** |
| **Worktree** | 无 | **WorktreeCreate, WorktreeRemove** |
| **配置** | 无 | **ConfigChange** |
| **文件** | 无 | **FileChanged, CwdChanged** |
| **指令** | 无 | **InstructionsLoaded** |
| **引出** | 无 | **Elicitation, ElicitationResult** |
| **设置** | 无 | **Setup** |
| **协作** | 无 | **TeammateIdle** |

---

## 二、Plugin 系统对比

### 2.1 总览

| 维度 | Qoder | Claude Code | deepcode-cli |
|------|-------|-------------|-------------|
| 清单格式 | `plugin.json` / `.ccode-plugin/` / `.claude-plugin/` | `.claude-plugin/plugin.json` | 无 |
| 组件类型 | **6**（Skills/Commands/Agents/Hooks/MCP/LSP） | 5（Skills/Commands/Agents/Hooks/MCP） | 无 |
| 安装源 | CLI/Project/User/Marketplace/ConfigPath | **10+**（git/github/npm/pip/git-subdir/archive/command/url/file/directory） | 无 |
| SHA 固定 | 无 | **SHA-256（git commit）** | 无 |
| 跨工具兼容 | 读 `.claude-plugin/` | 读 `.codex-plugin/` | 无 |
| 插件本地设置 | 无 | **`.claude/<name>.local.md`** | 无 |
| 数据隔离 | `~/.ccode/plugin-data/<id>/` | 无明确隔离 | 无 |
| 热重载 | `Arc<RwLock>` 快照替换 | 需重启 | 无 |
| 官方插件数 | 0 | **13** | 0 |

### 2.2 Qoder 独有优势

- **LSP Server 组件**：插件可声明 LSP 服务器（Claude Code 无此能力）
- **插件数据隔离**：每个插件独立数据目录
- **MCP 所有权追踪**：`mcp_owners` map 防止服务器冲突
- **热重载**：`Arc<RwLock<Option<Arc<PluginRegistry>>>>` 支持不中断会话的重载

### 2.3 Claude Code 独有特性

| 特性 | 说明 | 价值 |
|------|------|------|
| **pip 安装源** | Python 包作为插件 | 扩展生态覆盖面 |
| **archive + SHA-256** | ZIP 下载 + 内容哈希固定 | 安全可复现安装 |
| **command 源** | 本地命令输出插件目录 | 动态插件发现 |
| **git-subdir** | monorepo 子目录稀疏检出 | 大型仓库中的插件 |
| **Marketplace 策略控制** | `blockedMarketplaces` / `allowedMarketplaces` | 企业级策略管理 |
| **forceRemoveDeletedPlugins** | 市场移除的插件自动卸载 | 生命周期自动化 |
| **插件本地设置** | `.claude/<name>.local.md` YAML+Markdown | per-project 可定制 |

### 2.4 官方插件生态分析（Claude Code 13 个）

| 类别 | 数量 | 代表插件 |
|------|------|---------|
| 开发工具 | 4 | agent-sdk-dev, feature-dev, plugin-dev, claude-opus-4-5-migration |
| 生产力 | 3 | commit-commands, code-review, pr-review-toolkit |
| 安全 | 1 | security-guidance（三层审查：模式警告 + LLM diff 审查 + agentic commit 审查） |
| 学习 | 2 | explanatory-output-style, learning-output-style |
| 前端 | 1 | frontend-design |
| 自动化 | 1 | hookify（从对话模式创建 Hook） |
| 循环 | 1 | ralph-wiggum（自引用 AI 循环） |

**关键模式**：
- **security-guidance** 使用 asyncRewake 实现三层异步安全审查
- **hookify** 分析对话历史自动创建 Hook 防止重复错误
- **ralph-wiggum** 用 Stop Hook + `.local.md` 实现迭代循环

---

## 三、Skill 系统对比

### 3.1 总览

| 维度 | Qoder | Claude Code | deepcode-cli |
|------|-------|-------------|-------------|
| 定义格式 | SKILL.md + YAML frontmatter | SKILL.md + YAML frontmatter | 文件系统扫描 |
| 发现机制 | 启动扫描 + **动态中途发现** | 启动扫描 | 5 级扫描 |
| 匹配方式 | 名称 + `paths:` glob + `when-to-use` | **3 级渐进披露** | **LLM 匹配** |
| Scope | 6 级（Local/Repo/User/Server/Bundled/Plugin） | 3 级 | 无 |
| 供应商兼容 | Cursor/Claude denylist | 无 | 无 |
| 模型/effort 覆盖 | 有 | 无 | 无 |
| `enabledSkills` 开关 | 无 | 无 | **有** |
| BM25 工具搜索 | **有**（MCP 工具） | 无 | 无 |
| 动态中途发现 | **有**（文件路径向上遍历） | 无 | 无 |

### 3.2 Claude Code 3 级渐进披露

| 级别 | 内容 | 大小 | 加载时机 |
|------|------|------|---------|
| L1: 元数据 | name + description | ~100 词 | **始终在上下文中** |
| L2: SKILL.md 正文 | 核心指令 | <5k 词 | Skill 触发时 |
| L3: 捆绑资源 | references/, examples/, scripts/, assets/ | 无限* | 按需加载 |

*Scripts 可不读入上下文直接执行。

### 3.3 Qoder 独有优势

- **动态中途发现**：工具操作后从访问路径向上遍历，发现新目录中的 Skill
- **BM25 工具搜索**：MCP 工具通过 BM25 索引匹配，支持标识符分解
- **模型/effort 覆盖**：Skill 可指定使用哪个模型和 effort 级别
- **6 级 Scope**：比 Claude Code 更细的优先级控制

### 3.4 deepcode-cli 独有特性

- **`enabledSkills` per-skill 开关**：用户可在设置中逐个启用/禁用 Skill
- **LLM 匹配**：将 Skill 目录注入 prompt，由模型自主决定使用哪个 Skill

---

## 四、配置系统对比

### 4.1 总览

| 维度 | Qoder | Codex（参考） | Claude Code | deepcode-cli |
|------|-------|-------------|-------------|-------------|
| 格式 | TOML | TOML | JSON | JSON |
| 层数 | **6** | **9** | 4 | 4 |
| 优先级链 | BoolFlag 9 级 | 10 级优先级 | 企业→用户→项目→flag | 默认→用户→项目→env |
| 需求层 | 有（Ed25519 签名） | 有（MDM） | 无 | 无 |
| Campaign 系统 | **有** | 无 | 无 | 无 |
| 版本覆盖 | `[[version_overrides]]` | 无 | 无 | 无 |
| 环境变量展开 | 有 | 有 | 无 | 无 |
| 容忍反序列化 | 有（坏值→None+警告） | 未知 | 未知 | 无 |

### 4.2 Qoder BoolFlag 优先级链

```
Requirement > CLI > Env > Config > Managed > FeatureFlag > Remote > Default
```

每个解析值携带 `ConfigSource` 用于诊断。

---

## 五、综合差距分析

### 5.1 Qoder 领先项

| 项目 | 说明 |
|------|------|
| 客户端 Hook (ACP) | UI 层参与决策，竞品均无 |
| LSP 插件组件 | 插件可提供语言服务器 |
| 插件数据隔离 | 独立数据目录防止冲突 |
| MCP 所有权追踪 | 防止服务器名冲突 |
| 动态 Skill 发现 | 文件路径遍历中途发现新 Skill |
| BM25 工具搜索 | MCP 工具语义搜索 |
| Campaign 系统 | 功能特性灰度发布 |
| 热重载 | Arc<RwLock> 不中断会话 |

### 5.2 Qoder 落后项（按优先级排序）

| # | 差距 | 来源 | 优先级 | 复杂度 |
|---|------|------|--------|--------|
| 1 | **asyncRewake Hook** | Claude Code | **P0** | 中 |
| 2 | **Prompt/Agent Hook 类型** | Claude Code | **P1** | 高 |
| 3 | **缺失 Hook 事件**（Task/Worktree/File/Config） | Claude Code | **P1** | 低 |
| 4 | **`if` 条件过滤** | Claude Code | **P1** | 低 |
| 5 | **插件本地设置 (.local.md)** | Claude Code | **P2** | 低 |
| 6 | **archive + SHA-256 安装** | Claude Code | **P2** | 中 |
| 7 | **并行 Hook 调度** | Claude Code | **P2** | 中 |
| 8 | **`enabledSkills` 开关** | deepcode-cli | **P1** | 低 |
| 9 | **Skill 渐进披露量化** | Claude Code | **P2** | 低 |
| 10 | **Marketplace 策略控制** | Claude Code | **P2** | 中 |

---

## 六、改进方案

### Phase E1 实施项

#### E1.1 asyncRewake Hook（P0）

**来源**：Claude Code security-guidance 插件核心机制

**设计**：
```rust
// HookSpec 新增字段
pub struct HookSpec {
    // ... existing fields ...
    pub async_run: bool,        // 后台执行
    pub async_rewake: bool,     // 完成后 exit code 2 唤醒模型
    pub rewake_message: Option<String>,  // 唤醒时显示的消息
}
```

**调度逻辑**：
1. `async_rewake = true` 的 Hook 在 `tokio::spawn` 中后台执行
2. 完成后检查 exit code：
   - `0`：成功，静默
   - `2`：阻塞错误，注入 `rewake_message` 到下一轮对话
3. 其他 exit code：记录日志

**改动文件**：
- `ccode-hooks/src/config.rs` — HookSpec 新增字段
- `ccode-hooks/src/dispatcher.rs` — async 调度分支
- `ccode-hooks/src/runner/command.rs` — exit code 2 处理
- `ccore/src/node/thinker/mod.rs` — 接收 rewake 消息注入

#### E1.2 新增 Hook 事件（P1）

**新增 6 个事件**：

| 事件 | Gate 类型 | 说明 |
|------|----------|------|
| `TaskCreated` | Observe | 任务创建时触发 |
| `TaskCompleted` | Observe | 任务完成时触发 |
| `FileChanged` | Observe | 文件变更通知 |
| `ConfigChange` | Observe | 配置变更通知 |
| `PermissionRequest` | Gate | 权限请求时触发（可自动批准/拒绝） |
| `CwdChanged` | Observe | 工作目录变更 |

**改动文件**：
- `ccode-hooks/src/event.rs` — HookEventName 新增变体
- `ccode-hooks/src/dispatcher.rs` — 新事件调度入口
- `ccode-shell/src/session/acp_session_impl/hook_dispatch.rs` — 触发点

#### E1.3 `if` 条件过滤（P1）

**设计**：复用现有权限规则语法

```rust
// HookSpec 新增
pub struct HookSpec {
    // ... existing fields ...
    pub condition: Option<String>,  // e.g. "Bash(git *)", "Read(*.ts)"
}
```

**调度优化**：在构建 Hook 候选列表时，先评估 `condition`，不匹配则跳过，避免不必要的进程启动。

**改动文件**：
- `ccode-hooks/src/config.rs` — HookSpec 新增 condition
- `ccode-hooks/src/matcher.rs` — condition 评估逻辑
- `ccode-hooks/src/dispatcher.rs` — 预过滤

#### E1.4 `enabledSkills` 开关（P1）

**设计**：

```toml
# ~/.ccode/config.toml
[skills]
enabled = ["deploy", "code-review"]
disabled = ["experimental-feature"]
```

**改动文件**：
- `ccode-config-types/src/lib.rs` — 新增 skills 配置节
- `ccode-tools/src/implementations/skills/discovery.rs` — 过滤逻辑

#### E1.5 插件本地设置（P2）

**设计**：

```markdown
<!-- .ccode/my-plugin.local.md -->
---
enabled: true
max_retries: 3
---

# Additional Context
Project-specific instructions for my-plugin...
```

**改动文件**：
- `ccode-agent/src/plugins/` — 新增 `local_settings.rs`
- 插件可通过 `$CCODE_PLUGIN_ROOT/../<name>.local.md` 访问

#### E1.6 并行 Hook 调度（P2）

**设计**：observe 类事件并行执行，gate 类事件保持串行（需要确定性决策链）。

```rust
// dispatcher.rs
pub async fn dispatch_non_blocking(...) {
    let futures: Vec<_> = eligible_hooks.iter()
        .map(|spec| run_hook(spec, &envelope))
        .collect();
    let results = futures::future::join_all(futures).await;
    // 聚合结果，仅记录日志
}
```

---

## 七、实施优先级

| 序号 | 项目 | 复杂度 | 收益 | 依赖 |
|------|------|--------|------|------|
| E1.1 | asyncRewake Hook | 中 | **高** | 无 |
| E1.2 | 新增 Hook 事件 | 低 | 中 | 无 |
| E1.3 | `if` 条件过滤 | 低 | 中 | 无 |
| E1.4 | `enabledSkills` 开关 | 低 | 中 | 无 |
| E1.5 | 插件本地设置 | 低 | 低 | 无 |
| E1.6 | 并行 Hook 调度 | 中 | 低 | E1.2 |

**建议顺序**：E1.1 → E1.2 + E1.3（并行） → E1.4 → E1.5 + E1.6
