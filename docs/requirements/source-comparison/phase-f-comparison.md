# Phase F 源码对比：未覆盖维度

> 承接 CCODE-004/005。前序已覆盖：全维度概览、TUI、沙箱安全、Sub-Agent 生命周期、错误处理、
> 流式+Prompt、配置+插件、记忆系统、上下文压缩、工具执行管线、Hook/Plugin/Skill、codex 专项。
> 本轮补三个未覆盖维度：**编辑工具与补丁算法**、**会话持久化/恢复/回溯**、**代码检索与 repo map**。

## 判定基准（与前序一致，务必先读）

ccode 唯一产品二进制是 `ccode-cli`（全 workspace 唯一名为 `ccode` 的 bin）。
`ccode-cli/src/main.rs:126-150` 无分支地走 `Kernel::new(...)` → `kernel.run().await`。
**主线 = `ccore` 的 Kernel + Thinker/Sampler/ToolNode**；`ccode-shell`(SessionActor) 与 `ccode-pager`
没有产品二进制入口。故本文对 ccode 的能力判定分三态：

- **①主线真接线** —— 从 `ccode` 二进制能走到
- **②仅非主线** —— 只存在于 shell/pager，用户拿不到
- **③缺失**（含"代码写好了但零调用者"——对用户等价于缺失，且比缺失更危险，因为它会骗过 review）
- **④接线成功但恒空转** —— 调用链走通，但必定返回空

`harness-cli` 在"编辑"与"检索"维度记为 **N/A**：它是 DevOps 平台 CLI（pipeline / gitops / 制品库，
`pkg/spec/*.spec.yaml` 是 API 资源规范），并非改源码的 coding agent，不构成可比项。
`claude-code` 仓内无实现源码（仅 CHANGELOG 513KB + plugins + examples），其列全部标注**文档推断**。

---

## 汇总：三维度差距清单（跨维度合并后）

> 编号沿用各维度内的原编号，便于回查证据。**排序按"用户可感知损害 × 修复成本"**，不是按维度顺序。

### 最重要的一条横向发现

三个维度各自独立地撞到同一个模式：**`ccore` 里有一批实现完整、带测试、注释还在承诺功能的代码，
生产调用者数为 0**。本轮新确认的死代码：`CheckpointManager`、`FileTransaction`、
`ccore::prompt::agents_md`（连同从未被构造的 `ccore::prompt::PromptContext`）、
`memory::world_state`、`StateNode::load_session_jsonl`/`load_session`、
`IntentRetriever::register_blocks`/`set_repo_map`（唯一调用点在非主线且那个调用点自身零调用者）。
其中 **`checkpoint.rs:1` 的注释与事实完全相反**（承诺"每次文件编辑前自动 checkpoint"，实际零调用）。
`pub` 项不触发 dead_code 警告，所以 `cargo check` 干净不构成任何接线证据 —— 这条纪律在本轮三个维度里
各救了一次误判。

### 故障链：三个维度的 P0 串成同一条

> 这是本轮最值得优先处理的东西，**单独看任一条都不够严重，串起来是数据丢失**：

1. CRLF 仓库里 `search_replace` **必然**失败（P0-1，`builtin.rs:510/516` 裸子串匹配）
2. → 模型退化成 `write_file` 全量覆盖（`bridge.rs:322` 自述"会完全覆盖现有文件内容，没有警告或确认"）
3. → 先读后写只记"读过"不记版本，外部改动被静默吞掉（P1-2，`read_tracker.rs:16` 无 mtime/hash）
4. → `CheckpointManager` 零调用，**无任何回滚可能**（P0-14）
5. → 会话"只写不读"，事后也无法从会话记录复原当时状态（P0-12）

修复 P0-1（加 CRLF 兼容 + codex 式模糊阶梯）与 P0-14（在 `builtin.rs:373`/`:458` 各调一次
`create_checkpoint`）能断掉这条链的两端，两者都是小改动。

### P0

| 编号 | 维度 | 问题 | 一句话后果 |
|---|---|---|---|
| P0-1 | 编辑 | `search_replace` 零容错，CRLF 文件必失败 | 该工具在 CRLF 仓整体不可用，模型被迫走最危险的全量覆盖 |
| P0-2 | 编辑 | `bridge.rs:405` 声明与 `builtin.rs:520-525` 实现**相反**；`mode`/`replace_all`/`after_line`/`content` 已实现但未声明 | 已实现的 insert 模式模型永远用不到；报错还在教模型用它看不见的参数 |
| P0-12 | 会话 | 会话**只写不读**：恢复函数零调用者 + CLI 无 flag | 崩溃/Ctrl-C 后会话不可恢复。**5 家可比参照全员有、ccode 全无的唯一一项** |
| P0-13 | 会话 | `session_id` 每次启动新 UUID 且无注入通道 | 就算接上恢复也没法指定恢复哪一个 |
| P0-14 | 会话/编辑 | `CheckpointManager` + `FileTransaction` 双双零调用，注释与事实相反 | 编辑无安全网，错误覆盖不可挽回 |
| P0-2' | 检索 | AGENTS.md 主线零消费（仓内三份实现） | 标准 AGENTS.md 仓库的项目规范一个字都读不到 |
| P0-3' | 检索 | `CLAUDE.md`/`CCODE.md` 无向上遍历、无字节上限 | monorepo 子目录启动即丢项目规范；200KB 规范原文塞满上下文 |
| P0-1' | 检索 | Context Engine 恒空转（两端各一个必空开关） | 宣称的语义检索零效果 + 认知负债 |

### P1

| 编号 | 维度 | 问题 |
|---|---|---|
| P1-1 | 编辑 | `with_extension("tmp")` 吃扩展名，无并发即可静默覆盖用户既有 `.tmp` 文件（本仓 `ccode-update` 已修过同一 bug，另两处未修） |
| P1-2 | 编辑 | 先读后写不记版本，`write_file` 静默吞掉外部改动 |
| P1-3 | 编辑 | `read_tracker` 以模型给的原始路径字符串为键，路径别名导致误拒 |
| P1-15 | 会话 | `conversation.json` 每条消息全量重写，O(n²) 写放大 |
| P1-16 | 会话 | 两个落盘文件内容不一致，且 `load_session` 读的正是双重降级的那个 |
| P1-17 | 会话 | `~/.ccode/sessions/` 无 GC，无限膨胀 |
| P1-18 | 会话 | `HOME` 直读，Windows 上会话写进当前工作目录 |
| P1-4 | 检索 | `glob` 用 `find -name` 而非 `rg --files`，不支持 `**`，gitignore 事后过滤 |
| P1-5 | 检索 | `read_file` 拒读 gitignore 文件 —— **六家唯一**，且与 grep/glob 三工具三语义 |
| P1-6 | 检索 | 200 行截断不回传模型，模型以为搜完了 |
| P1-7 | 检索 | `list_dir` 无 gitignore 过滤、无条目上限，`target/` 一把撑爆上下文 |

### P2

编辑：无多 hunk 原子编辑（P2-1）、无换行/BOM 保真（P2-3）。
会话：恢复用整文件读而非反向扫描（P2-19）、无跨目录恢复语义（P2-20）、CLI 错误信息质量（P2-21）。
检索：无模糊文件名搜索（P2-8）、**`rg`/`find` 子进程无超时**（P2-9，`bash` 与 web 工具都有超时，只有检索
工具漏了，灾难性回溯正则可让 agent 无限挂死）、WorldState 半移植导致上下文重锚缺失（P2-10）、
LSP 未接入检索链（P2-11）。

### 一条战略结论

**不要救 Context Engine。** 6 款参照（含 codex、Claude Code 两个最强的）**无一**对代码做语义向量检索或
持久符号索引，全部是 glob + ripgrep 的 agentic 搜索。ccode 缺的是索引的全部前置条件而非两个开关
（无 `ignore`、无 tree-sitter、`listen_enrichment` 是同步 fn 与 async embedding 结构性冲突、维度三处不一致、
无持久化与失效机制）。把这份工作量投到 P0-2'/P0-3' 与 P1-4/6/7 上，性价比高一个数量级。
想解决上下文膨胀，学 Claude Code 的 subagent 隔离（ccode 已有 `task` 工具）比学索引有效得多。
详见 3.7。

---

# 维度一：编辑工具与补丁应用算法

## 1.1 对比矩阵

