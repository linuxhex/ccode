# P2 增强竞争力改动简述

## 改动概述
补齐 3 项 P2 差距：网络代理层、Rollout 重放调试、Rewind 会话回退。

## 新增文件

| 文件 | 职责 |
|------|------|
| crates/codegen/ccode-http/src/proxy.rs | HTTP/SOCKS5 代理服务器 |
| crates/codegen/ccode-http/src/proxy_rules.rs | 域名规则检查 |
| crates/codegen/ccode-http/src/proxy_audit.rs | 审计日志 |
| crates/codegen/ccode-http/src/mitm.rs | MITM TLS 拦截 |
| crates/codegen/ccode-shell/src/rollout.rs | Rollout 录制器 |
| crates/codegen/ccode-shell/src/rollout_player.rs | Rollout 重放播放器 |
| crates/codegen/ccode-shell/src/rollout_analyzer.rs | Rollout 分析器 |
| crates/codegen/ccode-shell/src/rollout_bundle.rs | Bundle 打包/解包 |
| crates/codegen/ccode-chat/src/rewind.rs | Rewind 回退管理器 |
| crates/codegen/ccode-chat/src/file_undo.rs | 文件修改撤销记录 |
| crates/codegen/ccode-pager/src/rewind_cmd.rs | /rewind 命令处理 |
| crates/codegen/ccode-pager/src/replay_cmd.rs | /replay 命令处理 |

## 修改文件

| 文件 | 改动 |
|------|------|
| crates/codegen/ccode-http/src/lib.rs | 导出代理模块 |
| crates/codegen/ccode-shell/src/lib.rs | 导出 rollout 模块 |
| crates/codegen/ccode-chat/src/lib.rs | 导出 rewind 模块，ConversationItem 新增 uuid 字段 |

## 方案审查

### 业务逻辑推演
- 业务流程推演：✓ 3 个阶段独立闭环
- 业务规则推演：✓ 代理域名规则/审计/回退逻辑完整
- 业务状态推演：✓ Rollout 录制/重放状态机正确
- 业务数据推演：✓ Rewind 消息截断+文件撤销数据流完整
- 业务异常推演：✗ MITM CA 证书信任失败时代理不可用（需优雅降级）
- 业务边界推演：✓ Bash 副作用标注为不可撤销
- 业务依赖关系：✓ 依赖 P0 的 compaction 和 P1 的 Hook async
- 业务异常恢复：✓ /rewind-undo 可恢复回退

### 技术方案审查
- 文件路径正确：✓
- 依赖关系合理：✓ hudsucker 为 Rust HTTP 代理库
- 技术方案可行：✓
- 接口契约一致：✓
- 配置项完整：✓

### 执行可行性审查
- 步骤无遗漏：✓ 9 个任务
- 步骤无冲突：✓
- 资源可获取：✓
- 环境可支持：✓

### 审查结论
- 发现问题：1 个
  - [minor] MITM CA 证书信任失败时需优雅降级为非 MITM 模式
