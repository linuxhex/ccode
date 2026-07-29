# P1 显著提升体验改动简述

## 改动概述
补齐 4 项 P1 差距：Context Collapse 增强、ML 分类器 Auto Mode、Hook async+asyncRewake、DesignSync 工具。

## 新增文件

| 文件 | 职责 |
|------|------|
| crates/common/ccode-compaction/src/collapse_version.rs | 压缩版本状态管理 |
| crates/codegen/ccode-hooks/src/permission_classifier.rs | ML 分类器 Auto Mode |
| crates/codegen/ccode-hooks/src/permission_history.rs | 审批历史记录 |
| crates/codegen/ccode-hooks/src/runner/async_runner.rs | async Hook 执行器 |
| crates/codegen/ccode-shell/src/permission_mode.rs | 新增 auto-edit/full-auto/plan 模式 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/mod.rs | DesignSync 工具 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/scanner.rs | 组件扫描器 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/tokens.rs | 设计 token 提取 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/client.rs | claude.ai API 客户端 |

## 修改文件

| 文件 | 改动 |
|------|------|
| crates/common/ccode-compaction/src/context_collapse.rs | 增强保留策略+增量压缩+质量评估 |
| crates/codegen/ccode-hooks/src/runner/mod.rs | 导出 async_runner |
| crates/codegen/ccode-hooks/src/config.rs | 支持 async/asyncRewake 配置 |

## 方案审查

### 业务逻辑推演
- 业务流程推演：✓ 4 个阶段独立闭环
- 业务规则推演：✓ ML 分类器信心阈值逻辑完整
- 业务状态推演：✓ 压缩版本状态管理正确
- 业务数据推演：✓ 审批历史→ML训练→自动决策管线完整
- 业务异常推演：✗ asyncRewake 可能无限循环（已有最大重试3次限制）
- 业务边界推演：✓ ML 冷启动退化为规则引擎
- 业务依赖关系：✓ 依赖 P0 的 MicroCompact 和 Context Collapse
- 业务异常恢复：✓ 压缩版本支持回滚

### 技术方案审查
- 文件路径正确：✓
- 依赖关系合理：✓ linfa 为纯 Rust ML 库
- 技术方案可行：✓
- 接口契约一致：✓
- 配置项完整：✓

### 执行可行性审查
- 步骤无遗漏：✓ 8 个任务
- 步骤无冲突：✓
- 资源可获取：✓
- 环境可支持：✓

### 审查结论
- 发现问题：0 个