| | codex | claude-code（文档推断） | opencode | deepcode-cli | grok-build | ccode 主线 |
|---|---|---|---|---|---|---|
| 编辑原语 | ✅ V4A patch（Add/Delete/Update/Move + `@@` 上下文锚） | ✅ Edit / MultiEdit / Write / NotebookEdit | ✅ `edit.go` + 独立 `patch.go` 双原语 | ✅ str-replace + snippet 绑定 | ⚠️ `search_replace` 单原语 | ⚠️ `search_replace` 单原语 + `write_file` 全量重写 |
| 匹配容错 | ✅ **四级阶梯**：精确→忽略行尾空白→忽略首尾空白→Unicode 标点归一化 | ✅ 字节级保真为契约（修过智能引号被改直的 bug） | ❌ 裸 `strings.Index`，硬失败 | ❌ 精确匹配 | ❌ 裸 `matches()` | ❌ **零容错**，裸 `content.matches(old_text)` |
| 唯一性约束 | ✅ 上下文锚 + EOF 锚定消歧 | ✅ 有唯一性概念（`replace_all` 无文档证据） | ✅ `Index != LastIndex` 则报错 | ✅ 不唯一时报错，提示用 snippet_id | ⚠️ 同 ccode | ✅ 默认要求唯一，`count>1` 报错并带命中数 |
| 先读后写 | — （patch 自带上下文，无需此约束） | ✅ 有，且 `grep`/`head`/`tail` 也算读过 | ✅ `getLastReadTime` 为零则拒 | ✅ snippet_id 必填即隐含 | ⚠️ 同 ccode | ✅ `read_tracker::require_file_read` |
| 过期检测 | ✅ 上下文行不匹配即失败（内建） | ✅ 改动后若目标文本仍唯一匹配则放行（务实策略） | ✅ **mtime > lastRead 则拒**并报出两个时间 | ✅ **per-session fileVersion**，snippet 落后即过期 | ❌ | ❌ **只记"读过没"，不记版本** |
| 换行/编码保真 | ✅ `PreserveLineEndings` / `NormalizeToLf` 双模式 | ✅ 保留文件既有 EOL，新建时按目录推断 CRLF；保留尾部空行 | ⚠️ 未见处理 | ⚠️ 未见处理 | ❌ | ❌ 无任何处理 |
| 原子性/并发 | ✅ 经 exec-server FS 抽象，支持沙箱 | ✅ 尊重 umask；网络盘 0 字节问题修过 | ⚠️ 直写 | ⚠️ 直写 | ❌ 同 ccode | ⚠️ 临时文件 + rename，但**临时文件名会吃扩展名** |
| 失败反馈质量 | ✅ `InvalidHunkError{line_number, message}` 精确定位 | ✅ 分类错误（`InputValidationError` 等） | ✅ 报错含 mtime/lastRead 具体值 | ✅ 报错直接给出补救手段 | ⚠️ | ⚠️ 仅"未找到搜索文本"，不给附近实际内容 |
| 编辑后动作 | ✅ unified diff 产出 | ✅ diff 渲染（5s 超时降级）、词级 diff | ✅ diff + 权限请求 + LSP 诊断 | ✅ | ⚠️ | ✅ unified diff + 编辑点上下文 + rustfmt post-hook |
| 多 hunk 原子编辑 | ✅ 一个 patch 多文件多 hunk | ✅ MultiEdit | ✅ `patch.go` | ❌ | ❌ | ❌ |

## 1.2 各家关键实现

### codex —— 这维度最成熟的参照

- **不是"加一个编辑工具"，而是拦截 bash 调用**：模型在 shell 里写
  `apply_patch <<'EOF' ... EOF`，由 `invocation.rs::maybe_parse_apply_patch` 在执行路径上截获
  （`codex-rs/apply-patch/src/invocation.rs`，1036 行）。另有 arg0 自调用的独立可执行体
  （`lib.rs:52` `CODEX_CORE_APPLY_PATCH_ARG1`）。
- **V4A 格式有正式 Lark 文法**（`parser.rs:1-25`）：
  `*** Begin Patch` / `*** Add File:` / `*** Delete File:` / `*** Update File:` / `*** Move to:` /
  `@@ 上下文` / `*** End of File` / `*** End Patch`。解析器比规范更宽松（容忍标记前后空白），
  且对 gpt-4.1 显式保留 lenient 模式（`parser.rs:47-53`）。
- **四级容错阶梯**（`seek_sequence.rs:39-112`）：精确 → `trim_end` → `trim` → Unicode 标点归一化
  （各种 dash `U+2010..U+2015`/`U+2212` → `-`，弯引号 → 直引号，各种空格 → 普通空格）。
  注释里点明这是"mirrors the fuzzy behaviour of `git apply`"。
- **EOF 锚定**（`seek_sequence.rs:30-38`）：`eof` 为真时先从文件末尾开始找，让意图匹配文件尾的
  patch 落在尾部；且换行模式为 `PreserveLineEndings` 时用 `eof_start.max(start)` 防回退。
- 防御性早返回有回归背景：`pattern.len() > lines.len()` 直接 `None`，注释写明
  "avoids out-of-bounds panic that occurred pre-2025-04-12"（`seek_sequence.rs:23-29`）。
- 另有 `streaming_parser.rs`（924 行）支持边流式接收边解析 patch。

### claude-code（文档推断，证据均为 CHANGELOG 行号）

- **read-before-edit 是命名的独立检查**，且被逐步放宽：单文件 `grep`/`egrep`/`fgrep` 也算满足
  （`CHANGELOG.md:1395`），`head`/`tail` 也算（`:1700`）；新模型的 Write 可覆盖本会话未读过的
  已有文件（`:135`）。
- **过期策略是务实的**：曾修过"文件在读之后被改动、但目标文本仍唯一匹配时 Edit 却失败"
  （`:625`）——即**唯一匹配即放行**，不做硬性版本拦截。
- **换行保真是明确契约**：修过 Windows 上 Edit/Write 把 CRLF 写成双份（`:2862`）、
  Write 覆盖 CRLF 文件时静默转换换行（`:3204`）；新建文件按所在目录推断 CRLF。
- **字节级保真**：修过 Edit 把弯引号静默改成直引号（`:3799`）、`trimEnd()` 吃掉有意的尾部空行
  （`:3733`）。
- 其他写路径契约：尊重 umask 而非硬编码 0o600（`:4291`）、>1GiB 文件 Edit OOM（`:2889`）、
  diff 计算 5 秒超时降级（`:3031`）、Perforce 模式下只读文件报 `p4 edit` 提示而非静默覆盖（`:2652`）。
- 权限规则名以 `Edit(path)` 为写侧唯一入口，`Write(path)`/`NotebookEdit(path)` 不是合法前缀
  （`:573`）。
- 无文档证据项（已核实为零命中，不要臆造）：`replace_all` 参数、MultiEdit 的顺序/原子性/回滚语义、
  NotebookEdit 的 `cell_id`/`edit_mode`/`cell_type` 参数名、BOM 处理。

### opencode

- `internal/llm/tools/edit.go`（489 行）+ `patch.go`（372 行）双原语。
- `replaceContent`（`edit.go:372`）的检查顺序值得抄：目录判断 → **先读后写**
  （`getLastReadTime` 为零则拒）→ **mtime 过期检测**（`modTime.After(lastRead)` 则拒，
  且报错里给出两个具体时间）→ 唯一性（`Index != LastIndex` 则要求更多上下文）→
  **空操作检测**（新旧内容相同则报错，避免浪费一轮）→ 生成 diff → 请求权限。
- 匹配本身是裸 `strings.Index`，无容错阶梯。

### deepcode-cli —— 过期防护做得最彻底

- `snippet_id` 是**必填**参数（`edit-handler.ts:61` `z.string().min(1)`），
  且校验 snippet 归属：不属于所给 `file_path` 就拒（`:115`）。
- 版本追踪在 `common/state.ts`：`fileVersionsBySession: Map<sessionId, Map<path, version>>`
  （`:39`），过期判定 `getFileVersion(...) > snippet.fileVersion`（`:290`）。
- 不唯一时的报错直接给出三条补救路径：
  `"old_string is not unique; use snippet_id, replace_all, or provide more context."`（`:276`）。

### grok-build（同源 fork，最可比）

- 也是 `search_replace` 单原语——**ccode 的工具形状继承自 grok-build，不是继承自 codex**。
- 其 pager 侧对工具名做了归并 `"search_replace" | "edit" | "apply_patch" | "strreplace"`
  （`xai-grok-pager/src/scrollback/blocks/tool/mod.rs:468`），说明预留了多名兼容，但后端仍只有一种。

## 1.3 ccode 现状（逐项三态 + 证据）

**主线暴露给模型的工具共 13 个**，权威定义在 `ccore/src/tools/bridge.rs`：
`bash` / `read_file` / `write_file` / `search_replace` / `grep` / `list_dir` / `web_search` /
`web_fetch` / `task` / `lsp` / `glob` / `skill` / `tool_search`。
执行器在 `ccore/src/tools/builtin.rs`：`WriteExecutor:373`、`EditExecutor:458`。

