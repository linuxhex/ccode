# 工具实现全面升级 实现计划

**目标：** 参考 Claude Code 的工具实现，将 7 个内置工具升级到工业级质量

**架构：** 每个工具独立升级，新增辅助模块供所有工具共享

**技术栈：** Rust + tokio + serde_json + regex

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| crates/codegen/ccore/src/tools/builtin.rs | 修改 | 7个工具执行器升级 |
| crates/codegen/ccore/src/tools/path_validator.rs | 新增 | 路径安全校验 |
| crates/codegen/ccore/src/tools/output_formatter.rs | 新增 | 输出格式化 |
| crates/codegen/ccore/src/tools/gitignore_filter.rs | 新增 | .gitignore过滤 |
| crates/codegen/ccore/src/tools/mod.rs | 修改 | 导出新模块 |

---

## 任务拆分

### 任务 1：新增 path_validator.rs

**目标**：路径安全校验，防止路径遍历攻击

**文件**：
- 新增：`crates/codegen/ccore/src/tools/path_validator.rs`

**实现要点**：
- `validate_path_in_workspace(path, workspace_root)` - 检查路径在工作目录内
- `canonicalize_path(path)` - 规范化路径，解析 `..` 和符号链接
- `is_binary_file(path)` - 二进制文件检测（基于扩展名+magic bytes）
- `is_hidden_file(path)` - 隐藏文件检测
- `safe_relative_path(path, base)` - 计算安全相对路径
- 禁止路径：`/etc/passwd`、`/etc/shadow`、`.env`（可配置）

### 任务 2：新增 output_formatter.rs

**目标**：统一工具输出格式化

**文件**：
- 新增：`crates/codegen/ccore/src/tools/output_formatter.rs`

**实现要点**：
- `truncate_output(output, max_chars)` - 输出截断，超出时显示 `... [截断，共 N 字符]`
- `format_with_line_numbers(content, start_line)` - 带行号格式化
- `format_file_header(path, size, modified)` - 文件头信息
- `format_error(tool, path, error)` - 统一错误格式
- `format_search_result(path, line_num, content, context_lines)` - 搜索结果格式

### 任务 3：新增 gitignore_filter.rs

**目标**：.gitignore 过滤，防止读取/搜索被忽略的文件

**文件**：
- 新增：`crates/codegen/ccore/src/tools/gitignore_filter.rs`

**实现要点**：
- `GitignoreFilter::new(workspace_root)` - 加载 .gitignore 规则
- `is_ignored(path)` - 检查路径是否被忽略
- `should_skip(path)` - 综合判断（.git + node_modules + target + .gitignore）
- 默认跳过列表：`.git/`、`node_modules/`、`target/`、`__pycache__/`、`.DS_Store`

### 任务 4：升级 Read 工具

**目标**：参考 Claude Code ReadFile，增加分页、过滤、安全检查

**文件**：
- 修改：`crates/codegen/ccore/src/tools/builtin.rs` ReadExecutor

**实现要点**：
- 新增参数：`offset`(起始行)、`limit`(最大行数，默认2000)、`show_line_numbers`(默认true)
- 文件大小限制：超过 1MB 提示用户用 offset/limit 分页读取
- 二进制文件检测：检测到二进制文件时返回错误提示而非乱码
- gitignore 过滤：被忽略的文件拒绝读取
- 路径安全校验：防止读取工作目录外的文件
- 技能文件(.skill/)跳过行数限制
- 文件头信息：显示路径、大小、修改时间

### 任务 5：升级 Write 工具

**目标**：参考 Claude Code Write，增加原子写入和安全检查

**文件**：
- 修改：`crates/codegen/ccore/src/tools/builtin.rs` WriteExecutor

**实现要点**：
- 原子写入：先写入 `.tmp` 临时文件，再 `rename` 到目标路径
- 二进制检测：检测内容是否包含 NUL 字节
- 写入后验证：重新读取文件确认写入成功
- 路径安全校验：防止写入工作目录外的文件
- 目录自动创建：确保父目录存在（已有，保留）

### 任务 6：升级 Edit 工具

**目标**：参考 Claude Code HashlineEdit，增加多种编辑模式

**文件**：
- 修改：`crates/codegen/ccore/src/tools/builtin.rs` EditExecutor

**实现要点**：
- 新增参数：`mode`(replace/insert/write，默认replace)
- insert 模式：在指定行号后插入内容（`after_line` 参数）
- write 模式：完全覆盖文件内容
- 上下文展示：编辑成功后显示修改点前后 3 行
- 原子写入：同 Write 的 temp→rename 模式
- 唯一性验证：搜索文本必须唯一匹配（已有，保留）
- replace_all 时的匹配计数显示

### 任务 7：升级 Bash 工具

**目标**：参考 Claude Code BashTool，增加安全预检查

**文件**：
- 修改：`crates/codegen/ccore/src/tools/builtin.rs` BashExecutor

**实现要点**：
- 命令安全预检查：检测危险命令（rm -rf /、mkfs、dd、>: 等）
- 危险命令列表：`rm -rf /`、`rm -rf ~`、`mkfs`、`dd if=`、`> /dev/sd`、`chmod 777 /`、`shutdown`、`reboot`
- 工作目录校验：确保在正确的工作目录下执行
- 输出截断：超过 50000 字符时截断并提示
- 超时提示：超时后给出更友好的错误信息
- 退出码报告：非零退出码时明确标注

### 任务 8：升级 Grep 工具

**目标**：增强搜索能力

**文件**：
- 修改：`crates/codegen/ccore/src/tools/builtin.rs` GrepExecutor

**实现要点**：
- 新增参数：`file_type`(代码类型过滤，如rs/py/js)、`context`(上下文行数)、`include`(包含模式)、`exclude`(排除模式)
- ripgrep 不存在时回退到简单文本搜索
- gitignore 过滤：默认跳过被忽略的文件
- 结果数量限制：最多 200 个匹配
- 搜索统计：显示匹配文件数和总匹配数

### 任务 9：升级 Glob 工具

**目标**：更快的文件名匹配

**文件**：
- 修改：`crates/codegen/ccore/src/tools/builtin.rs` GlobExecutor

**实现要点**：
- 使用 Rust glob crate 替代 `find` 命令（如可用），否则保留 find
- 新增参数：`exclude`(排除模式)
- gitignore 过滤：默认跳过被忽略的文件
- 结果排序：按修改时间排序（最新的在前）
- 结果数量限制：最多 200 个文件

### 任务 10：升级 ListDir 工具

**目标**：增强目录列表

**文件**：
- 修改：`crates/codegen/ccore/src/tools/builtin.rs` ListDirExecutor

**实现要点**：
- 新增参数：`recursive`(递归模式)、`show_hidden`(显示隐藏文件)、`max_depth`(最大深度)
- 递归模式：显示子目录内容
- 文件类型标记：/ 表示目录、* 表示可执行、@ 表示符号链接
- 排序：目录在前，文件在后，各自按名称排序

### 任务 11：更新 mod.rs + 注册

**目标**：导出新模块并确保注册正确

**文件**：
- 修改：`crates/codegen/ccore/src/tools/mod.rs`

**实现要点**：
- 新增 `pub mod path_validator;`、`pub mod output_formatter;`、`pub mod gitignore_filter;`
- 确保 `register_builtin_executors` 正常工作
