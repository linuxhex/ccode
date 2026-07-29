//! ccore MVP 集成测试
//!
//! 测试纯逻辑层，不依赖 ZMQ 连接。
//! 覆盖：消息帧编解码、Broker 路由、Memory 系统、Agent 类型解析、Provider 配置。

use ccore::message::frame::FrameCodec;
use ccore::message::{Message, Topic, TopicPattern};
use ccore::kernel::broker::Broker;
use ccore::node::{NodeId, NodeType, PermissionMode};
use ccore::agent::AgentType;
use ccore::memory::short_term::ShortTermMemory;
use ccore::memory::heat::{HeatInput, HeatWeights, HeatThresholds, compute_heat, classify, Temperature};
use ccore::memory::window::{SlidingWindow, MessageMeta};
use ccore::memory::working::WorkingEntry;
use ccore::config::provider::{ProviderConfig, ProviderAdapter};
use ccore::sampler::router::ProviderRouter;

use serde::{Deserialize, Serialize};

// ============================================================================
// A) 消息帧编解码测试
// ============================================================================

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct TestPayload {
    text: String,
    value: i32,
}

/// 基本 Message 编解码一致性测试
#[tokio::test]
async fn test_frame_encode_decode_roundtrip() {
    let topic = Topic::agent_input("agent-42");
    let payload = TestPayload {
        text: "你好世界".into(),
        value: 999,
    };
    let msg = FrameCodec::new_message(topic, "node-1", &payload).unwrap();

    // 编码
    let frames = FrameCodec::encode(&msg).unwrap();
    assert_eq!(frames.len(), 3, "帧数量应为 3");

    // 解码
    let decoded = FrameCodec::decode(&frames).unwrap();
    assert_eq!(decoded.topic, msg.topic, "topic 应一致");
    assert_eq!(decoded.header.src_node, "node-1", "src_node 应一致");
    assert_eq!(decoded.header.reply_to, None, "reply_to 应为 None");

    // 解码 payload
    let decoded_payload: TestPayload = FrameCodec::decode_payload(&decoded).unwrap();
    assert_eq!(decoded_payload, payload, "payload 应一致");
}

/// 不同 payload 大小的编解码测试
#[tokio::test]
async fn test_frame_various_payload_sizes() {
    // 小 payload
    let small = serde_json::json!({"key": "val"});
    let msg_small = FrameCodec::new_message(Topic::sys_heartbeat(), "node-a", &small).unwrap();
    let frames_small = FrameCodec::encode(&msg_small).unwrap();
    let dec_small = FrameCodec::decode(&frames_small).unwrap();
    let p: serde_json::Value = FrameCodec::decode_payload(&dec_small).unwrap();
    assert_eq!(p["key"], "val");

    // 中等 payload（约 1KB）
    let medium_text = "x".repeat(1024);
    let medium = serde_json::json!({"content": medium_text});
    let msg_med = FrameCodec::new_message(Topic::state_query(), "node-b", &medium).unwrap();
    let frames_med = FrameCodec::encode(&msg_med).unwrap();
    let dec_med = FrameCodec::decode(&frames_med).unwrap();
    let p: serde_json::Value = FrameCodec::decode_payload(&dec_med).unwrap();
    assert_eq!(p["content"].as_str().unwrap().len(), 1024);

    // 大 payload（约 100KB）
    let large_text = "A".repeat(100 * 1024);
    let large = serde_json::json!({"data": large_text});
    let msg_large = FrameCodec::new_message(Topic::state_persist(), "node-c", &large).unwrap();
    let frames_large = FrameCodec::encode(&msg_large).unwrap();
    let dec_large = FrameCodec::decode(&frames_large).unwrap();
    let p: serde_json::Value = FrameCodec::decode_payload(&dec_large).unwrap();
    assert_eq!(p["data"].as_str().unwrap().len(), 100 * 1024);
}