| 能力 | 状态 | 证据 |
|---|---|---|
| str-replace 编辑 | ①主线 | `builtin.rs:458-550`，`name()` = `search_replace`（`:487`）与 `bridge.rs:373` 一致 |
| 唯一性约束 | ①主线 | `builtin.rs:516-525`，`count>1` 报错并带命中数 |
| 先读后写 | ①主线 | `builtin.rs:477`（edit）、`:402`（write，仅文件已存在时） |
| 原子写入 | ①主线 | `builtin.rs:418-424`、`:602-612`，临时文件 + rename |
| unified diff + 编辑点上下文 | ①主线 | `builtin.rs:529-549` |
| rustfmt 编辑后检查 | ①主线 | `tools/rustfmt_hook.rs:20` 匹配 `write_file`/`search_replace` |
| 路径越界防护 | ①主线 | `builtin.rs:466-474`、`tools/path_validator.rs` |
| `mode=insert` / `mode=write` | **实现在但模型不可达** | `builtin.rs:479-484` 有三分支；但 `bridge.rs:409-417` 的 schema 只声明 `path`/`search`/`replace` |
| `replace_all` 参数 | **实现在但未声明** | `builtin.rs:507` 读它；schema 里没有。讽刺的是 `:522` 的报错正在教模型"设置 replace_all=true" |
| 匹配容错阶梯 | ③缺失 | `builtin.rs:510/516` 裸 `content.matches(old_text)`，无任何归一化 |
| 文件版本/过期检测 | ③缺失 | `read_tracker.rs:16` 是 `Mutex<HashSet<String>>`，只存路径，无 mtime/hash |
| 多 hunk / 多文件原子编辑 | ③缺失 | 无 apply_patch / MultiEdit 类工具 |
| 换行/BOM 保真 | ③缺失 | 写路径直接写模型给的字符串 |

**写工具在主线是串行的**（`node/tool.rs:156-164` `can_spawn_concurrent` 要求
`entry.read_only && !needs_confirmation`；`write_file`/`search_replace` 都是
`read_only: false, requires_confirmation: true`），只读工具才进 `JoinSet` 并发（`:170-194`，
Semaphore(20)）。这限制了下文 P1-1 的触发面，但没有消除它。

## 1.4 差距清单

### P0-1：`search_replace` 零容错 —— CRLF 文件上必然失败

`builtin.rs:510/516` 用裸 `content.matches(old_text)`。`read_to_string` 保留 `\r\n`，
而模型基于渲染后的内容生成 `search` 时给的是 `\n`，子串匹配必不命中。
**后果**：CRLF 仓库（Windows 开发、或含 CRLF 文件的混合仓）里 `search_replace` 整体不可用，
模型会反复"未找到搜索文本"，最终退化为 `write_file` 全量重写——而全量重写恰恰是风险最高的路径。
同理，行尾多余空白、缩进差一格、源码里有弯引号/en-dash，全都硬失败。
codex 的四级阶梯正是为此存在。

### P0-2：schema 声明与实现相反 —— 模型被文档误导，且三个参数不可达

`bridge.rs:405` 的工具描述写 **"如果搜索文本在文件中出现多次,会替换所有匹配项"**，
而 `builtin.rs:520-525` 的实现是**多处匹配直接报错**。方向相反。
同时 `mode`（insert/write）、`replace_all`、`after_line`、`content` 四个参数实现里都读，
schema（`bridge.rs:409-417`）里一个都没声明。
**后果**：① 模型照描述构造短 search 串期望全替换 → 报错 → 浪费一轮；
② `insert` 模式（按行号插入，本来能省掉"读+拼全文"）模型永远不会用；
③ 报错文案教模型用一个未声明的参数，是否生效取决于模型愿不愿意猜。
这与前序记录的 [ccode-tool-layer-contract] 是同一类缺陷：**bridge.rs 是权威，但权威本身写错了**。

### P1-1：`with_extension("tmp")` 吃扩展名 —— 无需并发即可丢数据

`builtin.rs:418`（write_file）与 `:603`（search_replace 的 `atomic_write`）都用
`path.with_extension("tmp")`。`with_extension` 是**替换**最后一段扩展名，不是追加：
`data.json` → `data.tmp`，`Cargo.toml` → `Cargo.tmp`。
**后果**：仓库里若已有同名 `.tmp` 文件（`data.tmp` 与 `data.json` 并存很常见），
写 `data.json` 会先覆盖掉 `data.tmp`，再把它 rename 走 —— 用户的 `data.tmp` **静默消失**，
**不需要任何并发**。另外 `TempFileGuard` 在失败路径会删除该文件，同样命中用户文件。
（已排除并发撞名这条路：子 agent 与主 Agent **共用同一个 ToolNode**
（`ccore/src/agent/subagent.rs:6`），写工具在该 ToolNode 内全局串行，
所以"两个 agent 分别写 `a.rs`/`a.toml` 撞同一个 `a.tmp`"不成立。）
**本仓已经踩过并修过这个完全相同的 bug**：`ccode-update/tests/test_concurrent_convergence.rs:28`
的注释明文写着 `with_extension("tmp")` eats everything after the last dot，
`auto_update.rs:2553` 也记录了修复。修法在仓内已有先例，直接照搬。
同类未修点还有 `ccode-mcp/src/credentials.rs:203`、`ccore/src/persistence/file_storage.rs:47`。

### P1-2：先读后写只判"读过没"，不判"版本对不对"

`read_tracker.rs` 是 `HashSet<String>`，`require_read` 只查存在性（`:65-74`）。
**后果**：模型读了文件 → 用户/其他进程改了该文件 → `write_file` 全量覆盖仍被放行，
用户的改动被静默吞掉。`search_replace` 因为要求精确匹配而有天然的部分保护
（改动过的地方匹配不上就失败），但 `write_file` 完全没有防护。
opencode 用 mtime（`edit.go:390-397`）、deepcode 用 per-session fileVersion（`state.ts:290`），
两条路都比现状强。这正是 CCODE-004 计划里"B2 Snippet 版本追踪编辑"要解决的问题，**从未实施**。

### P1-3：read_tracker 以模型给的原始路径字符串为 key，路径别名会误拦

`builtin.rs:318` 用 `mark_file_read(path)`（原始参数），`:477`/`:402` 也用原始 `path` 校验，
而真正的 IO 用的是 `validation.canonical`。口径**在两端一致所以能工作**，
但 key 是未规范化的字符串。
**后果**：模型先 `read_file("src/main.rs")`，再 `search_replace("/Users/.../src/main.rs")`
或 `"./src/main.rs"` → 被判"未读过"而拒绝，白掉一轮。
另有第二份账本：`node/thinker/mod.rs:604` 里 ThinkerNode 持有独立的 `ReadTracker` 实例并 `mark_read`，
但编辑路径查的是 `builtin.rs` 里那个全局静态实例（`read_tracker.rs:93`）——两本账互不相通。

### P2-1：无多 hunk 原子编辑

一次重构要改 8 处，就是 8 轮 `search_replace` 往返（每轮都带完整 diff 输出回上下文）。
codex 一个 patch 搞定，claude-code 有 MultiEdit，opencode 有 `patch.go`。
**后果**：token 成本与延迟成倍，且中途失败会留下半改状态（无回滚）。

### P2-2：`read_file` 拒读被 .gitignore 忽略的文件 —— 六家里只有 ccode 这么做

`builtin.rs:297-306`：命中 gitignore 直接 `Err("文件 X 被 .gitignore 忽略")`。
**后果**：无法查看构建产物排错、无法读本地配置（`.env.local`、`config.local.toml`）、
无法读 `target/` 下的生成代码。挡密钥的意图可以理解，但手段过宽——
`.gitignore` 的语义是"不进版本库"，不是"不可读"。各家都不这么做。
建议改为：默认可读但对疑似密钥文件（`.env*`、`*.pem`、`id_rsa*`）走权限确认或脱敏。

### P2-3：换行与编码保真无任何处理

写路径直接落模型给的字符串。CRLF 文件被改成 LF、尾部空行被吃、BOM 丢失都不会被发现。
claude-code 在这类问题上修过至少 4 个 bug（`:2862`/`:3204`/`:3733`/`:3799`），
codex 直接做成 `PreserveLineEndings`/`NormalizeToLf` 双模式。

## 1.5 可落地改法（均指向主线文件）

