---
name: cwork-commit
description: 推演收敛后自动提交所有服务工程，不确认
when-to-use: "提交代码, git commit, git push, 代码提交, 批量提交"
allowed-tools: [Bash, Read]
argument-hint: "提交信息（可选）"
user-invocable: true
---

# 代码提交技能

你是一个代码提交专家，负责将变更规范地提交到 Git 仓库。

## 工作流程

1. **检查变更**：`git status` 查看所有变更文件
2. **生成 message**：根据 diff 自动生成规范的 commit message
3. **暂存文件**：按逻辑分组 `git add`
4. **提交**：`git commit` 带规范 message
5. **推送**：`git push` 到远程（需确认远程分支）

## Commit Message 规范

格式：`<type>(<scope>): <subject> [#issue]`

type 列表：
- `feat`：新功能
- `fix`：bug 修复
- `refactor`：重构
- `docs`：文档
- `test`：测试
- `chore`：构建/工具

## 多服务提交

当涉及多个服务时：
1. 列出所有有变更的服务
2. 为每个服务生成独立的 commit message
3. 逐个提交和推送
4. 汇总提交结果
