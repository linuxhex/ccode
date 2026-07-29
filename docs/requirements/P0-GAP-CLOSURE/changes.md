# P0 核心差距补齐改动简述

## 改动概述
补齐 ccode 对标 Claude Code 和 Codex CLI 的 4 项 P0 核心差距：MicroCompact 压缩层、CCODE.md 层级指令、记忆自动提取管线、MCP Server 模式。

## 新增文件

| 文件 | 职责 |
|------|------|
| crates/common/ccode-compaction/src/micro_compact.rs | MicroCompact 按时间清除旧工具结果 |
| crates/common/ccode-compaction/src/budget.rs | Budget Reduction 工具输出 token 预算 |
| crates/common/ccode-compaction/src/snip.rs | Snip 超长输出截断 |
| crates/common/ccode-compaction/src/context_collapse.rs | Context Collapse 大范围摘要压缩 |
| crates/common/ccode-compaction/src/cache_aware.rs | Cache-Aware 压缩 + cache_edits |
| crates/common/ccode-compaction/src/compactable.rs | COMPACTABLE_TOOLS 白名单定义 |
| crates/codegen/ccode-config/src/claude_md.rs | CCODE.md 层级收集 + @import 展开 |
| crates/codegen/ccode-memory/src/inject.rs | 冷区→热区动态注入 |
| crates/codegen/ccode-mcp/src/server.rs | MCP Server 逻辑 |
| crates/codegen/ccode-mcp/src/server_transport.rs | stdio/SSE 传输层 |
| crates/codegen/ccode-mcp/src/server_tools.rs | MCP Server 暴露的工具定义 |

## 修改文件

| 文件 | 改动 |
|------|------|
| crates/common/ccode-compaction/src/lib.rs | 导出新增模块 |
| crates/codegen/ccode-config/src/lib.rs | 导出 claude_md 模块 |
| crates/codegen/ccode-memory/src/auto_extract.rs | 完善自动记忆提取逻辑 |
| crates/codegen/ccode-memory/src/consolidation.rs | 完善后台定期整合逻辑 |
| crates/codegen/ccode-memory/src/lib.rs | 导出 inject 模块 |
| crates/codegen/ccode-mcp/src/lib.rs | 导出 server 模块 |
| crates/codegen/ccode-cli/src/main.rs | 新增 --mcp-server 参数 |

## 方案审查

### 业务逻辑推演
- 业务流程推演：✓ 4 个阶段独立闭环，可独立交付验证
- 业务规则推演：✓ MicroCompact 白名单/时间阈值/保留策略完整
- 业务状态推演：✓ CCODE.md 层级合并顺序正确，@import 循环检测有深度限制
- 业务数据推演：✓ 记忆提取→整合→注入管线数据流完整
- 业务异常推演：✗ MCP Server 连接断开后客户端无重试（需补充）
- 业务边界推演：✓ COMPACTABLE_TOOLS 覆盖主要工具，边界清晰
- 业务依赖关系：✓ P0 内部无跨阶段依赖，可并行
- 业务异常恢复：✓ dream_lock 防多实例冲突，MCP Server 断连可重启

### 技术方案审查
- 文件路径正确：✓
- 依赖关系合理：✓ rmcp 已有 Server 端支持
- 技术方案可行：✓
- 接口契约一致：✓
- 配置项完整：✓

### 执行可行性审查
- 步骤无遗漏：✓ 10 个任务
- 步骤无冲突：✓
- 资源可获取：✓
- 环境可支持：✓

### 审查结论
- 发现问题：1 个
  - [minor] MCP Server 连接断开后客户端无重试策略 → 补充自动重连逻辑