/// Topic 路由匹配测试
#[tokio::test]
async fn test_topic_routing_match() {
    // 精确匹配
    let pattern = TopicPattern::new("agent/abc/input");
    let topic = Topic::agent_input("abc");
    assert!(pattern.matches(&topic), "精确匹配应成功");

    // 单段通配符 *
    let pattern = TopicPattern::all_agent_outputs();
    assert!(pattern.matches(&Topic::agent_output("abc")), "* 应匹配任意 agent ID");
    assert!(pattern.matches(&Topic::agent_output("xyz")), "* 应匹配任意 agent ID");
    assert!(!pattern.matches(&Topic::agent_input("abc")), "* 不应跨段匹配");

    // 多段通配符 **
    let pattern = TopicPattern::new("agent/**");
    assert!(pattern.matches(&Topic::agent_input("abc")), "** 应匹配多段");
    assert!(pattern.matches(&Topic::agent_output("def")), "** 应匹配多段");

    // sampler 通配符
    let pattern = TopicPattern::all_sampler_streams();
    assert!(pattern.matches(&Topic::sampler_stream("req-1")), "sampler 通配符匹配");
}

/// 回复消息的 reply_to 字段测试
#[tokio::test]
async fn test_reply_message() {
    let msg = FrameCodec::new_reply(
        Topic::state_response(),
        "state-1",
        "original-msg-id-123",
        &serde_json::json!({"ok": true}),
    ).unwrap();
    assert_eq!(msg.header.reply_to, Some("original-msg-id-123".into()));

    // 编解码一致性
    let frames = FrameCodec::encode(&msg).unwrap();
    let decoded = FrameCodec::decode(&frames).unwrap();
    assert_eq!(decoded.header.reply_to, Some("original-msg-id-123".into()));
}

/// 帧数量错误测试
#[tokio::test]
async fn test_frame_decode_invalid_length() {
    let result = FrameCodec::decode(&[vec![1u8], vec![2u8]]);
    assert!(result.is_err(), "2 帧应解码失败");
}

// ============================================================================
// B) Broker 路由测试
// ============================================================================

/// 注册 Node + 订阅 → 发消息 → 验证路由目标正确
#[tokio::test]
async fn test_broker_register_and_route() {
    let mut broker = Broker::new("ipc:///tmp/test-router".into(), "ipc:///tmp/test-pub".into());

    let agent_id: NodeId = "agent-1".parse().unwrap();
    let tool_id: NodeId = "tool-1".parse().unwrap();
    let state_id: NodeId = "state-1".parse().unwrap();

    // 注册 identity
    broker.register_identity(agent_id.clone(), b"agent-1-id".to_vec());
    broker.register_identity(tool_id.clone(), b"tool-1-id".to_vec());
    broker.register_identity(state_id.clone(), b"state-1-id".to_vec());

    // Agent 订阅自己的 input 和 tool_result
    broker.subscribe(agent_id.clone(), format!("agent/{}/input", agent_id));
    broker.subscribe(agent_id.clone(), format!("agent/{}/tool_result", agent_id));

    // Tool 订阅所有 Agent 的 tool_call
    broker.subscribe(tool_id.clone(), "agent/*/tool_call".into());

    // State 订阅所有状态查询
    broker.subscribe(state_id.clone(), "state/*".into());

    // 模拟 TUI 发送消息到 Agent input
    let msg = FrameCodec::new_message(
        Topic::agent_input("agent-1"),
        "tui-1",
        &serde_json::json!({"content": "请帮我写一个函数"}),
    ).unwrap();

    let targets = broker.route_message(&msg).unwrap();
    // 只有 agent-1 订阅了 agent/agent-1/input，tui-1 不是 agent-1 所以不过滤
    assert_eq!(targets.len(), 1, "应路由到 1 个目标");
    assert_eq!(targets[0].0, b"agent-1-id".to_vec(), "路由目标应为 agent-1");
}

