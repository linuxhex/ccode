//! Topic 命名与路由
//!
//! Topic 命名规范：{domain}/{node_type}/{node_id}/{action}
//! 支持通配符：* 匹配单段，** 匹配多段

use serde::{Deserialize, Serialize};

/// 消息 Topic，标识消息的路由目的地
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Topic(String);

/// Topic 匹配模式，支持通配符
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TopicPattern(String);

impl Topic {
    pub fn new(topic: impl Into<String>) -> Self {
        let t = topic.into();
        debug_assert!(!t.is_empty(), "topic 不能为空");
        Self(t)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    // ---- 系统 Topic ----

    /// 所有 Node 向 Kernel 发送心跳
    pub fn sys_heartbeat() -> Self {
        Self::new("sys/heartbeat")
    }

    /// 新 Node 注册
    pub fn sys_register() -> Self {
        Self::new("sys/register")
    }

    /// Node 注销
    pub fn sys_deregister() -> Self {
        Self::new("sys/deregister")
    }

    /// Kernel 广播新 Node 上线
    pub fn sys_spawn() -> Self {
        Self::new("sys/spawn")
    }

    /// Kernel 广播全局关闭
    pub fn sys_shutdown() -> Self {
        Self::new("sys/shutdown")
    }

    // ---- Agent Topic ----

    /// 向指定 Agent 发送输入
    pub fn agent_input(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/input"))
    }

    /// 指定 Agent 的输出流
    pub fn agent_output(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/output"))
    }

    /// 指定 Agent 请求执行工具
    pub fn agent_tool_call(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/tool_call"))
    }

    /// 工具执行结果返回给 Agent
    pub fn agent_tool_result(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/tool_result"))
    }

    /// 请求 Kernel 创建子 Agent
    pub fn agent_spawn(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/spawn"))
    }

    /// Agent 状态事件（崩溃、完成等）
    pub fn agent_event(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/event"))
    }

    // ---- Sampler Topic ----

    /// Agent 向 Sampler 请求采样
    pub fn sampler_request() -> Self {
        Self::new("sampler/request")
    }

    /// Sampler 流式返回指定请求
    pub fn sampler_stream(request_id: &str) -> Self {
        Self::new(format!("sampler/{request_id}/stream"))
    }

    // ---- State Topic ----

    /// 查询对话状态
    pub fn state_query() -> Self {
        Self::new("state/query")
    }

    /// 状态查询响应
    pub fn state_response() -> Self {
        Self::new("state/response")
    }

    /// 持久化对话
    pub fn state_persist() -> Self {
        Self::new("state/persist")
    }

    // ---- Tool Topic ----

    /// 工具注册
    pub fn tool_register() -> Self {
        Self::new("tool/register")
    }
}

impl TopicPattern {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self(pattern.into())
    }

    /// 匹配所有 agent 输出
    pub fn all_agent_outputs() -> Self {
        Self::new("agent/*/output")
    }

    /// 匹配指定 agent 的所有消息
    pub fn agent_all(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/*"))
    }

    /// 匹配所有 sampler 流式返回
    pub fn all_sampler_streams() -> Self {
        Self::new("sampler/*/stream")
    }

    /// 判断 topic 是否匹配此模式
    pub fn matches(&self, topic: &Topic) -> bool {
        topic_matches(&self.0, topic.as_str())
    }
}

/// 通配符匹配：* 匹配单段，** 匹配多段
fn topic_matches(pattern: &str, topic: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();
    match_parts(&pattern_parts, &topic_parts)
}

fn match_parts(pattern: &[&str], topic: &[&str]) -> bool {
    match (pattern.first(), topic.first()) {
        (None, None) => true,
        (Some("**"), _) => {
            // ** 匹配零或多个段
            match_parts(&pattern[1..], topic)
                || (!topic.is_empty() && match_parts(pattern, &topic[1..]))
        }
        (Some("*"), Some(_)) => match_parts(&pattern[1..], &topic[1..]),
        (Some(p), Some(t)) if p == t => match_parts(&pattern[1..], &topic[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let pattern = TopicPattern::new("agent/abc/input");
        let topic = Topic::agent_input("abc");
        assert!(pattern.matches(&topic));
    }

    #[test]
    fn test_wildcard_single() {
        let pattern = TopicPattern::all_agent_outputs();
        assert!(pattern.matches(&Topic::agent_output("abc")));
        assert!(pattern.matches(&Topic::agent_output("xyz")));
        assert!(!pattern.matches(&Topic::agent_input("abc")));
    }

    #[test]
    fn test_wildcard_multi() {
        let pattern = TopicPattern::new("agent/**");
        assert!(pattern.matches(&Topic::agent_input("abc")));
        assert!(pattern.matches(&Topic::agent_output("abc")));
    }
}
