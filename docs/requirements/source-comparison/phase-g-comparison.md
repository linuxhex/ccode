# Phase G：Claude Code 实际源码 + grok-build 深度对比

> 前序：CCODE-004（6 款工具概览）→ CCODE-005（架构收敛+TUI）→ 本轮
> 本轮新增：Claude Code 实际 TypeScript 源码 ~1884 文件（前序全为文档推断）
> 分析日期：2026-09-04

---

## 一、本轮新发现（前序未覆盖）

### 1.1 Claude Code 文件检查点系统（fileHistory.ts，1115 行）

ccode 之前删除了零调用的 `CheckpointManager`（Phase F P0-14），但 Claude Code 实际上有**完整的生产级文件检查点系统**：

| 能力 | Claude Code 实现 | ccode 现状 |
|------|-----------------|-----------|
| 编辑前自动备份 | `fileHistoryTrackEdit()` 在每次文件编辑前调用，备份原始内容 | ❌ 无 |
| 版本化备份 | `{hash}@v1`、`{hash}@v2` 递增版本号，防覆盖 | ❌ 无 |
| 快照与消息关联 | `FileHistorySnapshot` 关联 `messageId`，支持按消息回滚 | ❌ 无 |
| 快照上限 | `MAX_SNAPSHOTS = 100`，LRU 淘汰 | ❌ 无 |
| 变更检测 | `checkOriginFileChanged()` 比较 mtime + 内容 hash | ❌ 无 |
| 删除文件追踪 | `backupFileName: null` 标记被删文件，支持恢复 | ❌ 无 |
| 并发安全 | 三阶段设计（capture → async IO → commit），防竞态 | ❌ 无 |
| 配置开关 | `fileCheckpointingEnabled` 全局配置 + 环境变量覆盖 | ❌ 无 |

**关键设计**：
- `fileHistoryTrackEdit` 在编辑**前**调用，保存原始内容
- `fileHistoryMakeSnapshot` 在每轮消息结束时调用，备份所有被追踪文件的当前版本
- 文件路径缩短存储（`maybeShortenFilePath`）节省空间
- 遥测集成（`tengu_file_history_*` 事件）

### 1.2 Claude Code 编辑工具容错（FileEditTool/utils.ts）

ccode Phase F 已修了 P0-1（四级容错阶梯），但 Claude Code 有额外的容错层：

| 容错层 | Claude Code | ccode |
|--------|-------------|-------|
| 精确匹配 | ✅ | ✅ |
| 引号归一化 | ✅ `normalizeQuotes()` 弯引号→直引号 | ❌ |
| 反消毒还原 | ✅ `desanitizeMatchString()` API 占位符还原 | ❌ |
| 尾部空白剥离 | ✅ `stripTrailingWhitespace()` | ⚠️ 部分（行尾归一化） |
| 编码保真 | ✅ `readFileSyncWithMetadata` 检测 UTF-8/UTF-16LE + EOL | ❌ |
| Windows 时间戳容忍 | ✅ 内容相同则容忍 mtime 漂移 | ❌ |

### 1.3 Claude Code Auto-mode AI 分类器

ccode 的 auto-mode 用规则匹配（side_effect_eval），Claude Code 用**小模型分类器**：

```
权限检查流程：
1. 安全工具白名单 → 直接放行
2. acceptEdits 快速路径 → 已知安全操作放行
3. classifyYoloAction() → 发对话历史+工具调用给小模型判断安全性
4. 拒绝累计 → DenialTrackingState 追踪连续拒绝次数
5. 超限降级 → 回退到人工确认
```

**关键文件**：`permissions/yoloClassifier.ts`、`permissions/bashClassifier.ts`、`permissions/classifierDecision.ts`

### 1.4 Claude Code microcompact（工具结果预算裁剪）

ccode 有 4 级压缩，但缺少**单工具结果级别**的裁剪：

| 能力 | Claude Code | ccode |
|------|-------------|-------|
| 工具结果预算 | `applyToolResultBudget()` 大结果磁盘持久化+预览替换 | ❌ |
| microcompact | 旧文件读取内容替换为 stub（530 行实现） | ⚠️ 有 micro_compact 但无工具结果裁剪 |
| 压缩后状态恢复 | 恢复最近 5 个文件附件（每文件 5K 上限）+ 技能/计划状态 | ❌ |

### 1.5 Claude Code Prompt Cache 前缀一致性设计

| 设计 | Claude Code 实现 | ccode 现状 |
|------|-----------------|-----------|
| 冻结 prompt 字节 | `renderedSystemPrompt` 冻结字节数，防 GrowthBook 状态变化导致 cache bust | ⚠️ 有 cache_placement 但无冻结 |
| fork 继承 | `FORK_AGENT` 用 `tools: ['*']` + `model: 'inherit'` + 继承已渲染 systemPrompt | ⚠️ 有 cache_control 但无完整继承 |
| 权限检查复用 | `yoloClassifier` 请求使用 `getCacheControl({ querySource: 'auto_mode' })` 共享缓存前缀 | ❌ |

