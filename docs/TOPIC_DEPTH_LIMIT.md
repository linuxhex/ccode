# Topic 深度限制方案

## 问题描述

**位置**：`message/topic.rs:156-168`

**问题**：递归匹配函数可能导致栈溢出

```rust
fn match_parts(pattern: &[&str], topic: &[&str]) -> bool {
    match (pattern.first(), topic.first()) {
        (None, None) => true,
        (Some("**"), _) => {
            // ❌ 递归深度 = topic 长度，可能栈溢出
            match_parts(&pattern[1..], topic)
                || (!topic.is_empty() && match_parts(pattern, &topic[1..]))
        }
        (Some("*"), Some(_)) => match_parts(&pattern[1..], &topic[1..]),
        (Some(p), Some(t)) if p == t => match_parts(&pattern[1..], &topic[1..]),
        _ => false,
    }
}
```

**攻击场景**：
```
恶意 topic: "agent/a/b/c/d/e/.../z/output"  (1000 层)
→ 递归深度 1000
→ 栈溢出
→ Kernel 崩溃
```

## 修复方案

### 方案 1：添加深度限制（推荐）

**修改代码**：

```rust
/// Topic 匹配（带深度限制）
const MAX_TOPIC_DEPTH: usize = 100;  // 最大允许 100 层

fn topic_matches(pattern: &str, topic: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();
    
    // ✅ 检查深度
    if pattern_parts.len() > MAX_TOPIC_DEPTH || topic_parts.len() > MAX_TOPIC_DEPTH {
        tracing::warn!(
            "Topic 深度超过限制：pattern={}, topic={}, max={}",
            pattern_parts.len(),
            topic_parts.len(),
            MAX_TOPIC_DEPTH
        );
        return false;
    }
    
    match_parts_with_depth(&pattern_parts, &topic_parts, 0)
}

/// 带深度检查的递归匹配
fn match_parts_with_depth(pattern: &[&str], topic: &[&str], depth: usize) -> bool {
    // ✅ 深度检查
    if depth > MAX_TOPIC_DEPTH {
        return false;
    }
    
    match (pattern.first(), topic.first()) {
        (None, None) => true,
        (Some("**"), _) => {
            match_parts_with_depth(&pattern[1..], topic, depth + 1)
                || (!topic.is_empty() && match_parts_with_depth(pattern, &topic[1..], depth + 1))
        }
        (Some("*"), Some(_)) => match_parts_with_depth(&pattern[1..], &topic[1..], depth + 1),
        (Some(p), Some(t)) if p == t => match_parts_with_depth(&pattern[1..], &topic[1..], depth + 1),
        _ => false,
    }
}
```

**优点**：
- 简单有效
- 性能影响小
- 防止栈溢出

### 方案 2：转换为迭代式（最优）

**修改代码**：

```rust
/// 迭代式 Topic 匹配（无栈溢出风险）
fn match_parts_iterative(pattern: &[&str], topic: &[&str]) -> bool {
    use std::collections::VecDeque;
    
    // 使用 BFS/DFS 搜索匹配路径
    let mut queue = VecDeque::new();
    queue.push_back((0, 0));  // (pattern_idx, topic_idx)
    
    while let Some((p_idx, t_idx)) = queue.pop_front() {
        // 匹配成功
        if p_idx == pattern.len() && t_idx == topic.len() {
            return true;
        }
        
        // 边界检查
        if p_idx >= pattern.len() || t_idx >= topic.len() {
            continue;
        }
        
        match pattern[p_idx] {
            "**" => {
                // ** 可以匹配任意段
                // 两种选择：匹配 0 段，或匹配 1+ 段
                queue.push_back((p_idx + 1, t_idx));  // 匹配 0 段
                queue.push_back((p_idx, t_idx + 1));  // 匹配 1 段，继续
            }
            "*" => {
                // * 匹配单个段
                queue.push_back((p_idx + 1, t_idx + 1));
            }
            p if p == topic[t_idx] => {
                // 精确匹配
                queue.push_back((p_idx + 1, t_idx + 1));
            }
            _ => {
                // 不匹配
                continue;
            }
        }
    }
    
    false
}
```

**优点**：
- 完全消除栈溢出风险
- 性能更好（迭代式）
- 可处理任意深度

### 方案 3：Topic 验证（防御式）

**在 Topic 创建时验证**：

```rust
impl Topic {
    pub fn new(topic: impl Into<String>) -> Self {
        let t = topic.into();
        
        // ✅ 验证 topic 格式
        if let Err(e) = validate_topic(&t) {
            tracing::warn!("无效的 topic：{} - {}", t, e);
            // 返回默认或空 topic
            return Self(String::new());
        }
        
        Self(t)
    }
}

fn validate_topic(topic: &str) -> Result<(), String> {
    let parts: Vec<&str> = topic.split('/').collect();
    
    // 检查深度
    if parts.len() > MAX_TOPIC_DEPTH {
        return Err(format!("深度超过限制：{}", parts.len()));
    }
    
    // 检查每段长度
    for part in &parts {
        if part.len() > 100 {
            return Err(format!("段长度超过限制：{}", part.len()));
        }
        if part.is_empty() && parts.len() > 1 {
            return Err("包含空段".to_string());
        }
    }
    
    Ok(())
}
```

**优点**：
- 在源头阻止非法 topic
- 全面验证（深度、长度、格式）
- 记录详细错误信息

## 推荐方案

**推荐使用方案 1 + 方案 3**

**理由**：
- 方案 1：简单有效，快速修复
- 方案 3：防御式编程，防止恶意输入
- 两者结合：匹配时检查 + 创建时验证

## 实施步骤

1. 在 `message/topic.rs` 中添加 `MAX_TOPIC_DEPTH` 常量
2. 修改 `topic_matches` 函数，添加深度检查
3. 修改 `match_parts` 函数，添加深度参数
4. 修改 `Topic::new` 方法，添加验证逻辑
5. 添加单元测试验证限制生效

## 性能影响

**深度限制 = 100**：
- 正常 topic（<10 层）：无影响
- 复杂 topic（10-100 层）：轻微影响
- 恶意 topic（>100 层）：直接拒绝

**内存使用**：
- 递归深度限制：100 × 每层 ~100 bytes = 10KB 栈空间
- 安全范围内

## 测试用例

```rust
#[test]
fn test_topic_depth_limit() {
    // 正常 topic
    assert!(topic_matches("agent/*/output", "agent/test/output"));
    
    // 深度超限
    let deep_topic = (0..150).map(|_| "a").collect::<Vec<_>>().join("/");
    assert!(!topic_matches(&deep_topic, &deep_topic));
}

#[test]
fn test_wildcard_with_depth_limit() {
    // ** 通配符也应该受深度限制
    let pattern = "agent/**/output";
    let deep_topic = (0..150).map(|i| format!("l{}", i)).collect::<Vec<_>>().join("/");
    let full_topic = format!("agent/{}/output", deep_topic);
    
    assert!(!topic_matches(pattern, &full_topic));
}
```

## 总结

通过添加 topic 深度限制，可以防止恶意构造的超长 topic 导致栈溢出，提高系统安全性。推荐同时实施匹配时检查和创建时验证，形成双重保护。