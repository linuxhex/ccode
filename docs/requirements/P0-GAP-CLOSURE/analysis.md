# 需求分析（P0 核心差距补齐）

## 需求概述
> 补齐 ccode 对标 Claude Code 和 Codex CLI 的 4 项 P0 核心差距：MicroCompact 压缩层、CCODE.md 层级指令、记忆自动提取管线、MCP Server 模式。补齐后 ccode 在体验层达到竞品同等水平，同时保持架构层和多 Provider 的差异化优势。

## 业务背景
- ccode 架构层（微内核+消息总线+自愈+可观测性）领先竞品一个代际，但体验层落后半个代际
- 长会话场景下 token 浪费严重（缺少 MicroCompact），项目知识无法跨会话持久化（缺少 CCODE.md），跨会话知识无法积累（缺少自动提取），ccode 无法被其他 Agent 编排调用（缺少 MCP Server）
- Claude Code 有 5 层压缩管道（Budget→Snip→MicroCompact→Collapse→Auto），ccode 只有 3 层（Code/Intra/Inter）
- Claude Code 有 CLAUDE.md 层级指令 + @import，Codex 有 AGENTS.md 层级收集，ccode 两者均无
- Claude Code 有 extractMemories + autoDream 自动记忆管线，Codex 有两阶段 AI 记忆管线（Rollout 提取→全局合并），ccode 有 Dream 整合但缺少自动提取
- Codex 自身可作为 MCP 服务器运行，ccode 仅支持 MCP 客户端

## 本项目职责

| 职责项 | 说明 |
|--------|------|
| MicroCompact 压缩层 | 按时间清除旧工具结果，保留语义；Cache-Aware 压缩利用 prompt cache |
| CCODE.md 层级指令 | 层级化指令文件 + @import 引用 + 自动记忆写入 |
| 记忆自动提取管线 | 会话结束自动提取关键知识 + 后台定期整合 + 冷区→热区动态注入 |
| MCP Server 模式 | ccode 自身可作为 MCP 服务器，暴露工具给外部 Agent 调用 |

## 详细设计

### 1. MicroCompact 压缩层

**对标**：Claude Code 的 `services/compact/microCompact.ts`

**五层压缩管道**（当前 3 层 → 目标 5 层）：

```
现有：Code Compaction → Intra Compaction → Inter Compaction
目标：Budget Reduction → Snip → MicroCompact → Context Collapse → Auto-Compact
```

**MicroCompact 核心逻辑**：
- `COMPACTABLE_TOOLS` 白名单：FileRead、Bash、Grep、Glob、WebSearch、WebFetch、FileEdit、FileWrite
- 按时间清除旧工具结果（`TIME_BASED_MC_CLEARED_MESSAGE`）：超过 N 轮的工具调用结果替换为摘要占位符
- 保留最近 K 轮的完整结果（热区）
- Cache-Aware 压缩（`cachedMicrocompact`）：已缓存的系统提示+工具定义不重复压缩
- `cache_edits` 机制：压缩结果通过 `cache_edits` 参数注入 API 请求，利用 Anthropic prompt cache 节省 token

**Budget Reduction**：
- 压缩工具输出的 token 预算：每个工具输出有独立预算上限
- 超预算的输出截断并标注 `[truncated, N tokens saved]`

**Snip**：
- 截断超长输出：FileRead（>2000行截断）、Bash（>10000字符截断）、Grep（>500匹配截断）
- 截断位置保留首尾，中间用 `[... N lines/matches omitted ...]` 标注

**Context Collapse**：
- 大范围摘要压缩：当 MicroCompact 后仍超 token 预算，对整个对话历史做 LLM 摘要
- 摘要保留：关键决策、错误纠正、约束条件、已排除方案
- 摘要丢弃：闲聊、试错过程、重复内容

**Auto-Compact**：
- Token 接近上限时（>85% context window）自动触发全量压缩
- 压缩前执行 PreCompact 钩子，压缩后执行 PostCompact 钩子

### 2. CCODE.md 层级指令

**对标**：Claude Code 的 CLAUDE.md + Codex 的 AGENTS.md

**层级结构**：
```
~/.ccode/CLAUDE.md                    — 全局用户指令（所有项目）
<project-root>/CLAUDE.md              — 项目根指令（提交到仓库，团队共享）
<project-root>/.ccode/CLAUDE.md       — 项目本地指令（不提交，个人偏好）
<subdir>/CLAUDE.md                    — 子目录指令（该目录及子目录生效）
```

**收集算法**：
1. 从项目根到当前工作目录，收集所有 CLAUDE.md 文件
2. 加载全局 ~/.ccode/CLAUDE.md
3. 按层级合并：全局 → 项目根 → .ccode → 子目录（后者覆盖前者）
4. 解析 @import 引用：`@filename.md` 语法自动展开为文件内容

**@import 语法**：
```markdown
# 项目指令
@RTK.md          — 引用同目录下的 RTK.md 文件
@docs/api.md     — 引用相对路径文件
```

**自动记忆写入**：
- Agent 在会话中学习到的项目知识（架构决策、约束条件、常见错误修复）
- 用户可通过 `/memory` 命令手动写入
- 写入位置：`~/.ccode/projects/<project-hash>/memory/` 目录
- 格式：Markdown 文件，带 frontmatter（name, description, metadata.type）

**与现有 ccode-config 集成**：
- CCODE.md 内容注入系统提示的 `project_instructions` 段
- 每次会话启动时重新加载（支持热更新）
- 文件变更监听（复用 ccode-fsnotify）

