# 需求分析（ccode 视角）

## 需求概述
> 对标 Codex CLI / Claude Code / OpenCode / DeepSeek Harness，优化 ccode agent 的 token 效率

## 业务背景
- ccode 每轮 LLM 请求全量注入 22 个工具 schema，无延迟加载机制
- 工具输出无预算限制，bash 等工具可能返回数万字符直接灌入上下文
- 子 agent 完成后将完整输出注入父上下文，无摘要压缩
- 稳定前缀机制已定义但从未调用（`lock_stable_prefix` 未使用），`cache_control` 断点已设置但 provider 序列化时丢弃

## 本服务职责
| 职责项 | 说明 |
|--------|------|
| 工具 schema 管理 | 控制注入 LLM 的工具定义体积 |
| 工具输出控制 | 限制单次工具输出占用的 token 预算 |
| 子 agent 输出压缩 | 摘要返回，保护父上下文 |
| 缓存前缀稳定 | 确保跨轮次前缀字节不变，提升缓存命中 |

## 改动范围
- [ ] 改进 1：工具延迟加载（P0）
- [ ] 改进 2：工具输出预算控制（P0）
- [ ] 改进 3：子 Agent 摘要返回（P1）
- [ ] 改进 4：稳定前缀强化 + cache_control 序列化（P1）

## 风险与注意
- 工具延迟加载需确保模型能通过 `tool_search` 获取完整 schema，否则可能选错工具
- 工具输出截断需保留头尾 + 提示，避免模型丢失关键信息
- 子 agent 摘要需保留关键结论和文件路径
- cache_control 序列化需适配不同 provider（Anthropic 支持 tools 上的 cache_control，OpenAI 不支持）
