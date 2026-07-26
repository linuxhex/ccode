# Agent 全面增强 - 改动记录

## 新增文件（12 个）

| 文件 | 职责 |
|------|------|
| `crates/codegen/ccode-hooks/src/permission_rules.rs` | 权限规则引擎：allow/deny/ask 规则 + 通配符匹配 + JSON 配置加载 |
| `crates/codegen/ccode-tools/src/implementations/ccode_build/github_search/mod.rs` | GitHub 代码搜索工具：API 搜索 + 无 token 降级 + 结果筛选 + 推荐摘要 |
| `crates/codegen/ccode-tools/src/implementations/ccode_build/deps_search/mod.rs` | 依赖搜索工具：crates.io/npm/PyPI 搜索 + 适配性分析 + 推荐摘要 |
| `crates/codegen/ccode-tools/src/implementations/ccode_build/context_layer/mod.rs` | 上下文冷热分层管理器：滑动窗口 + 语义价值分流 + 错误纠正标注 |
| `crates/codegen/ccode-tools/src/implementations/ccode_build/context_layer/types.rs` | 上下文类型定义：ContextZone/SemanticValue/CorrectedKnowledge/ContextMessage |
| `crates/codegen/ccode-tools/src/implementations/ccode_build/agent_whitelist.rs` | 子代理工具白名单：4 级代理类型 + 工具过滤 + 递归禁止 |
| `crates/common/ccode-compaction/src/micro_compact.rs` | 微压缩层：COMPACTABLE_TOOLS 白名单 + 按时间清除 + MicroCompactable trait |
| `crates/codegen/ccode-shell/src/session/rewind.rs` | 会话持久化：JSONL 存储 + UUID 标识 + rewind 截断 + resume 加载 |
| `crates/codegen/ccode-memory/src/auto_extract.rs` | 自动记忆提取：关键词检测 + KnowledgeKind 分类 + extract_knowledge |
| `crates/codegen/ccode-memory/src/consolidation.rs` | 记忆整合：PID 文件锁 + 跨会话去重 + consolidate_memories |
| `docs/requirements/agent-enhancement/plan.md` | 实现计划文档 |
| `docs/requirements/agent-enhancement/changes.md` | 本文件 |

## 修改文件（5 个）

| 文件 | 改动 |
|------|------|
| `crates/codegen/ccode-hooks/src/lib.rs` | 添加 `pub mod permission_rules;` |
| `crates/codegen/ccode-tools/src/implementations/ccode_build/mod.rs` | 添加 `pub mod github_search;`、`pub mod deps_search;`、`pub mod context_layer;`、`pub mod agent_whitelist;` |
| `crates/common/ccode-compaction/src/lib.rs` | 添加 `pub mod micro_compact;` |
| `crates/codegen/ccode-shell/src/session/mod.rs` | 添加 `pub mod rewind;` |
| `crates/codegen/ccode-memory/src/lib.rs` | 添加 `pub mod auto_extract;`、`pub mod consolidation;` |

## 推演收敛

- 轮次：1
- 发现问题：0 个
- 各模块逻辑经推演验证可靠：
  - permission_rules: deny 优先两遍扫描 + glob 通配符匹配 + 默认安全基线
  - github_search: 双模式搜索（API + 降级 WebSearch）+ stars 排序
  - deps_search: 三 registry 支持 + 下载量排序 + 适配性分析
  - context_layer: 滑动窗口溢出分流 + 错误纠正标注 + 冷区按需注入
  - micro_compact: 白名单 + 按时间清除 + MicroCompactable trait
  - rewind: JSONL 存储 + UUID 截断 + 压缩边界保留
  - auto_extract: 关键词提取 + 5 种知识类型
  - consolidation: PID 锁竞态保护 + 去重整合
  - agent_whitelist: 4 级代理类型 + 递归禁止 + 工具过滤

## 编译/构建检查

- 因工作区预存问题（ccode-shell-session-support 依赖缺失），`cargo check` 无法整体通过
- 各模块 rust-analyzer 诊断零错误
- 所有新增代码仅使用已有依赖，无新增外部 crate（除 ccode-memory 的 libc unix 条件依赖）