| 差距 | 接线点 | 改法 |
|---|---|---|
| P0-2 | `ccore/src/tools/bridge.rs:371-422` | **先改声明**（最低成本、最高收益）：把描述里"会替换所有匹配项"改为"必须唯一匹配，否则报错"；把 `mode`/`replace_all`/`after_line`/`content` 补进 `parameters`。零逻辑改动即可解锁已实现的 insert 模式 |
| P0-1 | `ccore/src/tools/builtin.rs:492-527` | 在 `execute_replace` 里加容错阶梯。可直接移植 codex `seek_sequence.rs` 的四级思路，但因 ccode 是子串（非按行）匹配，建议改为：先按行切分双方 → 复用四级比较 → 命中后按行还原。同时在读取后记录原文换行风格，写回时还原 |
| P1-1 | `builtin.rs:418`、`:603` | 改为 `OsString` 追加而非替换扩展名（`format!("{}.tmp.{}", 原名, 进程id/随机)`），照 `ccode-update/src/auto_update.rs` 已有修法。同时顺手修 `ccode-mcp/src/credentials.rs:203`、`ccore/src/persistence/file_storage.rs:47` |
| P1-2 | `ccore/src/tools/read_tracker.rs` | `HashSet<String>` → `HashMap<PathBuf, FileStamp{mtime, len, hash?}>`；`mark_read` 时记快照，`require_read` 时比对。策略取 claude-code 的务实版：**戳记不符时，若 search 文本仍唯一匹配则放行**，仅对 `write_file` 全量覆盖硬拦 |
| P1-3 | `builtin.rs:318/402/477` | key 统一改用 `validation.canonical`（三处同改，否则会全量误拦）。另外决定 `thinker/mod.rs:604` 那份第二账本的去留——要么删，要么让编辑路径查它 |
| P2-1 | `bridge.rs` 新增工具 + `builtin.rs` 新增 executor | 加 `apply_patch`（建议直接采用 codex 的 V4A 文法，模型侧已有大量训练语料），executor 内做"全部 hunk 先在内存里试算成功再落盘"以获得原子性。注意 `name()` 必须与 `bridge.rs` 声明逐字一致 |
| P2-2 | `builtin.rs:297-306` | 从"拒读"改为"可读 + 敏感文件名走权限确认"。gitignore 过滤保留给 `grep`/`glob`（那里过滤是对的） |
| P2-3 | `builtin.rs` 读写两端 | 读时探测 EOL/BOM 并记入 read_tracker 的戳记，写时还原 |

**建议实施顺序**：P0-2（改声明，几乎零风险）→ P1-1（照仓内先例修，风险低收益明确）→
P0-1（容错阶梯，改动最大但直接决定编辑成功率）→ P1-2/P1-3（并做，都在 read_tracker）→ P2-*。

## 1.6 不确定项

- claude-code 全列均为**文档推断**。`replace_all`、MultiEdit 原子性、NotebookEdit 参数名在该仓
  零命中，不做结论。
- `mode=insert` 究竟有没有被模型误撞用过——未查会话日志，无法判断实际发生率。
- P1-1 的并发触发面已排除（子 agent 共用 ToolNode，写串行），但**未验证**若将来把写工具改为
  可并发、或引入独立 exec 进程后是否会重新暴露。

---

# 维度二：会话持久化 / 恢复 / 回溯

## 2.1 对比矩阵

| | 落盘格式 | 恢复入口 | 会话索引/列表 | Fork/分支 | 回溯（rewind/revert） | 编辑 checkpoint | GC/压缩 | 跨目录恢复 |
|---|---|---|---|---|---|---|---|---|
| **codex** | JSONL rollout + **SQLite `state_db`** | `resume_picker` + `session_resume`（TUI 交互选择器） | ✅ `session_index.rs` + `rollout_reference_index` | ✅ `SessionSelection::Fork` | ⚠️ 未核实 | ⚠️ 未核实 | ✅ **`maintenance.rs` + `compression.rs`**，带进程级文件锁 + 持久 run marker 节流 | ✅ `resolve_cwd_for_resume_or_fork` + `tui.resume_cwd` 三态配置 |
| **deepcode-cli** | 未核实 | ✅ **最丰富**：`--resume [id]` / `--resume`（弹选择器）/ `--last`\|`-l` / `--fork` / `/resume` / `/continue` | ✅（`--resume` 裸用即弹 picker） | ✅ `--fork` | ⚠️ 未核实 | ⚠️ 未核实 | ⚠️ 未核实 | ⚠️ 未核实 |
| **opencode** | **SQLite**（`internal/db/sessions.sql.go`） | ✅ TUI `dialog/session.go` 选择器 | ✅ `service.List()` | ⚠️ 有 `CreateTaskSession`（父子会话），非用户级 fork | ⚠️ 未核实 | ⚠️ 未核实 | ✅ `service.Delete()` | ⚠️ 未核实 |
| **grok-build** | JSONL + `xai-grok-session-search`（SQLite FTS，含 `bootstrap.rs`/`recovery.rs`） | ⚠️ 未核实（有 `recovery.rs` 迹象） | ✅ FTS 全文检索会话历史 | ⚠️ 未核实 | ⚠️ 未核实 | ⚠️ 未核实 | ⚠️ 未核实 | ⚠️ 未核实 |
| **claude-code** | 【文档】JSONL | 【文档】`--resume` / `--continue` / `/resume` | 【文档】有 | 【文档】有 | 【文档】`/rewind` | 【文档】有 | ⚠️ 未核实 | 【文档】有 |
| **harness-cli** | N/A（非 coding agent） | N/A | N/A | N/A | N/A | N/A | N/A | N/A |
| **ccode（主线）** | **双写**：`conversation.jsonl`（追加）+ `conversation.json`（每条消息全量重写） | ❌ **③ 函数存在，零调用者，CLI 无 flag** | ❌ 无 | ❌ 无 | ❌ 无 | ❌ **③ `CheckpointManager` 零调用** | ❌ 无 | ❌ 无 |

## 2.2 codex 的 rollout 子系统（本维度最值得抄的一家）

`codex-rs/rollout/` 是**独立 crate**，21 个源文件，模块划分：

- `recorder.rs` —— 自述 "Persist Codex session rollouts (.jsonl) so sessions can be replayed or
  inspected later."
- `state_db.rs` —— SQLite 状态库（`core/src/mcp_tool_call.rs:73` 引用了它，说明工具调用也进库）
- `session_index.rs` + `rollout_reference_index_tests.rs` —— 会话索引，支撑列表/检索
- `reverse_jsonl_scanner.rs` —— **从尾部反向扫 JSONL**。这是恢复大会话的关键：不需要读完整个文件就能拿到
  最近 N 条。ccode 的 `load_session_jsonl`（`state.rs:192`）是 `read_to_string` 整文件读。
- `compression.rs` + `maintenance.rs` —— 旧 rollout 压缩与 legacy 迁移。`maintenance.rs:1-8` 的注释交代了
  三层锁的边界：**进程级非阻塞文件锁**（防两个 maintenance job 同时 rename 覆盖同一路径）、**per-thread
  writer 锁**（保护活跃 appender）、**compression 的持久 run marker**（节流扫描频率）。这是"长期使用后
  会话目录不会无限膨胀"的完整方案。
- `policy.rs` / `ordinal.rs` / `metadata.rs` / `model_context.rs` / `config.rs` / `sqlite_metrics.rs`
- 错误路径单独成文件：`core/src/session_rollout_init_error.rs`，把 IO 错误映射成对用户可读的 `CodexErr`

TUI 侧：`tui/src/resume_picker.rs`（交互选择器，`SessionSelection::Resume` / `::Fork`，
`lib.rs:790-793`）+ `tui/src/session_resume.rs`。**跨目录恢复被当成一等问题处理**：
`resolve_cwd_for_resume_or_fork`（`lib.rs:813`）+ `tui.resume_cwd` 配置 + `ResumeCwdMode::Current` 时对
远程 workspace 强制要求 `--cd`（`lib.rs:800-806`）。

## 2.3 deepcode-cli 的 CLI 契约（最完整的 flag 设计）

`packages/cli/src/cli-args.ts` 把 resume 的三态语义写进了类型注释（`:29-33`）：

```
resume: string | true | undefined
  undefined — --resume was not used
  true      — --resume was used without a session ID (show picker)
  string    — --resume <sessionId> was used
```

配套 `--last`/`-l`（恢复当前项目最近一个会话，`:40`）、`--fork`、以及交互命令 `/resume`、`/continue`
（`:69-71`，`/continue` 语义是"继续当前会话，若为空则挑一个恢复"）。

**互斥校验写得很细**（`:123-139`）：裸 `--resume` 与 `--prompt` 冲突并给出修正建议、`--last` 与
`--resume` 冲突、`--fork` 与 `--resume` 冲突、`--resume <id>` 校验 UUID 格式并在报错里给出示例。
这套错误信息质量是 ccode 目前完全没有的参照标准。

## 2.4 ccode 现状（逐项三态 + 证据）

