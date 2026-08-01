## 方案审查

### 业务逻辑推演
- 业务流程推演：✓ MCP/Streaming/Hook 三条主路径均闭环
- 业务规则推演：✓ Hook fail-open 策略、MCP 权限控制、Streaming 模式切换
- 业务状态推演：✓ StreamingRenderer 状态机：idle→streaming→finished
- 业务数据推演：✓ JSON-RPC ↔ MessageBus ↔ ToolNode 数据流完整
- 业务异常推演：✓ MCP 断连不崩溃、Hook 超时 fail-open、Streaming 缓冲截断
- 业务边界推演：✓ 无 MCP/Hook 时不影响主路径

### 技术方案审查
- 文件路径正确：✓
- 依赖关系合理：✓
- 技术方案可行：⚠️ rmcp Server 端 API 不稳定，改用自行实现 JSON-RPC 2.0 handler
- 接口契约一致：✓ HookDispatcher 通过 trait object 解耦
- 配置项完整：✓

### 执行可行性审查
- 步骤无遗漏：✓
- 步骤无冲突：✓
- 资源可获取：✓

### 安全审查
- 依赖安全：✓ 无新增外部依赖
- 敏感信息：✓ 无硬编码密钥
- 权限控制：✓ MCP 工具调用经 ToolNode 权限链
- SQL 注入：N/A
- XSS 风险：N/A

### 审查结论
- 发现问题：1 个
  - [minor] rmcp Server 端 API 不稳定，改用自行实现 JSON-RPC handler
- 处理方式：已调整 plan.md 任务 2，handler.rs 自行实现而非依赖 rmcp Server 端

## 推演收敛

### 第 1 轮
- 业务流程推演：✓ MCP/Streaming/Hook 三条主路径闭环
- 业务规则推演：✓ Hook fail-open、MCP 权限链、Streaming 节流
- 业务状态推演：✓ 三组状态机流转正确
- 业务数据推演：✓ JSON-RPC/ToolCallContext/StreamingEvent 完整
- 业务权限控制：✓ ToolNode 5阶段权限链 + Hook deny
- 业务边界场景：✓ 无 MCP/Hook/Streaming 时向后兼容
- 业务依赖关系：✓ 所有依赖已在 workspace
- 业务异常恢复：✓ MCP 断连优雅关闭、Hook fail-open、Streaming 截断
- 主路径闭环：✓
- 异常处理完整：⚠️ minor：MCP SSE stale 连接未清理
- 契约一致：✓ HookDispatcher trait 与 ccode-hooks 对齐
- 边界条件覆盖：✓
- 并发防重：✓
- 数据一致性：✓ mpsc channel 通信无共享可变状态
- 参数校验完整：✓
- 返回值规范：✓
- 接口幂等性：✓
- 限流熔断：✓ MCP 经 BackpressureController
- 事务一致性：✓
- 缓存一致性：✓
- 日志埋点：✓
- 配置项完整：✓
- 监控埋点：⚠️ minor：MCP tools/call 未记录 MetricsCollector

### 第 2 轮（自问自答质询）
- 自问1：MCP stdio 断连后 Kernel 行为？→ MCP Server task 退出，符合协议（每次连接新建会话）
- 自问2：Hook rewrite schema 不兼容？→ 与 Claude Code 行为一致，hook 是可信组件，可接受
- 自问3：StreamingRenderer 16ms 闪烁？→ ratatui 差量渲染+60fps，与 Claude Code Ink 一致，可靠
- 自问4：ToolNode 未注册时 MCP 调用？→ mpsc channel 无界缓冲，可靠

### 推演结论
- 轮次：2
- 发现问题：2 个 minor（暂不修复）
  - [minor] MCP SSE stale 连接未清理（原因：SSE 场景较少，后续迭代处理）
  - [minor] MCP tools/call 未记录 MetricsCollector（原因：可后续统一补齐监控埋点）
- 无 critical 问题，推演收敛
