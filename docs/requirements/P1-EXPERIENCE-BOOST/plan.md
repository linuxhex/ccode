# P1 显著提升体验实现计划

**目标：** 补齐 4 项 P1 差距，显著提升 ccode 用户体验

**架构：** 在 P0 基础上，增强 Context Collapse、新增 ML 分类器 Auto Mode、Hook async、DesignSync

**技术栈：** Rust + linfa (ML) + tokio (async Hook) + ccode-http (DesignSync API)

---

## 交付阶段

### 阶段 A：Context Collapse 增强 — 压缩更智能
- 保留策略优化（决策/纠正/约束/排除方案始终保留）
- 增量压缩（只压缩新增轮次）
- 摘要质量评估（关键信息保留率检查）

### 阶段 B：ML 分类器 Auto Mode — 权限更自主
- 审批历史记录收集
- 轻量 ML 分类器（决策树/逻辑回归）
- 新增权限模式（auto-edit/full-auto/plan）
- 在线学习

### 阶段 C：Hook async+asyncRewake — Hook 更灵活
- async Hook 后台执行
- asyncRewake 错误唤醒模型
- 超时和重试控制

### 阶段 D：DesignSync 工具 — 设计更专业
- 组件扫描和预览生成
- 设计 token 提取
- claude.ai/design API 同步

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| crates/common/ccode-compaction/src/context_collapse.rs | 修改 | 增强保留策略+增量压缩+质量评估 |
| crates/common/ccode-compaction/src/collapse_version.rs | 新增 | 压缩版本状态管理 |
| crates/codegen/ccode-hooks/src/permission_classifier.rs | 新增 | ML 分类器 Auto Mode |
| crates/codegen/ccode-hooks/src/permission_history.rs | 新增 | 审批历史记录 |
| crates/codegen/ccode-hooks/src/runner/async_runner.rs | 新增 | async Hook 执行器 |
| crates/codegen/ccode-hooks/src/runner/mod.rs | 修改 | 导出 async_runner |
| crates/codegen/ccode-hooks/src/config.rs | 修改 | 支持 async/asyncRewake 配置 |
| crates/codegen/ccode-shell/src/permission_mode.rs | 新增 | 新增 auto-edit/full-auto/plan 模式 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/mod.rs | 新增 | DesignSync 工具 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/scanner.rs | 新增 | 组件扫描器 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/tokens.rs | 新增 | 设计 token 提取 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/client.rs | 新增 | claude.ai API 客户端 |

---

## 任务拆分

### 任务 1：Context Collapse 保留策略优化

**目标**：关键信息始终保留，低价值内容可安全丢弃

**文件**：
- 修改：`crates/common/ccode-compaction/src/context_collapse.rs`

**实现要点**：
- `RetentionCategory` 枚举：Decision/Correction/Constraint/ExcludedPlan/Chat/Trial/Repeat
- `classify_retention(item) -> RetentionCategory`：基于内容特征分类
  - 用户确认/否决 → Decision
  - 用户纠正 Agent → Correction（标注「反向知识」）
  - 用户指定限制 → Constraint
  - 用户否决方案 → ExcludedPlan
  - 其他 → Chat/Trial/Repeat
- 保留策略：Decision/Correction/Constraint/ExcludedPlan 始终保留，其他可压缩/丢弃
- 压缩 prompt 中明确指示 LLM 保留这些类别的内容

---

### 任务 2：增量压缩 + 版本管理

**目标**：只压缩新增轮次，已压缩摘要不重复压缩

**文件**：
- 新增：`crates/common/ccode-compaction/src/collapse_version.rs`
- 修改：`crates/common/ccode-compaction/src/context_collapse.rs`

**实现要点**：
- `CollapseVersion`：版本号 + 已压缩的轮次范围 + 摘要内容
- `incremental_collapse(new_items, prev_version, llm_client) -> CollapseVersion`：
  - 只对 prev_version 之后的新轮次做压缩
  - 合并新摘要与旧摘要
  - 返回新版本
- 版本回滚：保留最近 N 个版本（默认 3），支持回滚
- Agent 崩溃恢复：从最新版本恢复压缩状态

---

### 任务 3：摘要质量评估

**目标**：压缩后检查关键信息保留率

**文件**：
- 修改：`crates/common/ccode-compaction/src/context_collapse.rs`

**实现要点**：
- `quality_check(original_items, summary, key_items) -> QualityReport`：
  - 提取原始对话中的关键信息（决策/纠正/约束）
  - 检查摘要中是否包含这些关键信息
  - 计算保留率 = 保留的关键信息数 / 总关键信息数
- 保留率 < 90% 时告警
- 告警处理：重新压缩（增大摘要 token 预算）或保留原始内容

---

### 任务 4：审批历史记录收集

**目标**：记录用户每次权限审批决策，供 ML 分类器学习

**文件**：
- 新增：`crates/codegen/ccode-hooks/src/permission_history.rs`

**实现要点**：
- `PermissionHistory`：审批历史记录器
- 记录格式：JSONL，每行一条审批记录
  ```json
  {"tool":"Bash","input":"git status","decision":"allow","timestamp":"2026-07-29T10:00:00Z","confidence":0.98}
  ```
