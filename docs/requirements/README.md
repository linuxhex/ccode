# ccode 对标差距补齐需求总览

## 需求来源
基于 ccode vs Claude Code vs Codex CLI 全面对比分析，识别出 P0/P1/P2 三级差距。

## 需求清单

| 优先级 | 需求ID | 需求名称 | 对标竞品 | 状态 |
|--------|--------|----------|----------|------|
| **P0** | P0-GAP-CLOSURE | P0 核心差距补齐 | Claude Code + Codex | ✅ 需求已写 |
| **P1** | P1-EXPERIENCE-BOOST | P1 显著提升体验 | Claude Code + Codex | ✅ 需求已写 |
| **P2** | P2-COMPETITIVENESS | P2 增强竞争力 | Claude Code + Codex | ✅ 需求已写 |

## P0 核心差距补齐（4项）

| # | 差距 | 影响 | 对标 | 需求文档 |
|---|------|------|------|----------|
| 1 | MicroCompact 压缩层 | 长会话 token 浪费严重 | Claude Code 5层压缩 | [P0-GAP-CLOSURE/analysis.md](P0-GAP-CLOSURE/analysis.md) |
| 2 | CCODE.md 层级指令 | 项目知识无法跨会话持久化 | Claude Code CLAUDE.md + Codex AGENTS.md | 同上 |
| 3 | 记忆自动提取管线 | 跨会话知识无法积累 | Claude Code extractMemories + Codex 两阶段管线 | 同上 |
| 4 | MCP Server 模式 | ccode 无法被其他 Agent 编排调用 | Codex MCP Server | 同上 |

## P1 显著提升体验（4项）

| # | 差距 | 影响 | 对标 | 需求文档 |
|---|------|------|------|----------|
| 5 | Context Collapse 增强 | 压缩质量不足，关键信息可能丢失 | Claude Code | [P1-EXPERIENCE-BOOST/analysis.md](P1-EXPERIENCE-BOOST/analysis.md) |
| 6 | ML 分类器 Auto Mode | 无法实现真正自主权限决策 | Claude Code full-auto | 同上 |
| 7 | Hook async+asyncRewake | Hook 不够灵活，无法后台执行和唤醒模型 | Claude Code | 同上 |
| 8 | DesignSync 工具 | 缺少设计系统同步能力 | Claude Code /design-sync | 同上 |

## P2 增强竞争力（3项）

| # | 差距 | 影响 | 对标 | 需求文档 |
|---|------|------|------|----------|
| 9 | 网络代理层 | 无法检查/过滤 Agent 出站流量 | Codex codex-network-proxy | [P2-COMPETITIVENESS/analysis.md](P2-COMPETITIVENESS/analysis.md) |
| 10 | Rollout 重放调试 | 无法事后分析 Agent 决策 | Codex codex-rollout | 同上 |
| 11 | Rewind 会话回退 | 试错后无法安全恢复 | Claude Code /rewind | 同上 |

## 执行顺序

```
P0-GAP-CLOSURE（核心缺失，必须先补齐）
  ├─ 阶段A：MicroCompact 压缩层
  ├─ 阶段B：CCODE.md 层级指令
  ├─ 阶段C：记忆自动提取管线
  └─ 阶段D：MCP Server 模式

P1-EXPERIENCE-BOOST（P0 完成后）
  ├─ 阶段A：Context Collapse 增强
  ├─ 阶段B：ML 分类器 Auto Mode
  ├─ 阶段C：Hook async+asyncRewake
  └─ 阶段D：DesignSync 工具

P2-COMPETITIVENESS（P1 完成后）
  ├─ 阶段A：网络代理层
  ├─ 阶段B：Rollout 重放调试
  └─ 阶段C：Rewind 会话回退
```

## 预期效果

补齐 P0 后：ccode 在体验层达到竞品同等水平，同时保持架构层和多 Provider 的差异化优势
补齐 P1 后：ccode 在权限/压缩/Hook/设计方面超越 Codex，接近 Claude Code
补齐 P2 后：ccode 在安全/调试/容错方面全面超越 Claude Code，与 Codex 持平或领先