/// 多订阅者通配符匹配测试
#[tokio::test]
async fn test_broker_wildcard_multiple_subscribers() {
    let mut broker = Broker::new("ipc:///tmp/test".into(), "ipc:///tmp/test-pub".into());

    let tui1: NodeId = "tui-1".parse().unwrap();
    let tui2: NodeId = "tui-2".parse().unwrap();
    let state: NodeId = "state-1".parse().unwrap();

    // 两个 TUI 都订阅所有 Agent 输出
    broker.register_identity(tui1.clone(), b"tui-1-id".to_vec());
    broker.register_identity(tui2.clone(), b"tui-2-id".to_vec());
    broker.register_identity(state.clone(), b"state-1-id".to_vec());

    broker.subscribe(tui1.clone(), "agent/*/output".into());
    broker.subscribe(tui2.clone(), "agent/*/output".into());
    broker.subscribe(state.clone(), "agent/*/event".into());

    // Agent 发送 output
    let msg = FrameCodec::new_message(
        Topic::agent_output("agent-x"),
        "agent-x",
        &serde_json::json!({"text": "输出结果"}),
    ).unwrap();

    let targets = broker.route_message(&msg).unwrap();
    // agent-x 是发送者，不应回发给自己，所以 tui1 和 tui2 各收到
    assert_eq!(targets.len(), 2, "应路由到 2 个 TUI 订阅者");

    // 验证不回发给发送者
    for (identity, _) in &targets {
        assert_ne!(identity.as_slice(), b"agent-x", "不应回发给发送者");
    }
}

/// Broker 注销测试
#[tokio::test]
async fn test_broker_deregister() {
    let mut broker = Broker::new("ipc:///tmp/test".into(), "ipc:///tmp/test-pub".into());

    let node_id: NodeId = "node-a".parse().unwrap();
    broker.register_identity(node_id.clone(), b"node-a-id".to_vec());
    broker.subscribe(node_id.clone(), "test/topic".into());

    // 注销
    broker.deregister_identity(&node_id);

    // 验证 identity 已移除
    assert!(broker.get_identity(&node_id).is_none(), "注销后 identity 应不存在");

    // 验证订阅已清理
    let subscribers = broker.find_subscribers("test/topic");
    assert!(!subscribers.contains(&node_id), "注销后订阅应被清理");
}

/// Broker find_subscribers 去重测试
#[tokio::test]
async fn test_broker_subscriber_dedup() {
    let mut broker = Broker::new("ipc:///tmp/test".into(), "ipc:///tmp/test-pub".into());

    let node: NodeId = "node-dup".parse().unwrap();
    broker.register_identity(node.clone(), b"node-dup-id".to_vec());
    // 同一 Node 订阅了两个匹配同一 topic 的 pattern
    broker.subscribe(node.clone(), "agent/*/output".into());
    broker.subscribe(node.clone(), "agent/**".into());

    let subscribers = broker.find_subscribers("agent/abc/output");
    // 去重后应只有 1 个
    let count = subscribers.iter().filter(|id| **id == node).count();
    assert_eq!(count, 1, "同一 Node 匹配多个 pattern 应去重");
}

// ============================================================================
// C) Memory 系统测试
// ============================================================================

/// ShortTermMemory store + search_by_text 测试
#[tokio::test]
async fn test_short_term_memory_store_and_search() {
    let mut mem = ShortTermMemory::new();

    // 存入几条消息
    let id1 = mem.store("user".into(), "请帮我写一个排序算法".into(), 20, false);
    let id2 = mem.store("assistant".into(), "好的，这是快速排序的实现...".into(), 80, false);
    let id3 = mem.store("user".into(), "天气怎么样？".into(), 10, false);
    let _id4 = mem.store("tool".into(), "ls -la 的执行结果...".into(), 50, true);

    // 验证存储数量和轮次
    assert_eq!(mem.len(), 4);
    assert_eq!(mem.current_turn(), 4);

    // 语义搜索：搜索"排序"相关内容
    let results = mem.search_by_text("排序算法", 2);
    assert!(!results.is_empty(), "搜索应返回结果");
    // 第 1 条（排序算法）应排在最前
    assert_eq!(results[0].id, id1, "最相关的应为排序消息");

    // 搜索天气
    let weather_results = mem.search_by_text("天气", 1);
    assert!(!weather_results.is_empty());
}

