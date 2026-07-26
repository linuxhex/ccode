# ccode 使用指南

## 快速开始

### 1. 基础配置

创建配置文件 `ccode.toml`：

```toml
[kernel]
# Kernel 网络配置
router_addr = "tcp://127.0.0.1:5555"
pub_addr = "tcp://127.0.0.1:5556"

# 健康检查配置
heartbeat_timeout_secs = 30

[backpressure]
# 背压控制配置
high_watermark = 0.8        # 80% 开始限流
critical_watermark = 0.95   # 95% 严重限流
backpressure_delay_ms = 10
critical_delay_ms = 100
max_rate = 1000             # 最大 1000 msg/s

[ack]
# 消息确认配置
ack_timeout_secs = 30
max_retries = 3
initial_retry_delay_ms = 1000
max_retry_delay_ms = 30000

[monitoring]
# 监控配置
max_error_rate = 0.1        # 最大错误率 10%
max_latency_ms = 5000       # 最大延迟 5 秒
min_throughput = 1          # 最小吞吐量 1 msg/s
```

### 2. 启动 Kernel

```rust
use ccode_core::kernel::{Kernel, KernelConfig};
use ccode_core::config::CcodeConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载配置
    let config = CcodeConfig::from_file("ccode.toml")?;
    
    // 创建 Kernel
    let mut kernel = Kernel::new(config.kernel);
    
    // 启动 Kernel
    println!("启动 Kernel...");
    kernel.start().await?;
    
    // 运行事件循环
    kernel.run().await?;
    
    Ok(())
}
```

### 3. 创建 Agent Node

```rust
use ccode_core::node::{AgentNode, NodeId, NodeType};
use ccode_core::message::Topic;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 创建 Agent Node
    let node_id = NodeId::new("agent-001");
    let node_type = NodeType::Agent;
    
    let mut agent = AgentNode::new(
        node_id,
        node_type,
        // 订阅的消息 topic
        vec![
            "agent/001/input",
            "agent/*/output",
            "sys/ack",
        ],
        AgentConfig::default(),
    ).await?;
    
    // 连接到 Kernel
    agent.connect("tcp://127.0.0.1:5555", "tcp://127.0.0.1:5556").await?;
    
    println!("Agent Node 已连接");
    
    // 运行 Agent
    agent.run().await?;
    
    Ok(())
}
```

---

## 核心功能使用

### 1. 发送消息（自动序列号）

```rust
use ccode_core::message::{FrameCodec, Topic};
use serde_json::json;

// 创建消息
let payload = json!({
    "content": "Hello, World!",
    "timestamp": chrono::Utc::now().to_rfc3339(),
});

let msg = FrameCodec::new_message(
    Topic::new("agent/001/output"),
    "agent-001",
    &payload,
)?;

// 发送消息（自动添加序列号）
agent.send_message(&msg).await?;

// Kernel 会自动检查序列号，拒绝乱序消息
```

### 2. 接收消息并确认

```rust
// Agent 自动处理消息确认
async fn handle_message(&mut self, msg: Message) -> Result<()> {
    let topic = msg.topic.as_str();
    
    // 处理业务消息
    if topic.starts_with("agent/") {
        let payload: Value = FrameCodec::decode_payload(&msg)?;
        println!("收到消息：{}", payload);
        
        // 处理完成后，自动发送 ACK
        // （由 handle_message 内部处理）
    }
    
    // 处理 ACK 消息
    if topic == "sys/ack" {
        // 已自动处理
    }
    
    Ok(())
}
```

### 3. 背压控制

```rust
// Kernel 自动管理背压
// - 检测通道队列长度
// - 高负载时自动延迟发送
// - 记录背压统计

// 查看背压统计
let stats = kernel.backpressure.stats();
println!(
    "背压统计：发送={}, 丢弃={}, 触发次数={}",
    stats.sent_count,
    stats.dropped_count,
    stats.backpressure_count
);
```

