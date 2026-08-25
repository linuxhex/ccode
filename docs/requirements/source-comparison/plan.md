# 源码对比研究 — 改进计划

> 基于 6 款 CLI Coding Agent 的源码对比分析，提炼 Qoder(ccode) 可落地的改进项

---

## Phase A: 快速补强（低成本高收益）

### A1. 副作用声明制权限
- **来源**: deepcode-cli
- **方案**: bash 工具 schema 增加 `sideEffects` 参数，模型声明命令副作用范围（read-in-cwd / write-in-cwd / network 等），权限系统基于声明而非命令解析
- **改动**: `ccode-tools/bash` + `ccore/permission`
- **收益**: 减少权限判断开销，提高 auto-mode 准确率

### A2. Context Fragment 化
- **来源**: Codex
- **方案**: 将系统提示从单一字符串拆为 typed fragments（BaseInstructions / Environment / UserInstructions / Skills / TokenBudget），每个 fragment 可独立注入/移除/压缩
- **改动**: `ccode-agent/prompt` + `ccore/memory`
- **收益**: compaction 时精确控制哪些 fragment 保留、哪些压缩

### A3. Prompt Cache 共享优化
- **来源**: Claude Code
- **方案**: sub-agent fork 时继承父对话的 prompt cache prefix；权限检查复用已缓存的对话前缀
- **改动**: `ccore/sampler` + `ccore/agent/subagent`
- **收益**: 降低 API 成本，减少首 token 延迟

### A4. Agent 环境检测
- **来源**: Harness CLI
- **方案**: 检测 `CLAUDECODE`/`CURSOR_AGENT`/`CODEX_SANDBOX`/`OPENCODE_CLIENT` 等环境变量，识别驱动 Agent 类型，用于遥测和适配输出格式
- **改动**: `ccode-env` + `ccode-telemetry`
- **收益**: 了解用户生态分布，支持 agent-specific 输出优化

---

## Phase B: 中期增强（中等成本）

### B1. 平台沙箱隔离
- **来源**: Codex (seatbelt/bwrap/landlock/Windows)
- **方案**: 实现 `SandboxManager` trait，macOS 用 seatbelt、Linux 用 bwrap/landlock、Windows 用 restricted token
- **改动**: 新增 `ccode-sandbox` crate
- **收益**: 企业级安全隔离，对标 Codex

### B2. Snippet 版本追踪编辑
- **来源**: deepcode-cli
- **方案**: read 工具返回 snippet_id + 文件版本号，edit 工具要求 snippet_id，文件变更时 snippet 过期拒绝
- **改动**: `ccode-tools/read` + `ccode-tools/edit`
- **收益**: 消除过期上下文编辑错误

### B3. Sub-Agent 生命周期完善
- **来源**: Codex V2
- **方案**: 实现完整的 sub-agent 管理：spawn/wait/send/interrupt/list/resume，支持 thread fork 快照
- **改动**: `ccore/agent/subagent` + `ccore/agent/orchestrator`
- **收益**: 多 agent 协作能力对标 Codex

---

## Phase C: 长期演进（高成本战略）

### C1. 插件市场生态
- **来源**: Claude Code (13 官方插件)
- **方案**: 定义 plugin manifest 格式，支持 commands/agents/skills/hooks/mcp 五类组件，建立市场注册表
- **改动**: 新增 `ccode-plugin` crate + marketplace 服务

### C2. exec-server 进程隔离
- **来源**: Codex
- **方案**: 将命令执行移到独立进程，与 agent core 隔离，支持本地和远程执行
- **改动**: 新增 `ccode-exec-server` crate

### C3. 声明式工具定义
- **来源**: Harness CLI (YAML spec)
- **方案**: 支持 YAML 定义工具 schema + handler mapping，降低工具开发门槛
- **改动**: `ccore/tools` 扩展