### 3. 记忆自动提取管线

**对标**：Claude Code 的 extractMemories + autoDream，Codex 的两阶段记忆管线

**Phase 1：会话结束提取**：
- 触发时机：会话结束（SessionEnd 钩子）或用户 `/dream` 命令
- 提取方式：fork 当前会话作为子代理，发送提取 prompt
- 提取内容：关键决策、约束条件、已排除方案、错误纠正、用户偏好、项目架构发现
- 产出：`raw_memory` Markdown 文件，写入 `~/.ccode/projects/<hash>/memory/`

**Phase 2：后台定期整合**：
- 触发时机：空闲时自动触发（类似 Dream 整理），或用户 `/dream` 命令
- 整合方式：
  1. 扫描所有 `raw_memory` 文件
  2. 去重：语义相似度 > 阈值的记忆合并
  3. 提炼：多条相关记忆合并为更精炼的摘要
  4. 写入整合后的 `MEMORY.md`
- 分布式锁：基于 PID 文件的 consolidation lock（复用现有 dream_lock），防止多实例同时整合
- 互斥检测：如果主 Agent 已写了记忆文件，提取跳过（避免重复）

**冷区→热区动态注入**：
- 当 Agent 遇到与冷区记忆相关的问题时，自动从长期记忆检索
- 检索方式：BM25 + embedding 混合检索（复用 ccode-memory 的 search 模块）
- 注入位置：系统提示的 `recalled_memory` 段
- 用完即回收：注入的记忆在下一轮不自动保留，除非再次检索命中

**与现有 ccode-memory 集成**：
- 复用 embedding 模块（embed_missing_chunks）
- 复用 search 模块（BM25 + MMR）
- 复用 dream 模块（Dream 整合逻辑）
- 复用 dream_lock 模块（跨进程去重）
- 新增 auto_extract 模块（会话结束提取）
- 新增 consolidation 模块（后台定期整合）

### 4. MCP Server 模式

**对标**：Codex 的 MCP Server 能力

**架构**：
```
ccode 作为 MCP Server：
  stdio transport — 标准输入/输出通信
  SSE transport   — HTTP SSE 通信（远程调用）

暴露的工具：
  ccode_read       — 读取文件
  ccode_write      — 写入文件
  ccode_edit       — 编辑文件
  ccode_search     — 搜索代码（Grep/Glob）
  ccode_bash       — 执行命令
  ccode_memory     — 检索记忆
  ccode_code_graph — 代码图谱查询（跳转/引用/符号搜索）
```

**启动方式**：
- CLI 参数：`ccode --mcp-server` 或 `ccode mcp serve`
- 配置：在 `~/.ccode/config.toml` 中配置 MCP Server 模式
- Transport 选择：stdio（默认，本地调用）/ SSE（远程调用，需指定端口）

**安全控制**：
- MCP Server 模式下默认使用最严格沙箱（read-only 或 workspace-write）
- 工具白名单：只暴露安全工具，不暴露 Bash（除非显式配置允许）
- 权限规则：复用现有 permission_rules 引擎
- 连接认证：API Key 或 OAuth

**与现有 ccode-mcp 集成**：
- 复用 rmcp 库的 Server 端实现
- 复用 wire 协议类型
- 新增 server 模块（MCP Server 逻辑）
- 新增 transport 模块（stdio/SSE 传输层）

## 复用现有模块

| 现有模块 | 复用方式 |
|----------|----------|
| ccode-compaction | 新增 micro_compact + context_collapse 模块，复用现有 code/intra/inter 压缩 |
| ccode-memory | 复用 embedding/search/dream/dream_lock，新增 auto_extract/consolidation |
| ccode-mcp | 复用 rmcp/wire，新增 server 模块 |
| ccode-config | 扩展配置支持 CCODE.md 加载和 MCP Server 配置 |
| ccode-fsnotify | 复用文件变更监听，用于 CCODE.md 热更新 |
| ccode-hooks | 复用 PreCompact/PostCompact/SessionEnd 钩子 |
| ccode-sandbox | MCP Server 模式下复用沙箱 |
| ccode-graph | MCP Server 暴露代码图谱工具 |
| ccode-secrets | MCP Server 模式下密钥脱敏 |

## 技术选型

| 领域 | 选型 | 理由 |
|------|------|------|
| 压缩策略 | 按时间清除 + Cache-Aware | Claude Code 验证过的方案 |
| 指令格式 | Markdown + frontmatter | 与 CLAUDE.md/AGENTS.md 生态兼容 |
| 记忆提取 | fork 子代理 + LLM 提取 | Claude Code 验证过的方案 |
| 记忆整合 | BM25 + embedding 混合检索 | ccode-memory 已有实现 |
| MCP Server | rmcp Server 端 | ccode-mcp 已用 rmcp 客户端，Server 端同库 |
| 分布式锁 | PID 文件锁 | 复用 dream_lock |

## 风险与注意

- ⚠️ MicroCompact 的 COMPACTABLE_TOOLS 白名单需与实际工具名对齐，遗漏会导致压缩不生效
- ⚠️ CCODE.md @import 需防止循环引用（A→B→A），需引用深度限制
- ⚠️ 记忆提取 fork 子代理有额外 token 成本，需控制提取频率和 prompt 大小
- ⚠️ MCP Server 模式下安全边界需严格测试，避免通过 MCP 调用绕过沙箱
- 💡 建议分 4 个独立任务交付，每个任务可独立验证
- 💡 MicroCompact 和 Context Collapse 可渐进式上线：先 MicroCompact，验证效果后再加 Collapse
