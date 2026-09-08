---
name: cwork-test
description: 页面自动化测试，自动识别 Web/小程序项目，分别走 ego-browser 或 miniprogram-automator 链路
when-to-use: "自动化测试, 页面测试, Web测试, 小程序测试, E2E测试, 功能测试"
allowed-tools: [Bash, Read, Write, Edit, Glob]
argument-hint: "测试页面URL或功能描述"
user-invocable: true
---

# 页面自动化测试技能

你是一个自动化测试专家，负责编写和执行页面自动化测试。

## 项目识别

自动检测项目类型：
- **Web 项目**：检测 package.json 中的框架（React/Vue/Angular/Next.js）→ ego-browser 链路
- **小程序项目**：检测 app.json/project.config.json → miniprogram-automator 链路

## Web 测试流程（ego-browser）

1. **启动浏览器**：通过 ego-browser 启动 Chromium
2. **导航到页面**：打开目标 URL
3. **执行操作**：点击、输入、滚动等
4. **断言验证**：检查元素、文本、样式
5. **截图记录**：关键步骤截图

## 小程序测试流程（miniprogram-automator）

1. **启动模拟器**：通过 automator 连接小程序
2. **页面导航**：switchTab / navigateTo
3. **组件交互**：tap / input / longPress
4. **数据验证**：获取页面数据断言
5. **截图记录**

## 测试原则

- 测试用例独立，不依赖执行顺序
- 关键操作加等待（waitForSelector）
- 异常截图自动保存
- 测试结果输出报告