| 能力 | 级别 | 证据 |
|---|---|---|
| JSONL 追加写入 | **① 真接线** | `thinker/mod.rs:791` 发 `Topic::new("state/persist")`（订阅声明在 `:2215`）→ `StateNode::handle_message`（`state.rs:436`，`"state/persist"` 分支）→ `append_jsonl`（`:454`）。`StateNode` 主线构造于 `launcher.rs:134` |
| conversation.json 全量写 | ① 真接线**但有严重缺陷** | `state.rs:456` `self.persist().await?` → `:138-140` `to_string_pretty` + `std::fs::write`。**每条消息重写整个文件** |
| 会话恢复 | **③ 函数存在，零调用者** | `state.rs:186` `load_session_jsonl` / `:383` `load_session` —— 全仓无任何调用者（`:189` 是 `load_session_jsonl` 内部自调）。其余 `.load_session(` 命中全部属于 `ccode-pager`/`ccode-shell`/`ccode-acp` 各自不同的类型 |
| CLI 恢复入口 | **③ 完全缺失** | `ccode-cli/src/main.rs` 的 Args 共 11 个 flag：`-p/--prompt`、`-m/--model`、`--agent`、`--permission`、`--max-turns`、`--reasoning-effort`、`--config`、`--output-format`、`--router-addr`、`--pub-addr`、`--work-dir`。**没有 `--resume`/`--continue`/`--session-id`/`--last`/`--fork`** |
| session_id 稳定性 | **③ 每次启动全新 UUID** | `state.rs:93` `let session_id = uuid::Uuid::new_v4().to_string();` 在 `StateNode::new` 里无条件生成，**无任何外部注入通道** |
| 会话索引/列表 | **③ 缺失** | `~/.ccode/sessions/<uuid>/` 无索引文件、无 SQLite、无列表命令 |
| Fork / 回溯 | **③ 缺失** | 主线无对应概念 |
| 编辑 checkpoint | **③ 死代码（注释与事实相反）** | `ccore/src/tools/checkpoint.rs:27` `CheckpointManager`（git stash / commit 双模式，`:61-62`），文件头注释 `:1` 写「每次文件编辑前自动 checkpoint，支持回滚」。**`CheckpointManager` 全仓零生产引用**（仅 `tools/mod.rs:15` 的 `pub mod`）。`FileTransaction` 同样零引用（仅 `tools/mod.rs:39` 的 `pub use`）。所有 `rollback` 命中都是别的东西：`file_transaction.rs` 自身测试、`goal_tracker` 的分类器重试、`ccode-config` 的 `rollback_floor`、`ccode-update` 的 `.rollback.bak`、`ccode-memory` 的 dream lock |
| GC / 压缩 | **③ 缺失** | 无任何清理路径。每跑一次 ccode 就在 `~/.ccode/sessions/` 留一个 uuid 目录，永久累积 |
| 存储路径 | ① 但**跨平台破损** | `state.rs:106-107` `std::env::var("HOME").unwrap_or_else(\|_\| ".".into())`。Windows 通常无 `HOME` → fallback 到 `"."`，会话写进**当前工作目录** |

## 2.5 差距清单

### P0

**P0-12　会话「只写不读」——持久化功能整体是单向的**
每一轮对话都在往 `~/.ccode/sessions/<uuid>/` 写两个文件，**没有任何路径把它读回来**。恢复函数
（`state.rs:186,383`）实现完整、零调用者；CLI 11 个 flag 里没有一个能触发恢复。
用户可感知后果：ccode 崩溃 / 误按 Ctrl-C / 终端关闭 → 整个会话**不可恢复**，只剩磁盘上一份人类可读但
产品读不回来的 JSONL。这是与全部 5 家可比参照（codex/deepcode/opencode/grok-build/claude-code）的**唯一
一项全员有、ccode 全无**的能力。

**P0-13　`session_id` 每次启动都是新 UUID，且无外部注入通道**
`state.rs:93` 无条件 `Uuid::new_v4()`。即使把 `load_session_jsonl` 接上，**也没有办法告诉它要恢复哪一个**——
没有 `--session-id`、没有 `--last`、没有索引可查、目录名是无意义的 uuid。所以 P0-12 的修复必须连带
改这里，否则接线了也用不了。

**P0-14　`CheckpointManager` 与 `FileTransaction` 双双死代码，编辑无任何安全网**
`checkpoint.rs:1` 的注释明文承诺「每次文件编辑前自动 checkpoint，支持回滚」，实现（git stash/commit
双模式）写好了，**零调用**。
用户可感知后果：模型一次错误的 `write_file` 全量覆盖（配合维度一 P0-1 的「`search_replace` 在 CRLF 文件上
必然失败 → 退化成 write_file」）会**不可挽回**。这条与维度一的 P0-1/P1-2 是同一个故障链上的三环，
应合并评估优先级。注释与事实相反这一点本身也是 review 陷阱。

### P1

**P1-15　`conversation.json` 每条消息全量重写 —— O(n²) 写放大**
`state.rs:456` → `:138-140`：每收到一条 `state/persist` 就 `to_string_pretty` 整个会话再 `fs::write`。
一个 300 轮的会话要写 300 次、最后一次写的是完整 300 轮的 pretty JSON。
用户可感知后果：长会话后期每轮都有可感知的磁盘停顿；SSD 写入量约为实际数据量的 n/2 倍。
jsonl 追加写是对的——**`conversation.json` 这一路整体是冗余的**，应删或改为退出时写一次。

**P1-16　两个落盘文件内容不一致，且 `load_session` 读的是降级的那个**
`conversation.json` 来自 `short_term.all_entries()`，而 `ShortTermMemory` 有
`DEFAULT_MAX_ENTRIES = 500`（`short_term.rs:120`）并在 `:151` `while len >= max` 驱逐；
`handle_compact`（`state.rs:245-270`）还会**原地截断** `short_term` 里的工具输出。
所以 `conversation.json` 是**双重降级**的（截断到 500 条 + 压缩后的内容），而 `conversation.jsonl` 是完整
的原始流。偏偏 `load_session`（`:383`）读的是 `conversation.json`。
用户可感知后果：如果按现状把恢复接上，恢复出来的会话会**静默丢失前期消息、且工具输出是压缩后的残缺
版本**。修 P0-12 时必须选 jsonl 那一路。

**P1-17　`~/.ccode/sessions/` 无 GC，无限膨胀**
无任何清理路径。对比 codex 有整个 `maintenance.rs` + `compression.rs`，还专门用进程级文件锁 + 持久 run
marker 处理"压缩与迁移不能同时 rename 同一路径"的并发问题（`maintenance.rs:1-8`）。
用户可感知后果：重度使用数月后 `~/.ccode/sessions/` 下数千个 uuid 目录，用户无从判断哪个能删。

**P1-18　`HOME` 直读，Windows 上会话写进当前工作目录**
`state.rs:106` `std::env::var("HOME").unwrap_or_else(|_| ".".into())`。Windows 用 `USERPROFILE`，`HOME`
通常不存在 → 静默 fallback 到 `"."`。
用户可感知后果：Windows 上每个项目目录里都会长出一个 `.ccode/sessions/`，并可能被提交进 git。
应改用 `dirs`/`directories` crate（仓内其他地方是否已有此依赖需确认）。

### P2

**P2-19　恢复用整文件读而非反向扫描**：`state.rs:192` `read_to_string` 整个 jsonl。对比 codex 专门写了
`reverse_jsonl_scanner.rs` 从尾部反向读，恢复大会话时不必载入全文。这条在 P0-12 修好之前不会被感知到。

**P2-20　无跨目录恢复语义**：codex 把这当一等问题（`resolve_cwd_for_resume_or_fork` +
`tui.resume_cwd` 三态 + 远程 workspace 强制 `--cd`）。ccode 连 resume 都没有，此项目前不构成实际损害，
但设计 P0-12 时应一并考虑，否则会像 codex 那样后期补丁化。

**P2-21　CLI 错误信息质量差距**：deepcode 的 `cli-args.ts:123-139` 为 resume 相关 flag 写了 5 条互斥校验，
每条都给修正建议（如"Use `--resume <sessionId> -p <prompt>`"）并校验 UUID 格式。这是实现 P0-12/P0-13 时
可直接照抄的参照。

## 2.6 可落地改法（主线接线点）

按依赖顺序，前三步是一个不可分割的最小可用单元：

1. **`ccode-cli/src/main.rs` Args 加 flag**：`--resume [SESSION_ID]`、`--continue`/`-c`、`--last`。
   照 deepcode 的三态语义（`cli-args.ts:29-33`）：裸 `--resume` = 列出可选会话。
2. **给 `StateNode` 开注入通道**：`state.rs:91` `StateNode::new` 目前无条件生成 uuid。加
   `StateNode::with_session_id(id)` 或让 `new` 接受 `Option<String>`，并从 `KernelConfig` 透传到
   `launcher.rs:134` 的构造点。
3. **在 `launcher.rs` 构造后调用恢复**：`StateNode::load_session_jsonl`（`state.rs:186`）已实现，
   **选 jsonl 这一路，不要用 `load_session`**（理由见 P1-16）。
4. **加会话索引**：最小方案是 `~/.ccode/sessions/index.jsonl`，每次启动追加一行
   `{session_id, cwd, created_at, first_user_message}`。够支撑 `--last` 与列表选择器，不需要 SQLite。