/// ShortTermMemory 空搜索和边界测试
#[tokio::test]
async fn test_short_term_memory_search_edge_cases() {
    let mut mem = ShortTermMemory::new();

    // 空 Memory 搜索
    let results = mem.search_by_text("任何内容", 5);
    assert!(results.is_empty(), "空 Memory 搜索应返回空");

    // 存入 1 条
    let id = mem.store("user".into(), "测试消息".into(), 5, false);

    // top_k = 0
    let results = mem.search_by_text("测试", 0);
    assert!(results.is_empty(), "top_k=0 应返回空");

    // top_k 大于条目数
    let results = mem.search_by_text("测试", 100);
    assert_eq!(results.len(), 1, "不应返回超过实际条目数");
}

/// ShortTermMemory mark_recalled 测试
#[tokio::test]
async fn test_short_term_memory_mark_recalled() {
    let mut mem = ShortTermMemory::new();
    let id = mem.store("user".into(), "重要信息".into(), 10, false);

    // 初始 recall_count 为 0
    assert_eq!(mem.all_entries()[0].recall_count, 0);

    // 标记召回
    mem.mark_recalled(&id);
    mem.mark_recalled(&id);
    assert_eq!(mem.all_entries()[0].recall_count, 2, "recall_count 应为 2");
}

/// ShortTermMemory 按轮次范围获取测试
#[tokio::test]
async fn test_short_term_memory_get_by_range() {
    let mut mem = ShortTermMemory::new();
    mem.store("user".into(), "第1轮".into(), 5, false);
    mem.store("assistant".into(), "第2轮".into(), 10, false);
    mem.store("user".into(), "第3轮".into(), 5, false);
    mem.store("assistant".into(), "第4轮".into(), 10, false);

    let range = mem.get_by_range(2, 3);
    assert_eq!(range.len(), 2, "轮次 2-3 应有 2 条");
}

/// HeatInput 冷热评分测试
#[tokio::test]
async fn test_heat_scoring() {
    let weights = HeatWeights::default();

    // 最近 + 高相关性 = 热
    let hot_input = HeatInput {
        elapsed_turns: 0,
        relevance: 0.9,
        recall_count: 5,
        is_tool_result: true,
        tool_importance: 0.8,
    };
    let hot_score = compute_heat(&hot_input, &weights);
    assert!(hot_score > 0.5, "最近的高相关性消息应是热的: {}", hot_score);

    // 很旧 + 低相关性 = 冷
    let cold_input = HeatInput {
        elapsed_turns: 100,
        relevance: 0.1,
        recall_count: 0,
        is_tool_result: false,
        tool_importance: 0.0,
    };
    let cold_score = compute_heat(&cold_input, &weights);
    assert!(cold_score < 0.3, "很旧的低相关性消息应是冷的: {}", cold_score);

    // 工具调用结果应比纯对话更热
    let base_input = HeatInput {
        elapsed_turns: 10,
        relevance: 0.5,
        recall_count: 0,
        is_tool_result: false,
        tool_importance: 0.0,
    };
    let tool_input = HeatInput {
        is_tool_result: true,
        tool_importance: 0.9,
        ..base_input.clone()
    };
    assert!(
        compute_heat(&tool_input, &weights) > compute_heat(&base_input, &weights),
        "工具调用结果应比纯对话更热"
    );
}

