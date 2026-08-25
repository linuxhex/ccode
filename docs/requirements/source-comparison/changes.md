# 变更清单

## 文档产出

| 文件 | 类型 | 说明 |
|------|------|------|
| `docs/requirements/source-comparison/analysis.md` | 新增 | 6 款工具全维度对比分析 |
| `docs/requirements/source-comparison/plan.md` | 新增 | 基于对比的改进计划（3 Phase） |
| `docs/requirements/source-comparison/changes.md` | 新增 | 本文件 |
| `docs/requirements/source-comparison/workflow-state.json` | 新增 | 工作流状态 |

## 源码获取记录

| 工具 | 路径 | 来源 |
|------|------|------|
| Claude Code | `ref-agents/claude-code` | 已有（公开仓库，闭源，仅文档+CHANGELOG） |
| Codex | `ref-agents/codex` | 已有（github.com/openai/codex, Apache 2.0） |
| OpenCode | `ref-agents/opencode` | 已有（github.com/anomalyco/opencode） |
| deepcode-cli | `ref-agents/deepcode-cli` | 新克隆（github.com/lessweb/deepcode-cli） |
| Qoder(ccode) | 当前工程 | 本地 |
| Harness CLI | `ref-agents/harness-cli` | 新克隆（github.com/harness/cli, Apache 2.0） |

## 无代码改动

本次为纯研究分析，未修改 ccode 源码。改进计划见 plan.md。
