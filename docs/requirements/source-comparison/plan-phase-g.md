# Phase G 实现计划

> 基于 Phase G 对比分析，按优先级排序的 7 个改进项

---

## G1：秘密脱敏（安全 P0）

**目标**：消除 telemetry 日志中的凭证泄露风险

**改动文件**：
1. `ccode-telemetry/src/redact_common.rs` — 实现 `redact_owned()`，移植 grok-build 正则匹配集
2. `ccode-telemetry/src/redact_common.rs` — 新增 `redact_json_string_values()`、`redact_url()`、`redact_user_paths()`

**正则匹配集**（从 grok-build `sanitizer.rs` 移植）：
- `sk-[a-zA-Z0-9]{20,}` — OpenAI API key
- `xai-[a-zA-Z0-9]{20,}` — xAI API key
- `AKIA[0-9A-Z]{16}` — AWS Access Key
- `ghp_[a-zA-Z0-9]{36}` — GitHub PAT
- `glpat-[a-zA-Z0-9\-]{20}` — GitLab PAT
- `xox[baprs]-[a-zA-Z0-9\-]+` — Slack token
- `-----BEGIN.*PRIVATE KEY-----` — PEM 私钥
- `Bearer [a-zA-Z0-9\-._~+/]+=*` — Bearer token
- `ey[a-zA-Z0-9_-]+\.ey[a-zA-Z0-9_-]+` — JWT

**测试**：
- 各格式匹配/不匹配测试
- JSON 遍历脱敏测试
- URL 中参数脱敏测试

---

## G2：文件检查点系统（编辑安全 P0）

**目标**：编辑前自动备份，支持回滚

**改动文件**：
1. 新建 `ccore/src/tools/file_history.rs` — FileHistory 模块
2. `ccore/src/tools/mod.rs` — 注册模块
3. `ccore/src/tools/builtin.rs` — WriteExecutor 和 Edit Executor 入口调用 `file_history_track_edit()`

**核心设计**（参照 Claude Code `fileHistory.ts`）：
```rust
pub struct FileHistory {
    snapshots: Vec<FileHistorySnapshot>,  // MAX 100
    tracked_files: HashSet<PathBuf>,
    session_dir: PathBuf,
}

pub struct FileHistorySnapshot {
    pub message_id: String,
    pub tracked_file_backups: HashMap<PathBuf, FileBackup>,
    pub timestamp: SystemTime,
}

pub struct FileBackup {
    pub backup_path: Option<PathBuf>,  // None = 文件不存在
    pub version: u32,
    pub backup_time: SystemTime,
}
```

**关键方法**：
- `track_edit(file_path, message_id)` — 编辑前备份原始内容
- `make_snapshot(message_id)` — 每轮消息结束创建快照
- `rollback_to(message_id)` — 回滚到指定消息点

---

## G3：read_tracker 版本追踪（编辑安全 P1）

**目标**：write_file 全量覆盖前检测外部改动

**改动文件**：
1. `ccore/src/tools/read_tracker.rs` — `HashSet<String>` → `HashMap<PathBuf, FileStamp>`
2. `ccore/src/tools/builtin.rs` — `mark_file_read` 时记录 FileStamp，`require_read` 时比对

**FileStamp 结构**：
```rust
pub struct FileStamp {
    pub mtime: SystemTime,
    pub len: u64,
}
```

**策略**：
- `mark_read(path)` — 记录 mtime + len
- `require_read(path)` — 若 mtime/len 变化则返回 `Stale` 错误
- 对 `search_replace`：宽松策略（目标文本仍唯一匹配则放行）
- 对 `write_file`：严格策略（stamp 不符即拒绝）

---

## G4：编辑工具增强（编辑质量 P1）

**目标**：减少特殊字符/编码导致的编辑失败

**改动文件**：
1. `ccore/src/tools/builtin.rs` — `execute_replace` 增加引号归一化
2. `ccore/src/tools/builtin.rs` — 读写路径增加编码检测/还原

**引号归一化**：
```rust
fn normalize_quotes(s: &str) -> String {
    s.replace('\u{2018}', "'")  // ' → '
     .replace('\u{2019}', "'")  // ' → '
     .replace('\u{201C}', "\"") // " → "
     .replace('\u{201D}', "\"") // " → "
     .replace('\u{2010}', "-")  // ‐ → -
     .replace('\u{2011}', "-")  // ‑ → -
     .replace('\u{2012}', "-")  // ‒ → -
     .replace('\u{2013}', "-")  // – → -
     .replace('\u{2014}', "-")  // — → -
     .replace('\u{2212}', "-")  // − → -
}
```

**编码保真**：
- 读时检测 BOM（UTF-8 BOM、UTF-16LE BOM）和 EOL（CRLF/LF）
- 记入 FileStamp
- 写时还原

---

## G5：Context Engine 死代码清理（认知负债 P1）

**目标**：删除或标记实验性代码，消除认知负债

**改动文件**：
1. `ccore/src/node/thinker/sensory.rs` — 删除 `listen_enrichment` 中的零向量硬编码
2. `ccore/src/node/thinker/mod.rs` — 删除 `self.listen_enrichment(content)` 调用
3. 或：加 `#[cfg(feature = "experimental-semantic-search")]` 条件编译

---

## G6：工具结果预算裁剪（上下文管理 P2）

**目标**：大工具结果磁盘持久化 + 预览替换，减少长会话上下文膨胀

**改动文件**：
1. 新建 `ccore/src/tools/result_budget.rs`
2. `ccore/src/tools/builtin.rs` — 工具返回结果后调用 `budget_apply()`

**设计**：
```rust
pub struct ResultBudget {
    pub max_inline_bytes: usize,     // 默认 32KB
    pub persist_dir: PathBuf,
}

impl ResultBudget {
    pub fn budget_apply(&self, result: &str) -> BudgetedResult {
        if result.len() <= self.max_inline_bytes {
            BudgetedResult::Inline(result.to_string())
        } else {
            // 持久化到磁盘，返回预览
            let preview = build_preview(result, self.max_inline_bytes / 4);
            let path = self.persist(result);
            BudgetedResult::Persisted { preview, path }
        }
    }
}
```

---

## G7：P1-1 残留修复 + P1-3 路径规范化（代码质量 P2）

**改动文件**：
1. `ccode-mcp/src/credentials.rs:203` — `with_extension("tmp")` → `unique_tmp_path`
2. `ccore/src/persistence/file_storage.rs:47` — 同上
3. `ccore/src/tools/read_tracker.rs` — key 统一用 canonical path（配合 G3）

---

## 实施顺序与依赖

```
第一批（安全+编辑安全，无依赖）：
  G1（秘密脱敏）
  G3（read_tracker 版本）+ G7（P1-1 残留 + P1-3 路径）

第二批（编辑安全+质量，依赖 G3）：
  G2（文件检查点）
  G4（编辑增强）

第三批（清理+优化，无依赖）：
  G5（死代码清理）
  G6（结果预算）
```

**预计总改动量**：约 800-1200 行新增/修改代码 + 测试