/// HeatInput 温度等级分类测试
#[tokio::test]
async fn test_heat_classify() {
    let thresholds = HeatThresholds::default();

    assert_eq!(classify(0.6, &thresholds), Temperature::Hot, "0.6 应为 Hot");
    assert_eq!(classify(0.3, &thresholds), Temperature::Warm, "0.3 应为 Warm");
    assert_eq!(classify(0.1, &thresholds), Temperature::Cold, "0.1 应为 Cold");
}

/// SlidingWindow 更新测试
#[tokio::test]
async fn test_sliding_window_update() {
    // 设置较小的 token 预算
    let window = SlidingWindow::new(200);

    let messages = vec![
        MessageMeta {
            elapsed_turns: 0,
            relevance: 0.9,
            recall_count: 3,
            is_tool_result: true,
            tool_importance: 0.8,
            role: "assistant".into(),
            content: "这是最近的工具调用结果，包含重要信息".into(),
            token_count: 30,
            source_range: (0, 1),
        },
        MessageMeta {
            elapsed_turns: 5,
            relevance: 0.5,
            recall_count: 1,
            is_tool_result: false,
            tool_importance: 0.0,
            role: "user".into(),
            content: "中等时效的对话消息".into(),
            token_count: 20,
            source_range: (1, 2),
        },
        MessageMeta {
            elapsed_turns: 50,
            relevance: 0.1,
            recall_count: 0,
            is_tool_result: false,
            tool_importance: 0.0,
            role: "user".into(),
            content: "很早之前的消息，已经不重要了".into(),
            token_count: 25,
            source_range: (2, 3),
        },
    ];

    let entries = window.update(&messages);

    // 应至少有一个 Hot 条目（最近的高相关性消息）
    let hot_count = entries.iter().filter(|e| matches!(e, WorkingEntry::Hot { .. })).count();
    assert!(hot_count >= 1, "应至少有 1 个 Hot 条目");

    // 总 token 不超过预算
    let total_tokens: u32 = entries.iter().map(|e| e.token_count()).sum();
    assert!(total_tokens <= 200, "总 token 数不应超过预算，实际: {}", total_tokens);
}

/// SlidingWindow 空 messages 测试
#[tokio::test]
async fn test_sliding_window_empty() {
    let window = SlidingWindow::new(1000);
    let entries = window.update(&[]);
    assert!(entries.is_empty(), "空输入应返回空条目");
}

// ============================================================================
// D) Agent 类型解析测试
// ============================================================================

/// AgentType::from_str 各种输入
#[test]
fn test_agent_type_from_str() {
    assert_eq!("primary".parse::<AgentType>().unwrap(), AgentType::Primary);
    assert_eq!("general-purpose".parse::<AgentType>().unwrap(), AgentType::GeneralPurpose);
    assert_eq!("general".parse::<AgentType>().unwrap(), AgentType::GeneralPurpose);
    assert_eq!("explore".parse::<AgentType>().unwrap(), AgentType::Explore);
    assert_eq!("plan".parse::<AgentType>().unwrap(), AgentType::Plan);
    assert_eq!("codex".parse::<AgentType>().unwrap(), AgentType::Codex);

    // 未知类型默认为 GeneralPurpose
    assert_eq!("unknown".parse::<AgentType>().unwrap(), AgentType::GeneralPurpose);
    assert_eq!("".parse::<AgentType>().unwrap(), AgentType::GeneralPurpose);
    assert_eq!("Primary".parse::<AgentType>().unwrap(), AgentType::GeneralPurpose, "区分大小写");
}

/// PermissionMode 转换测试
#[test]
fn test_permission_mode() {
    // 验证枚举值
    assert_ne!(PermissionMode::Yolo, PermissionMode::Trust);
    assert_ne!(PermissionMode::Trust, PermissionMode::Ask);
    assert_ne!(PermissionMode::Yolo, PermissionMode::Ask);

    // 序列化/反序列化一致性
    let modes = vec![PermissionMode::Yolo, PermissionMode::Trust, PermissionMode::Ask];
    for mode in &modes {
        let json = serde_json::to_string(mode).unwrap();
        let decoded: PermissionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(*mode, decoded, "PermissionMode 序列化/反序列化应一致");
    }
}