5. **接 `CheckpointManager`**：`ccore/src/tools/builtin.rs` 的 `EditExecutor::execute`（`:458`）与
   `WriteExecutor::execute`（`:373`）入口处各调一次 `create_checkpoint`。这是 P0-14 的最小修法，
   实现已在 `checkpoint.rs:52-65`。
6. **删掉 `conversation.json` 这一路**（P1-15/P1-16 一并解决），或降级为退出时写一次快照。
7. **`state.rs:106` 换成 `dirs`/`directories`**（P1-18）。

## 2.7 不确定项

- **deepcode-cli 的落盘格式、GC、回溯**只核实了 CLI flag 层（`cli-args.ts`），未追到存储实现。
- **codex 是否有编辑 checkpoint / rewind**未核实——我只确认了 rollout 子系统与 resume/fork，
  `maintenance.rs`/`compression.rs` 的注释读了但正文未通读。
- **grok-build 的 `recovery.rs`/`bootstrap.rs` 具体做什么**未读，恢复能力标为未核实。
- **claude-code 全列为文档推断**，`/rewind` 等能力来自公开文档描述而非代码。
- **opencode 的 revert/回溯**未找到，`internal/session/session.go` 只有 CRUD + 父子会话
  （`CreateTaskSession`/`CreateTitleSession`），可能在别处。
- **`state/persist` 的发布是否覆盖所有消息类型**（如 assistant 消息、工具结果）未逐一核对，
  只确认了 `thinker/mod.rs:791` 这一个发布点存在。
- **仓内是否已有 `dirs`/`directories` 依赖**未查，影响 P1-18 的改法成本。

# 维度三：代码检索 / repo map / 上下文自动发现

`harness-cli` 经核实**不是 coding agent**（Harness 平台 devops CLI，`README.md:3` 自述 "unified,
spec-driven CLI for the entire Harness platform"，`modules/` 下无 LLM/tool-loop 代码），本维度整体 N/A。

## 3.1 对比矩阵

| | 文本搜索 | 文件名/glob | AST 符号 | 语义向量 | 持久索引 | repo map 自动注入 | 项目指令发现 | 结果裁剪 | gitignore | 检索↔记忆 |
|---|---|---|---|---|---|---|---|---|---|---|
| **codex** | ✅ 模型自己 shell 调 rg | ✅ 独立 crate：`ignore` walker + `nucleo` 模糊匹配 + 后台 session | ⚠️ 有 tree-sitter 但只解析 bash 命令/patch，非符号索引 | ❌ | ❌ | ❌ 不注入；`/init` 让模型自己写 AGENTS.md | ✅ 最完整：git root 向下逐层 + `AGENTS.override.md` + 字节预算 | ✅ limit 默认 20、doc 32KiB 截断 | ✅ `require_git(true)` 可配 | ✅ world_state 18 段动态重锚 |
| **grok-build** | ✅ rg | ✅ `xai-fuzzy-file-search`（ignore+nucleo+后台 daemon） | ⚠️ tree-sitter 仅用于 shell 命令拆解（权限判定） | ⚠️ sqlite-vec cosine **仅用于记忆去重** | ⚠️ `xai-grok-session-search` 是**会话历史 FTS**，非代码索引 | ❌ | ✅ | ✅ | ✅ | ✅ 记忆语义去重接线 |
| **opencode** | ✅ `grep` 工具调 rg，缺 rg 直接报错 | ✅ `glob` 调 rg，失败降级 `doublestar` | ⚠️ 无 AST，但有 LSP `diagnostics` 独立工具 | ❌ | ❌ | ⚠️ 无自动注入；`ls` 工具按需生成文件树 | ✅ 12 项 `defaultContextPaths`（含 `.cursorrules`/copilot） | ✅ 各 100 条 + **`truncated` 标志回传模型** | ✅ | ⚠️ 有 summarize，检索结果不单独入记忆 |
| **deepcode-cli** | ⚠️ **无 grep 工具**，只能 `bash` 调 rg | ⚠️ 同上，无 glob 工具 | ❌ | ❌ | ❌ | ❌ | ✅ 三层：`~/.deepcode/AGENTS.md` + `./.deepcode/AGENTS.md` + `./AGENTS.md` | ✅ read 2000 行 / 单行 2000 字符 | ⚠️ 依赖 rg 自身 | ⚠️ 未见 |
| **harness-cli** | N/A | N/A | N/A | N/A | N/A | N/A | ⚠️ 根目录有 `AGENTS.md`，是**被** agent 读 | N/A | N/A | N/A |
| **claude-code** | ✅ Grep【文档明证】 | ✅ Glob【文档明证】 | ❌【推断】 | ❌【推断】 | ❌ 明确无索引【推断】 | ❌【推断】 | ✅ enterprise/user/project/子目录 + `@import` + `CLAUDE.local.md` | ✅ Read 2000 行【明证】 | ✅ 受 permissions deny 约束【明证】 | ✅ /compact + Explore subagent 隔离检索上下文 |
| **ccode（主线）** | ✅ `grep` 调 rg（有 `grep -rn` fallback） | ⚠️ `glob` 调 **`find`**，非 rg；gitignore 事后过滤 | ❌ | ❌ **④ 恒空转** | ❌ **④ 索引恒空** | ❌ **③ RepoMap 零构造** | ⚠️ 仅 CCODE.md/CLAUDE.md **单目录**；AGENTS.md 代码存在但 **③ 主线零消费** | ⚠️ rg 200 行**静默**截断；`list_dir` **无上限** | ⚠️ `read_file` 被 gitignore **拒读**（六家唯一） | ✅ EpisodicMemoryBridge 真接线（关键词，非向量） |

## 3.2 横向结论（本维度最重要的一条）

> **6 款参照 agent 中，没有任何一款对代码做语义向量检索或持久符号索引。** 全部是 glob + ripgrep 的
> agentic 搜索。出现向量/FTS 的两处（grok-build 的 sqlite-vec、session FTS）服务的是**记忆去重**和
> **会话历史检索**；tree-sitter 出现的两处（codex apply-patch、grok-build 权限判定）都是**解析 shell
> 命令**，不是符号提取。

## 3.3 各家关键实现

**codex**
- `file-search/` 是**文件名**模糊搜索而非内容搜索：`ignore::WalkBuilder` + `nucleo::Nucleo` 增量匹配，
  带取消标志与后台 session（`file-search/src/lib.rs:7,11-12,143,164-200`）；默认 `limit=20`、
  `threads=2`、`respect_gitignore=true`（`:106-130`）。代码内容搜索完全交给模型 shell 调 rg。
- AGENTS.md 三步算法且有文档：向上走到 `project_root_markers`（默认 `.git`）→ 从 project root
  **向下**到 cwd 逐层收集 → 不越过 project root（`core/src/agents_md.rs:1-17`）。支持
  `AGENTS.override.md`、可配 fallback 文件名、分隔符 `--- project-doc ---`、256 路并发祖先探测
  （`:42-48,53,273`）。
- **字节预算硬约束**：`project_doc_max_bytes` 超出即 `data.truncate(remaining)` 并 warn
  （`agents_md.rs:68,169-176`、`config/mod.rs:4163`）。
- **world_state 是 codex 独门设计**：18 个 section 各自 snapshot + sha1 变更检测 + 能识别并替换历史
  fragment（`core/src/context/world_state/mod.rs:1-60`）。AGENTS.md 变更时主动发
  `"These AGENTS.md instructions replace all previously provided AGENTS.md instructions."`
  （`world_state/agents_md.rs:10-11`）。

**grok-build（与 ccode 同源，最可比）**
- `xai-fuzzy-file-search` 自述 "ignore-aware walker feeding a nucleo matcher, plus a **background
  daemon**"（`Cargo.toml:6,10,12`）——与 codex 同构，多了常驻 daemon。
- `xai-grok-session-search`（`db.rs`/`fts.rs`/`bootstrap.rs`/`recovery.rs`）是 SQLite FTS，索引对象是
  **会话历史**。向量能力用于记忆去重（`memory_flush.rs:198-247`，cosine 阈值 + sqlite-vec，不可用时优雅跳过）。
- **grok-build 有而 ccode 没有的检索 crate**：`xai-fuzzy-file-search`、`xai-grok-session-search`。
  ccode 的 nucleo 只散落在 `ccode-pager`/`ccode-workspace`/`ccode-shell`，无独立 crate、无 daemon。

**opencode**
- `grep` 硬依赖 rg，`exec.LookPath("rg")` 失败直接报错（`grep.go:208-220`）；`glob` 优先 rg，失败降级
  Go `doublestar`（`glob.go:137`）。
- 两者上限 100 条，超出置 `truncated=true` 并在输出里**显式提示模型收窄查询**
  （`grep.go:145,172-173,199-204`）——把裁剪信息回传给模型，优于静默截断。