### 4. 监控和健康检查

```rust
// Kernel 自动收集监控指标
// - 消息吞吐量
// - 延迟统计（P99）
// - 错误率

// 获取当前指标
let metrics = kernel.monitoring.get_metrics(active_node_count);
println!(
    "系统指标：发送={}, 接收={}, 成功={}, 失败={}, P99延迟={:.2}ms",
    metrics.messages_sent,
    metrics.messages_received,
    metrics.messages_success,
    metrics.messages_failed,
    metrics.p99_latency_ms
);

// 执行健康检查
let status = kernel.monitoring.health_check(active_node_count);
match status {
    HealthStatus::Healthy => println("系统健康"),
    HealthStatus::Warning(msg) => println!("警告：{}", msg),
    HealthStatus::Unhealthy(msg) => println!("不健康：{}", msg),
}
```

### 5. 序列号机制

```rust
// Node 发送消息时自动添加序列号
let msg1 = agent.create_message(...);  // 序列号：1
let msg2 = agent.create_message(...);  // 序列号：2
let msg3 = agent.create_message(...);  // 序列号：3

// Kernel 自动检查序列号
// - 如果收到 1, 2, 3 → 正常处理
// - 如果收到 1, 3, 2 → 拒绝消息 2（乱序）
// - 如果收到 1, 3, 4 → 接受消息 3，警告可能丢包（从 1 跳到 3）
```

### 6. Topic 深度限制

```rust
// Topic 自动验证
let valid_topic = Topic::new("agent/001/output");  // ✅ 有效
let invalid_topic = Topic::new("a/b/c/.../z");     // ❌ 超过深度限制（拒绝）

// Topic 匹配自动检查深度
let pattern = "agent/**/output";
let topic = "agent/001/.../output";  // 如果超过 100 层，自动拒绝匹配
```

---

## 高级功能

### 1. 自定义背压策略

```rust
use ccode_core::kernel::backpressure::{BackpressureConfig, BackpressureLevel};

let config = BackpressureConfig {
    high_watermark: 0.7,          // 70% 开始限流
    critical_watermark: 0.9,      // 90% 严重限流
    backpressure_delay_ms: 5,     // 轻度延迟
    critical_delay_ms: 50,        // 严重延迟
    max_rate: 500,                // 限流到 500 msg/s
    ..Default::default()
};

let kernel = Kernel::with_backpressure(config);
```

### 2. 自定义 ACK 策略

```rust
use ccode_core::message::ack::{AckConfig, AckManager};

let config = AckConfig {
    ack_timeout_secs: 60,         // 60 秒超时
    max_retries: 5,               // 最大重试 5 次
    initial_retry_delay_ms: 500,  // 初始延迟 500ms
    max_retry_delay_ms: 60000,    // 最大延迟 60s
    retry_backoff_multiplier: 3.0, // 3 倍退避
};

let ack_manager = AckManager::new(config);
```

### 3. 自定义监控配置

```rust
use ccode_core::kernel::metrics::{HealthCheckConfig, MonitoringService};

let config = HealthCheckConfig {
    heartbeat_timeout_secs: 60,   // 60 秒心跳超时
    max_error_rate: 0.05,         // 最大错误率 5%
    max_latency_ms: 3000,         // 最大延迟 3 秒
    min_throughput: 10,           // 最小吞吐量 10 msg/s
};

let monitoring = MonitoringService::new(config);
```

---

## 示例场景

### 场景 1：高可靠消息传递

```rust
// 启用 ACK + 重试
let ack_config = AckConfig {
    ack_timeout_secs: 30,
    max_retries: 5,
    ..Default::default()
};

// 发送消息
agent.send_with_ack(&msg).await?;

// 等待 ACK（由后台自动处理）
// 如果超时未收到 ACK，自动重试
// 达到最大重试次数后记录失败
```

### 场景 2：高负载处理