// ============================================================================
// E) Provider 配置测试
// ============================================================================

/// ProviderConfig 模板生成测试
#[test]
fn test_provider_config_templates() {
    // Ccode 默认配置
    let ccode = ProviderConfig::default_ccode();
    assert_eq!(ccode.name, "ccode");
    assert_eq!(ccode.adapter, ProviderAdapter::Ccode);
    assert!(ccode.models.contains(&"ccode-3".to_string()));
    assert!(ccode.models.contains(&"ccode-3-fast".to_string()));
    assert_eq!(ccode.fallback, vec!["deepseek".to_string()]);
    assert_eq!(ccode.rate_limit, Some(60));

    // Claude 模板
    let claude = ProviderConfig::claude_template();
    assert_eq!(claude.name, "claude");
    assert_eq!(claude.adapter, ProviderAdapter::Claude);
    assert!(claude.models.iter().any(|m| m.contains("claude")));
    assert_eq!(claude.api_version, Some("2023-06-01".to_string()));

    // DeepSeek 模板
    let deepseek = ProviderConfig::deepseek_template();
    assert_eq!(deepseek.name, "deepseek");
    assert_eq!(deepseek.adapter, ProviderAdapter::OpenAI);
    assert!(deepseek.fallback.is_empty());

    // GLM 模板
    let glm = ProviderConfig::glm_template();
    assert_eq!(glm.name, "glm");
    assert_eq!(glm.adapter, ProviderAdapter::GLM);
    assert!(glm.models.contains(&"glm-4".to_string()));

    // Kimi 模板
    let kimi = ProviderConfig::kimi_template();
    assert_eq!(kimi.name, "kimi");
    assert_eq!(kimi.adapter, ProviderAdapter::Kimi);

    // 千问模板
    let qianwen = ProviderConfig::qianwen_template();
    assert_eq!(qianwen.name, "qianwen");
    assert_eq!(qianwen.adapter, ProviderAdapter::Qianwen);
}

/// ProviderConfig 序列化/反序列化一致性
#[test]
fn test_provider_config_serialization() {
    let ccode = ProviderConfig::default_ccode();
    let json = serde_json::to_string(&ccode).unwrap();
    let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.name, ccode.name);
    assert_eq!(decoded.models, ccode.models);
    assert_eq!(decoded.fallback, ccode.fallback);
}

/// ProviderRouter 模型查找测试
#[test]
fn test_provider_router_model_lookup() {
    let configs = vec![
        ProviderConfig::default_ccode(),
        ProviderConfig::deepseek_template(),
    ];

    let mut router = ProviderRouter::from_configs(&configs);

    // 精确查找 ccode 模型
    let provider = router.find_provider_name("ccode-3-fast");
    assert!(provider.is_some(), "应找到 ccode-3-fast 对应的 Provider");
    assert_eq!(provider.unwrap(), "ccode");

    // 查找 deepseek 模型
    let provider = router.find_provider_name("deepseek-chat");
    assert!(provider.is_some(), "应找到 deepseek-chat 对应的 Provider");
    assert_eq!(provider.unwrap(), "deepseek");

    // 不存在的模型
    let provider = router.find_provider_name("nonexistent-model-xyz");
    assert!(provider.is_none(), "不存在的模型应返回 None");
}

/// ProviderRouter 可用模型列表测试
#[test]
fn test_provider_router_available_models() {
    let configs = vec![
        ProviderConfig::default_ccode(),
        ProviderConfig::deepseek_template(),
    ];

    let router = ProviderRouter::from_configs(&configs);
    let models = router.available_models();

    assert!(models.contains(&"ccode-3".to_string()), "应包含 ccode-3");
    assert!(models.contains(&"deepseek-chat".to_string()), "应包含 deepseek-chat");
    assert_eq!(router.provider_count(), 2, "应有 2 个 Provider");
}