- 存储位置：`~/.ccode/permission_history.jsonl`
- 特征提取：tool_name, command_pattern（Bash 命令前缀）, file_path_pattern, project_type
- 历史查询：按 tool_name + pattern 查询历史决策统计

---

### 任务 5：ML 分类器 Auto Mode

**目标**：基于历史审批数据自动决策权限

**文件**：
- 新增：`crates/codegen/ccode-hooks/src/permission_classifier.rs`
- 新增：`crates/codegen/ccode-shell/src/permission_mode.rs`

**实现要点**：
- `PermissionClassifier`：ML 分类器
- 特征向量：[tool_name_hash, command_pattern_hash, file_path_hash, project_type_hash, hour_of_day]
- 模型：逻辑回归（linfa-logistic），本地推理 <1ms
- `classify(features) -> (decision, confidence)`：
  - confidence > 0.95 → Allow
  - confidence < 0.05 → Deny
  - 其他 → Ask（弹出确认）
- 冷启动：无历史数据时退化为规则引擎
- 在线学习：每次审批后增量更新模型参数
- 新增权限模式：
  - `auto-edit`：编辑自动 allow，Bash 需确认
  - `full-auto`：ML 分类器自动决策
  - `plan`：只读，禁止写入和执行

---

### 任务 6：Hook async 执行

**目标**：Hook 可后台异步执行，不阻塞 Agent 主循环

**文件**：
- 新增：`crates/codegen/ccode-hooks/src/runner/async_runner.rs`
- 修改：`crates/codegen/ccode-hooks/src/runner/mod.rs`
- 修改：`crates/codegen/ccode-hooks/src/config.rs`

**实现要点**：
- `AsyncHookRunner`：异步 Hook 执行器
- `run_async(hook, input, timeout) -> JoinHandle<HookResult>`：
  - tokio::spawn 后台执行
  - 结果通过 mpsc channel 发送到主循环
  - 独立超时（默认 30s）
- Hook 配置新增字段：
  ```json
  {"type": "command", "command": "audit.sh", "async": true, "asyncRewake": false, "timeout": 30}
  ```
- 主循环处理 async Hook 结果时不阻塞，结果异步消费

---

### 任务 7：Hook asyncRewake 唤醒

**目标**：async Hook 失败时唤醒模型重试

**文件**：
- 修改：`crates/codegen/ccode-hooks/src/runner/async_runner.rs`

**实现要点**：
- asyncRewake 触发条件：async Hook exit code != 0 且 `asyncRewake: true`
- 唤醒方式：
  1. 将 Hook 的 stderr 作为 `additional_context` 注入
  2. 通过 ccode-interjection 发送到 Agent 主循环
  3. Agent 在下一轮看到错误信息，自动修复
- 防止无限循环：同一 Hook asyncRewake 最大触发 3 次/轮
- 适用场景：代码格式检查 Hook → 格式错误 → Agent 看到错误 → 自动修复

---

### 任务 8：DesignSync 工具

**目标**：将本地组件库同步到 claude.ai/design

**文件**：
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/mod.rs`
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/scanner.rs`
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/tokens.rs`
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/design_sync/client.rs`

**实现要点**：
- `ComponentScanner`：扫描组件目录
  - 支持 React (.jsx/.tsx)、Vue (.vue)、Svelte (.svelte)
  - 提取组件名、props、描述
  - 生成预览 HTML（Storybook 或内联渲染）
- `TokenExtractor`：提取设计 token
  - 从 CSS 变量、SCSS 变量、Tailwind 配置提取
  - 输出：颜色、间距、字体、阴影等 token
- `DesignSyncClient`：claude.ai/design API 客户端
  - 认证：claude.ai OAuth
  - API：create_project, list_files, write_files, register_assets
  - 同步流程：扫描 → 生成预览 → 推送到 claude.ai/design
- 工具暴露：`design_sync` 工具
  - 输入：component_dir, project_id, sync_mode (full/incremental)
  - 输出：同步结果（新增/更新/删除的组件数）

---

## 验证方案

### 阶段 A 验证（Context Collapse 增强）
1. 构造含决策/纠正/约束的对话，验证压缩后这些内容保留
2. 验证增量压缩只处理新增轮次
3. 验证摘要质量评估：保留率 < 90% 时告警

### 阶段 B 验证（ML Auto Mode）
1. 构造审批历史数据，训练分类器
2. 验证 full-auto 模式下高频操作自动 allow
3. 验证低信心操作弹出确认
4. 验证冷启动退化为规则引擎

### 阶段 C 验证（Hook async）
1. 配置 async Hook，验证不阻塞 Agent 主循环
2. 验证 asyncRewake：Hook 失败→模型看到错误→自动修复
3. 验证最大重试 3 次限制

### 阶段 D 验证（DesignSync）
1. 扫描 React 组件目录，验证组件元数据提取
2. 验证设计 token 提取（CSS 变量+Tailwind）
3. 验证同步到 claude.ai/design（需测试账户）