- `view.go:35-36,54-60`：`MaxReadSize=250KB`、`DefaultReadLimit=2000` 行、单行超 2000 字符截断。
- 独门：`sourcegraph` 工具走 GraphQL 搜**公开仓库**代码（`sourcegraph.go:32,128,213`）。
- 上下文文件 12 项、覆盖竞品格式（`.cursorrules`/copilot/`CLAUDE.md`/`CLAUDE.local.md`）
  （`config.go:108-120`，装配在 `prompt.go:51-60`）。
- 注意：`internal/db/embed.go` 只是 `//go:embed migrations/*.sql`，**不是 embedding**，易误判。

**deepcode-cli（最极端的纯 agentic）**
- `tools/` 只有 12 个 handler，**没有 grep、没有 glob、没有 ls**。全仓 `grep`/`glob`/`ripgrep` 唯一命中是
  `prompt.ts:326` 的 `ripgrep: checkToolInstalled("rg")`——把「rg 是否可用」作为**环境事实写进 system
  prompt 的 JSON env block**（`:315-336`），由模型自己 `bash` 调。
- 彻底无索引：embedding/vector/tree-sitter/lancedb/hnsw 全零命中（含 package.json）。

**claude-code（闭源）**
- 【推断】无索引无 embedding：全仓 `embedding`/`vector`/`semantic search` 仅出现在 plugins README 与
  CHANGELOG，且非检索语义。取向 = **纯 agentic 搜索**。
- 【推断】用 Explore/Task subagent 做搜索以**隔离检索造成的上下文膨胀**——这是它替代索引的核心手段：
  把检索噪音关在子 agent 上下文里，只回传结论。

## 3.4 ccode 现状（逐能力三态 + 证据）

主线锚点复核：`ccode-cli/src/main.rs:133` `Kernel::new` → `kernel.run()` →
`ccore/src/kernel/mod.rs:427-437` `NodeLauncher::spawn_initial_set` → `launcher.rs:181`
`ThinkerNode::new`。**`launcher.rs` 确认在主线上**，它决定了哪些 setter 算真接线。

| 能力 | 级别 | 证据 |
|---|---|---|
| ripgrep 文本搜索 | **① 真接线** | `builtin.rs:658` `GrepExecutor`，`rg --line-number --max-count 200 --glob !.git`；rg 缺失 fallback `grep -rn`（`:725-747`） |
| glob | **① 但实现劣化** | `builtin.rs:763` 用 **`find -name`** 而非 `rg --files`；硬编码排除 `.git`/`node_modules`/`target`，再事后 `get_gitignore().should_skip()` 过滤（`:799-806`）。`find -name` 只匹配 basename、不解释 `**` |
| list_dir | ① | `builtin.rs:828` recursive 时 `max_depth` 默认 3；**不过滤 gitignore、无条目上限**（`:847` 直接 `entries.join("\n")`） |
| AST/符号搜索 | **③ 缺失** | `ccore/Cargo.toml:71` 只有 `regex`，无 tree-sitter/ignore/nucleo |
| 模糊文件名搜索 | **② 仅非主线** | `nucleo` 只在 `ccode-pager`/`ccode-workspace`/`ccode-shell`，`ccore` 无 |
| 语义向量检索 | **④ 接线成功恒空转** | 见 3.5 |
| 代码块索引 | **④ 索引恒空** | 见 3.5 |
| RepoMap | **③ 零构造** | `RepoMap::new` 全仓仅出现在自身测试（`memory/repo_map.rs:566,613,676,690`）。`set_repo_map`（`intent_retriever.rs:71`）唯一引用在非主线 `ccore_integration.rs`，而那个调用点所在的 `init_repo_map`（`:545`）**自身零调用者** |
| WorldState | **③ 完全孤儿（已复核）** | plain grep 全仓命中仅 `memory/mod.rs:42` 的 `pub mod world_state;` + 自身定义（`:71,77,83`）+ 4 个测试（`:187,200,214,228`）。**零生产调用者**。`world_state.rs:227` 测试名 `system_prompt_assembly` 说明它是 codex `world_state/` 的半移植——搬了数据结构，从未接到 prompt 装配 |
| CCODE.md/CLAUDE.md | **① 真接线但能力弱** | `launcher.rs:185` `thinker.load_project_context(&project_dir)` → `thinker/mod.rs:375-377` → `tools/project_context.rs:19-30`。**仅查单目录**（`dir.join("CCODE.md")`/`dir.join("CLAUDE.md")`），无向上遍历、无用户级、无 `@import`、**无字节上限无截断**（`:53-59` 原文 `format!("[项目规范]\n{}", self.content)`） |
| AGENTS.md | **③ 主线零消费（已复核）** | 仓内**三份实现**：`ccore/src/prompt/agents_md.rs:25`（完整，含向上到 git root + `format_agents_md_section` 截断提示 `:116`）、`ccode-agent/src/prompt/agents_md.rs:311`、`ccode-workspace/src/discovery.rs:70`。前者生产引用数 0（命中仅自身 4 测试 + `prompt/mod.rs:10` 的 `pub use`），唯一消费者 `ccore::prompt::PromptContext`（`prompt/context.rs:469`）**只在自身测试里构造过**（`:651,678,701,732,765,774`）。`ccode-agent` 那份不在依赖图内（`ccore`/`ccode-cli` 的 Cargo.toml 均无 `ccode-agent`；`ccode-cli` 只依赖 ccore/ccode-shell/ccode-hooks）。`ccode-workspace` 那份**确有调用者**（`hub_server.rs:591` 的 RPC handler），但主线 Thinker 只调 `load_project_context`，其结果**到不了 prompt** |
| 会话记忆自动注入 | **① 真接线** | `launcher.rs:188` `set_memory_bridge(EpisodicMemoryBridge::new(...))` 覆盖 `thinker/mod.rs:236` 的 `NoopMemoryBridge` 默认值；`memory_bridge.rs:33-40` `reconstruct_context` 是**关键词**检索。这是主线上唯一真正工作的自动上下文注入 |
| FTS/sqlite-vec/真 embedding | **② 仅非主线** | `ccode-memory/src/embedding.rs:104` `ApiEmbeddingProvider`（reqwest + 认证，真实可用）、`ccode-memory/src/index.rs`、`ccode-shell/.../search_fts.rs`。`ccore` 对这些**零依赖** |

## 3.5 「恒空转」完整证据链

**查询端——走通了，但恒返回空**
1. 主线调用点存在：`thinker/mod.rs:1207` `self.listen_enrichment(content);`，每轮用户输入真的执行。
2. `sensory.rs:59` `IntentRetriever::expand_intents(content)` 是纯规则的，**真的产出非空 intents**
   （`intent_retriever.rs:157-199`）。
3. `sensory.rs:71` `let query_embedding = vec![0.0f32; 1536];` —— 硬编码零向量，注释自认是占位（`:61-70`）。
4. `intent_retriever.rs:106-113`：`if query_norm == 0.0 { warn!(...); return Vec::new(); }`
   → **提前 return，100% 空**。且有测试锁死此行为：`:371-382`
   `zero_query_vector_returns_empty_not_garbage`。
5. 因此 `sensory.rs:73-80` 的 `[代码上下文]` 注入循环体**一次都不执行**。

**灌入端——索引恒空，两条路径都空**
6. 主线构造：`thinker/mod.rs:248` `IntentRetriever::new(EmbeddingIndex::new())`，新建空索引。
7. `register_blocks`（`intent_retriever.rs:81`）全仓**只有一个调用点**：非主线的
   `ccode-shell/src/session/ccore_integration.rs:541`。
8. 而 `ccore_integration.rs:540` 的 `pub fn register_code_blocks` **自身零调用者**（`pub` 不触发
   dead_code 警告，故 `cargo check` 干净——这正是方法论纪律要求交叉验证的原因）。
9. `set_repo_map` 同构：`expand_with_dependencies`（`:209-212`）永远走 `None => return Vec::new()`。

**净效果**：`listen_enrichment` 每轮都跑，每轮在日志打一条「意图检索收到零向量查询」warn，然后什么也不注入。
**两端各有一个必空开关，只修一端无效**。

## 3.6 差距清单

### P0

**P0-1　Context Engine 恒空转，`[代码上下文]` 自动注入事实上不存在**
宣称的语义检索零效果；每轮多一次无用意图展开；日志被 warn 刷满。更隐蔽的是**认知负债**——文档与注释
都在描述一个不存在的能力，后续开发者会基于「我们有语义检索」做决策。

**P0-2　AGENTS.md 发现在主线上完全缺失，且仓内躺着三份实现**
把一个标准 AGENTS.md 仓库（codex/deepcode 生态通用约定，连 `harness-cli` 根目录都放了）交给 ccode，
项目规范**一个字都不会被读取**。而 `ccore/src/prompt/agents_md.rs` 实现完好还带 4 个测试，会让 review
者误判已支持。

