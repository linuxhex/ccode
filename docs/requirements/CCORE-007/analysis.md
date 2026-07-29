# 需求分析：工具实现全面升级（对标 Claude Code）

## 需求概述
> 参考 Claude Code 的工具实现细节，将 ccore 的 7 个内置工具全部升级到工业级质量

## 业务背景
- ccore 已有 7 个核心工具执行器（bash/read/write/edit/grep/glob/list_dir），但实现较为简陋
- Claude Code 的工具经过大量实战打磨，有丰富的安全检查、输出格式化、错误恢复能力
- 在相同 LLM 下，工具质量直接影响 Agent 的代码编辑能力

## 改动范围

### 需要升级的工具（7 个）

| 工具 | 当前状态 | 升级要点 |
|------|---------|---------|
| read | 基础读取+行号 | offset/limit分页、gitignore过滤、文件大小限制、二进制检测、技能文件特殊处理 |
| write | 基础写入 | 原子写入(temp→rename)、二进制检测、目录自动创建、写入后验证 |
| edit | 搜索替换 | 多种编辑模式(insert/replace/write)、上下文展示、唯一性验证、原子写入 |
| bash | 基础执行 | 命令安全预检查、工作目录校验、输出截断、后台任务支持 |
| grep | 基础ripgrep | 类型过滤、上下文行数、包含/排除模式 |
| glob | 基础find | 更快的glob匹配、排除模式、排序 |
| list_dir | 基础ls | 递归模式、隐藏文件显示、文件类型图标 |

### 需要新增的辅助模块

| 模块 | 职责 |
|------|------|
| path_validator | 路径安全校验（防止路径遍历、检查工作目录内） |
| output_formatter | 工具输出格式化（截断、行号、语法高亮标记） |
| gitignore_filter | .gitignore 过滤（防止读取/搜索被忽略的文件） |

## 风险与注意
- 原子写入依赖 `tokio::fs::rename`，跨文件系统可能失败
- gitignore 过滤需要解析 .gitignore 规则
- bash 安全检查不能太严格，否则影响正常使用
