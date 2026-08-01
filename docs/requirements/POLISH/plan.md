# 全面打磨 实现计划

**目标：** 清理死代码+unused warning、E2E 运行验证、打磨用户体验细节，将评分从 95 提升至 105+

**架构：** 三步走：清理→验证→打磨

**技术栈：** Rust cargo check/clippy、ccore Kernel 启动

---

## 任务拆分

### 任务 1：清理 ccore unused warning

**目标：** 消除 ccore crate 的所有编译 warning

**文件**：
- 修改：ccore 下所有含 unused warning 的文件（约 15 个）

**实现要点**：
- 移除未使用的 import 语句
- 给确实需要保留但暂时未使用的字段/方法加 #[allow(dead_code)]（带注释说明保留原因）
- 删除空 stub 文件（hot_reload.rs）并从 mod.rs 移除声明
- 确保 cargo check -p ccore 零 warning

---

### 任务 2：E2E 运行验证

**目标：** 验证 Kernel 启动+ZMQ 总线+Node 注册+感官信号+反射弧全链路

**文件**：
- 修改：ccore/src/kernel/mod.rs（如需修复启动问题）

**实现要点**：
- 运行 cargo run -p ccode-shell（或 ccode-pager-bin）尝试启动
- 检查 Kernel 启动日志：ZMQ socket 绑定、Node 注册、MCP Server（如 enabled）
- 如果启动失败，修复问题
- 记录成功/失败结果

---

### 任务 3：流式渲染 Spinner 动画

**目标：** 在工具调用进行中时显示 spinner 动画

**文件**：
- 修改：ccode-pager/src/app/agent_view/render.rs

**实现要点**：
- 在 ToolCallBlock 渲染时，如果 end_time 为 None（进行中），显示旋转字符动画（⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏）
- 使用 std::time::Instant 计算当前帧索引
- 完成后显示 ✓ 或 ✗

---

### 任务 4：MCP Server 集成测试

**目标：** 验证 MCP Server 的 tools/list 和 tools/call 端到端可用

**文件**：
- 修改：ccore/src/mcp_server/handler.rs（增加测试）

**实现要点**：
- 在 handler.rs 的 tests 模块中增加集成测试
- 模拟完整的 initialize → tools/list → tools/call 流程
- 验证 6 个工具都能正确返回描述和 schema