```rust
// 启用背压控制
let backpressure_config = BackpressureConfig {
    high_watermark: 0.8,
    critical_watermark: 0.95,
    ..Default::default()
};

// Kernel 自动监控队列
// - 队列 > 80%：轻度延迟发送
// - 队列 > 95%：严重延迟 + 记录警告

// 查看背压状态
let level = kernel.backpressure.get_level();
match level {
    BackpressureLevel::Normal => 正常处理,
    BackpressureLevel::High => 轻度限流,
    BackpressureLevel::Critical => 严重限流,
}
```

### 场景 3：系统监控

```rust
// 定期获取指标
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    
    loop {
        interval.tick().await;
        
        let metrics = kernel.monitoring.get_metrics(active_nodes);
        
        // 发送到监控系统
        send_to_prometheus(&metrics);
        
        // 检查健康状态
        let status = kernel.monitoring.health_check(active_nodes);
        if status != HealthStatus::Healthy {
            send_alert(&status);
        }
    }
});
```

---

## 故障排查

### 问题 1：消息乱序

**现象**：Kernel 拒绝消息，日志显示"序列号乱序"

**原因**：网络延迟导致消息乱序到达

**解决**：
```rust
// 1. 检查序列号跳跃
let gap = sequence - last_sequence;
if gap > 1 {
    tracing::warn!("可能丢包：{} 条消息", gap);
}

// 2. 检查网络稳定性
// 3. 调整超时时间
```

### 问题 2：背压触发频繁

**现象**：日志频繁显示"背压触发"

**原因**：消息生产速度 > 消费速度

**解决**：
```rust
// 1. 降低消息发送速率
// 2. 增加消费者数量
// 3. 调整背压阈值
let config = BackpressureConfig {
    high_watermark: 0.9,  // 提高阈值
    ..Default::default()
};
```

### 问题 3：消息丢失

**现象**：发送的消息没有到达目标

**原因**：ACK 机制未正确处理

**解决**：
```rust
// 1. 检查 ACK 配置
let ack_config = AckConfig {
    max_retries: 5,  // 增加重试次数
    ..Default::default()
};

// 2. 检查失败队列
let failed_messages = ack_manager.get_failed_messages().await;
for (msg, error) in failed_messages {
    tracing::error!("消息失败：{} - {}", msg.header.msg_id, error);
}

// 3. 手动重发失败消息
```

---

## 最佳实践

### 1. 配置优化

```rust
// 生产环境推荐配置
let config = CcodeConfig {
    kernel: KernelConfig {
        heartbeat_timeout_secs: 60,  // 较长超时
    },
    backpressure: BackpressureConfig {
        high_watermark: 0.8,
        critical_watermark: 0.95,
        max_rate: 2000,  // 根据系统容量调整
    },
    ack: AckConfig {
        ack_timeout_secs: 30,
        max_retries: 3,
        initial_retry_delay_ms: 1000,
    },
    monitoring: HealthCheckConfig {
        max_error_rate: 0.05,  // 严格错误率
        max_latency_ms: 3000,
    },
};
```

### 2. 监控告警

```rust
// 监控关键指标
- 消息吞吐量（持续下降 → 告警）
- 错误率（> 5% → 告警）
- P99 延迟（> 3s → 告警）
- 背压触发次数（频繁触发 → 告警）
- 失败消息数量（> 100 → 告警）
```

### 3. 容错设计

```rust
// 1. 消息持久化（失败消息）
// 2. 重试机制（指数退避）
// 3. 熔断降级（错误率过高时）
// 4. 限流保护（背压控制）
```

---

## 总结

ccode 现在具备：

✅ **可靠性**：序列号检查 + ACK + 重试
✅ **性能**：批量发送 + 背压控制
✅ **安全性**：Topic 限制 + 流量整形
✅ **可观测性**：监控指标 + 健康检查

系统已经可以用于生产环境！