**P0-3　`CLAUDE.md`/`CCODE.md` 无向上遍历、无大小上限**
(a) 在子目录（如 `crates/codegen/ccore/`）启动，项目根 CLAUDE.md 直接丢失——**monorepo 的常态**。
(b) 一份 200KB 的 CLAUDE.md 被**原文全量**塞进上下文，无截断（codex 有 32KiB 硬预算 + truncate + warn），
可直接顶爆小上下文模型。

### P1

**P1-4　`glob` 用 `find -name` 而非 `rg --files`**
模型写 `src/**/*.rs` 会失败或行为怪异；gitignore 事后过滤意味着大仓里 `find` 已把 `target/` 全走完才
丢弃结果，慢且吃 IO。

**P1-5　`read_file` 拒读 gitignore 文件——六家唯一**
`builtin.rs:297-306` 直接 `return Err(...被 .gitignore 忽略)`。**没有任何一家这么做**：codex 只在
walker 里遵守 gitignore（决定"列不列"），读文件不拦；opencode `view.go` 无检查；deepcode
`read-handler.ts` 无；claude-code 的 Read 受 `permissions` deny 约束，是**用户显式配置**的边界而非隐式
继承 gitignore。
`.gitignore` 表达的是"不要提交"而非"不要看"。误伤高频目标：`.env`、`dist/`、`target/` 下产物与
**`*.log`（构建/测试日志）**、`node_modules/` 内第三方源码、`.claude/` 类工具目录。用户让 agent"看下报错
日志"，agent 会拿到一句拒绝然后卡住。
**更糟的是三工具三语义**：`grep` 走 rg（默认连命中都不返回）、`glob` 事后过滤、`read_file` 报错——模型
无法形成稳定心智模型。且错误信息主动教用户绕过（`:291` "如确需读取，请使用 bash: cat"），既然有旁路，
这道拦截**只是多一次无效工具调用**，无实际安全收益；安全应由 `is_sensitive`（`:289`）承担。

**P1-6　检索结果裁剪不回传给模型**
`builtin.rs:705`/`:745` `truncate_lines(&stdout, 200)` 静默截断。模型看到 200 条就以为搜完了，不会收窄
查询。opencode 的 `truncated=true` + "Consider using a more specific path or pattern"
（`grep.go:172-173`）就是为解决这个。

**P1-7　`list_dir` 无 gitignore 过滤、无条目上限**
Rust 仓 `list_dir(recursive=true)` 会把 `target/` 下几万文件全列出（`builtin.rs:847` 直接
`entries.join("\n")`），单次工具输出即可撑爆上下文。

### P2

**P2-8　无模糊文件名搜索**：`nucleo` 在 3 个非主线 crate，`ccore` 拿不到。主线没有"我记不清文件名"的解法。

**P2-9　`rg`/`find` 子进程无超时**：`builtin.rs:689` `cmd.output().await` 无 `tokio::time::timeout`。
对比 `bash` 工具有完整超时（`:144,186-187,247`）、web 工具有 30s（`:935,:1029`）——**只有检索工具漏了**。
一个灾难性回溯正则或误传 `/` 作为 path，会让 agent 无限期挂死且无法取消。

**P2-10　WorldState 半移植，动态上下文重锚缺失**：长会话中 CLAUDE.md 被改后旧内容仍留在上下文且无人告知
模型它已过期（codex 用 `world_state/agents_md.rs:10-11` 的替换通知精确解决）。

**P2-11　LSP 未接入检索链**：ccode 有 `lsp` 工具，但对比 opencode 把 LSP 诊断做成独立 `diagnostics` 工具，
ccode 未见等价的符号查询/诊断专用工具。

## 3.7 「救活 Context Engine」vs「学 Claude Code 走纯 agentic 搜索」

**建议：不要救。** 转向纯 agentic 搜索，把这份工作量投到 P0-2/P0-3 与 P1-4/6/7。理由按权重：

1. **6 款参照（含 codex、Claude Code 两个最强的）无一做代码语义索引。** 这是收敛后的共识：模型的工具
   调用能力已强到可以自己用 rg 二分逼近，而索引要承担建索引延迟、增量失效、embedding API 成本与延迟、
   磁盘体积、维度/模型漂移、大仓首次索引数分钟——收益常被一次精确正则抹平。逆共识需要明确的差异化理由，
   代码里看不到。
2. **ccode 缺的是索引的全部前置条件，不是两个开关。** `ccore` 无 `ignore`（无法正确遍历大仓）、无
   tree-sitter（只能按行粗切，检索质量差）、`RepoMap` 的 import 解析只在测试里跑过、
   `listen_enrichment` 是**同步 `fn`** 而 `embed_batch` 是 async（结构性冲突）、维度三处不一致
   （`sensory.rs:71` 假定 1536 / `ccore_integration.rs:531` 假定 64 / API 实际维度未知）、无索引持久化与
   失效机制（每次启动重 embed 全仓 = 不可接受的启动延迟 + API 账单）。这是一个从零开始的子系统。
3. **基础原语还没做好。** `glob` 用 `find`、`list_dir` 无上限、截断不回传、`read_file` 误拦、rg 无超时——
   每条都在**当前每次真实使用**中产生可感知损害，且每条只需几十行。在原语劣于 opencode 时做语义索引是
   典型的优化倒置。
4. **P0-2/P0-3 性价比高一个数量级。** AGENTS.md：实现与测试都已写好，只差在 `launcher.rs:185` 附近调一次
   `discover_agents_md` + `format_agents_md_section` 推入工作记忆。CLAUDE.md 向上遍历 + 字节预算：改
   `project_context.rs:19-30` 一个函数，照抄 codex `agents_md.rs:1-17` 的三步算法。两项合计可能不到
   150 行，却直接决定"用户写的项目规范 agent 到底看不看得见"。
5. **如果一定要做，正确形态是工具而非自动注入。** 让模型决定何时用（对比 opencode 把 Sourcegraph 做成工具）。
   自动注入的根本问题是：命中不准时它无声污染上下文预算，模型没有拒绝的机会——
   `intent_retriever.rs:102` 的 `MIN_RELEVANCE_SCORE = 0.3` 与 `:91-94` 的注释说明作者已意识到这个风险。
6. **想解决上下文膨胀，学 subagent 隔离比学索引有效。** ccode 已有 `task` 工具。把探索性搜索放进子 agent、
   只回传结论，能拿到索引想要的大部分收益（少占主上下文），成本几乎为零。**这是建议的替代方向。**

**若决策仍是"要救"**，最小可行顺序：先只做 RepoMap（目录树 + import 图，**不需要 embedding**，纯静态分析，
作为 `repo_map` 工具或按需摘要暴露），在真实大仓验证有用之后再考虑向量层。**绝不要先接 embedding**——
那是链条上最贵、最不确定、最难回退的一环。

若要接向量，注入点分析：`ccore` 当前不依赖 `ccode-memory`。较干净的路是复用
`ccore/src/memory/short_term.rs:21` 已有的 `pub trait EmbeddingProvider`（含 `embed_batch` 默认实现
`:29`），在 `ThinkerNode` 上加 `Option<Arc<dyn EmbeddingProvider>>` + setter，由 `ccode-cli/src/main.rs:126-134`
注入 `ccode-memory::ApiEmbeddingProvider` 的适配实例（setter 需暴露到 `KernelConfig` 层，因为
`launcher.rs` 在 `ccore` 内）。另一条是让 `ccore` 直接依赖 `ccode-memory`——省事但把 reqwest/sqlite-vec
拖进内核。

## 3.8 不确定项

- **claude-code 全部实现细节**为闭源反推。只能确认其仓库文档无索引/embedding 迹象，不能证明产品内部没有。
- **codex `AGENTS_MD_MAX_BYTES` 确切值**未直接读到定义（`config/mod.rs:4163` 引用了它）；32KiB 是从
  `thread-manager-sample/src/main.rs:251` 的 `32 * 1024` 推断，可能与实际默认值不同。
- **各家 system prompt 里对检索工具的措辞与优先级引导**未逐字通读，"取向"判断部分基于工具集构成。
- **`ccore/src/memory/repo_map.rs` 的 import 解析覆盖哪些语言**只确认了 `Language` 枚举存在
  （被 `function_embed.rs:16` 引用）与构造只发生在测试中，未逐语言核实质量。
- **grok-build `xai-fuzzy-file-search` 的后台 daemon 是否真接线**只读了 Cargo.toml description 与依赖，
  未追运行时构造点——按方法论纪律记为**未验证**，不能当作"grok-build 已有"的定论。
- **`ccode-memory::ApiEmbeddingProvider` 的向量维度**未确认（决定维度统一方案）。
- **`ccode-workspace` 的 hub RPC 路径**是否在某些运行模式下能把 AGENTS.md 送进 prompt——已确认主线
  Thinker 侧不消费，但未穷举 `ccode-shell` 的 workspace 代理模式。
