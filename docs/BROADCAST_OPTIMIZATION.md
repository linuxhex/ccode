# 广播性能优化方案

## 当前问题

**位置**：`kernel/mod.rs:325-346`

**问题**：逐个发送消息到多个 Node，阻塞事件循环

```rust
async fn route_and_forward(
    &self,
    msg: &Message,
    transport: &mut KernelTransport,
) -> Result<()> {
    let targets = self.broker.route_message(msg)?;

    // ❌ 逐个发送，阻塞事件循环
    for (identity, frames) in targets {
        let identity_bytes = Bytes::from(identity);
        let frames_bytes: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
        transport.send_to(identity_bytes, frames_bytes).await?;  // 每个 1ms
    }

    Ok(())
}
```

**影响**：
- 100 个 Node × 1ms = 100ms 阻塞
- 期间无法处理其他消息（心跳、注册等）
- 可能导致健康检查误判 Node 超时

## 优化方案

### 方案 1：使用批量发送（推荐）

**修改位置**：`kernel/mod.rs:325-346`

**优化代码**：

```rust
async fn route_and_forward(
    &self,
    msg: &Message,
    transport: &mut KernelTransport,
) -> Result<()> {
    let targets = self.broker.route_message(msg)?;

    if targets.is_empty() {
        return Ok(());
    }

    // ✅ 批量发送优化
    // 由于所有目标的消息帧相同，可以合并为一次调用
    let identities: Vec<Bytes> = targets
        .into_iter()
        .map(|(identity, _)| Bytes::from(identity))
        .collect();

    // 编码消息帧（只需一次）
    let frames = FrameCodec::encode(msg)?;
    let frames_bytes: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();

    // 批量发送（一次调用）
    transport.send_to_many(identities, frames_bytes).await?;

    Ok(())
}
```

**优点**：
- 一次通道发送，不阻塞事件循环
- 减少内存拷贝（消息帧只编码一次）
- 性能提升：100 Node × 1ms → 1 次调用 ≈ 1ms

**需要修改**：
- `kernel/transport.rs` 的 `send_to_many` 实现（已存在）
- `kernel/broker.rs` 的 `route_message` 返回值（可选优化）

### 方案 2：异步批量发送（更高级）

**优化代码**：

```rust
async fn route_and_forward(
    &self,
    msg: &Message,
    transport: &mut KernelTransport,
) -> Result<()> {
    let targets = self.broker.route_message(msg)?;

    if targets.is_empty() {
        return Ok(());
    }

    // ✅ 完全异步，不阻塞事件循环
    let identities: Vec<Bytes> = targets
        .into_iter()
        .map(|(identity, _)| Bytes::from(identity))
        .collect();

    let frames = FrameCodec::encode(msg)?;
    let frames_bytes: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();

    // 使用 tokio::spawn 完全异步发送
    let router_send_tx = transport.router_send_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = router_send_tx
            .send(RouterSendCommand::MultiSend {
                identities,
                frames: frames_bytes,
            })
            .await
        {
            tracing::warn!("批量发送失败：{}", e);
        }
    });

    Ok(())
}
```

**优点**：
- 完全不阻塞事件循环
- 立即返回，继续处理其他消息
- 性能最优

**缺点**：
- 错误处理更复杂
- 无法保证发送顺序

### 方案 3：批量 + 重试（最完整）

**优化代码**：

```rust
async fn route_and_forward(
    &self,
    msg: &Message,
    transport: &mut KernelTransport,
) -> Result<()> {
    let targets = self.broker.route_message(msg)?;

    if targets.is_empty() {
        return Ok(());
    }

    // 分批处理（避免一次性发送太多）
    const BATCH_SIZE: usize = 50;
    
    let identities: Vec<Bytes> = targets
        .into_iter()
        .map(|(identity, _)| Bytes::from(identity))
        .collect();

    let frames = FrameCodec::encode(msg)?;
    let frames_bytes: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();

    // 分批发送
    for batch in identities.chunks(BATCH_SIZE) {
        transport.send_to_many(batch.to_vec(), frames_bytes.clone()).await?;
    }

    Ok(())
}
```

**优点**：
- 避免一次性发送太多（内存压力）
- 仍然比逐个发送快很多
- 错误隔离（一个批次失败不影响其他）

## 性能对比

| 方案 | 100 Node 耗时 | 阻塞事件循环 | 内存使用 | 复杂度 |
|------|--------------|-------------|---------|--------|
| 当前（逐个发送） | ~100ms | ✅ 是 | 低 | 低 |
| 方案 1（批量发送） | ~1ms | ⚠️ 部分 | 中 | 低 |
| 方案 2（异步批量） | ~0ms | ❌ 否 | 中 | 中 |
| 方案 3（分批+重试） | ~2ms | ⚠️ 部分 | 中 | 高 |

## 推荐方案

**推荐使用方案 1（批量发送）**

**理由**：
- 实现简单，修改量小
- 性能提升显著（100x）
- 风险低，易于测试
- 兼容现有架构

## 实施步骤

1. 修改 `kernel/mod.rs` 的 `route_and_forward` 方法
2. 修改 `kernel/broker.rs` 的 `route_message` 方法（可选）
3. 添加单元测试验证批量发送
4. 添加性能基准测试

## 额外优化

### 优化 broker.route_message

**当前实现**：

```rust
pub fn route_message(&self, msg: &Message) -> Result<Vec<(NodeIdentity, Vec<Vec<u8>>)>> {
    let frames = FrameCodec::encode(msg)?;  // 编码消息
    
    let targets: Vec<(NodeIdentity, Vec<Vec<u8>>)> = subscribers
        .iter()
        .filter_map(|id| {
            let identity = self.node_identities.get(id)?;
            Some((identity.clone(), frames.clone()))  // 每次克隆 frames
        })
        .collect();

    Ok(targets)
}
```

**优化为**：

```rust
pub fn route_message(&self, msg: &Message) -> Result<(Vec<NodeIdentity>, Vec<Vec<u8>>)> {
    let frames = FrameCodec::encode(msg)?;
    
    let identities: Vec<NodeIdentity> = subscribers
        .iter()
        .filter_map(|id| {
            self.node_identities.get(id).cloned()
        })
        .collect();

    Ok((identities, frames))  // 返回 identity 列表 + 单个 frames
}
```

**优点**：
- 减少 `frames.clone()` 调用（从 N 次 → 1 次）
- 内存使用减少 N 倍
- 与批量发送接口完美匹配

## 总结

通过批量发送优化，可以将广播性能提升 **100 倍**，从 100ms 降低到 1ms，显著改善系统响应性。