### 1.6 grok-build 唯一未移植功能：秘密脱敏

**`xai-grok-secrets`** 是 grok-build 有而 ccode 无的唯一实质性功能：

- 正则检测：`sk-*`、`xai-*`、`AKIA`、`ghp_`、`glpat-`、`xox*-`、PEM 私钥、Bearer、JWT 等
- JSON 遍历脱敏：`redact_json_string_values()`、`redact_url()`、`redact_user_paths()`
- ccode 的 `ccode-telemetry/src/redact_common.rs` 中 `redact_owned()` 直接返回 `None`，是**空桩**

**风险**：用户日志/遥测数据中可能泄露 API key、AWS 凭证、GitHub PAT 等敏感信息。

---

## 二、ccode 剩余未修复问题汇总

### P1 级（用户可感知损害）

| 编号 | 问题 | 影响 | 修复成本 |
|------|------|------|---------|
| P1-2 | `read_tracker` 只记"读过没"，不记版本 | 外部改动被 `write_file` 静默吞掉 | 中（改 HashSet→HashMap+FileStamp） |
| P1-3 | `read_tracker` 以原始路径字符串为 key | 路径别名导致误拦 | 低（统一用 canonical path） |
| P1-1 残留 | `credentials.rs:203`、`file_storage.rs:47` 仍用 `with_extension("tmp")` | 静默覆盖用户 `.tmp` 文件 | 低（照 builtin.rs 已有修法） |

### P0 级（战略决策）

| 编号 | 问题 | 决策 |
|------|------|------|
| P0-1' | Context Engine 恒空转 | **建议不救**（前序已论证），改为删除死代码减少认知负债 |

---

## 三、改进计划（按优先级排序）

### G1：秘密脱敏（安全 P0）

**来源**：grok-build `xai-grok-secrets`
**方案**：移植 `sanitizer.rs` 的正则匹配集和 JSON 遍历脱敏
**改动**：`ccode-telemetry/src/redact_common.rs` 或新建 `ccode-secrets` crate
**收益**：消除凭证泄露风险

### G2：文件检查点系统（编辑安全 P0）

**来源**：Claude Code `fileHistory.ts`
**方案**：实现 `FileHistory` 模块，编辑前自动备份 + 版本化 + 消息关联
**改动**：新建 `ccore/src/tools/file_history.rs`，在 `builtin.rs` 编辑入口调用
**收益**：错误编辑可回滚，对标 Claude Code

### G3：read_tracker 版本追踪（编辑安全 P1）

**来源**：opencode mtime 检测 + Claude Code 文件过期检测
**方案**：`HashSet<String>` → `HashMap<PathBuf, FileStamp{mtime, len}>`
**改动**：`ccore/src/tools/read_tracker.rs`
**收益**：`write_file` 全量覆盖前检测外部改动

### G4：编辑工具增强（编辑质量 P1）

**来源**：Claude Code `FileEditTool/utils.ts`
**方案**：
- 引号归一化（弯引号→直引号）
- 编码保真（读时检测 UTF-8/UTF-16LE + EOL，写时还原）
**改动**：`ccore/src/tools/builtin.rs` 编辑执行器
**收益**：减少特殊字符/编码导致的编辑失败

### G5：Context Engine 死代码清理（认知负债 P1）

**方案**：删除 `sensory.rs` 中硬编码零向量的 `listen_enrichment`，或标记为 `#[cfg(feature = "experimental")]`
**改动**：`ccore/src/node/thinker/sensory.rs`、`intent_retriever.rs`
**收益**：消除"我们有语义检索"的认知负债

### G6：工具结果预算裁剪（上下文管理 P2）

**来源**：Claude Code `applyToolResultBudget()`
**方案**：大工具结果（>32KB）磁盘持久化 + 预览替换
**改动**：`ccore/src/tools/` 工具结果处理管线
**收益**：减少长会话上下文膨胀

### G7：P1-1 残留修复 + P1-3 路径规范化（代码质量 P2）

**改动**：
- `ccode-mcp/src/credentials.rs:203` → 改用 `unique_tmp_path`
- `ccore/src/persistence/file_storage.rs:47` → 同上
- `read_tracker` key 统一用 canonical path

---

## 四、建议实施顺序

```
G1（秘密脱敏）→ G3（read_tracker 版本）→ G7（P1-1 残留 + P1-3）
    → G2（文件检查点）→ G4（编辑增强）→ G5（死代码清理）→ G6（结果预算）
```

**G1-G3 是本轮最小高收益集合**：秘密脱敏消除安全风险，read_tracker 版本追踪 + 路径规范化消除编辑数据丢失风险，三者合计改动量约 300-500 行。
