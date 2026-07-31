//! Topic 命名与路由
//!
//! Topic 命名规范：{domain}/{node_type}/{node_id}/{action}
//! 支持通配符：* 匹配单段，** 匹配多段

use serde::{Deserialize, Serialize};

/// Topic 最大深度限制（防止栈溢出）
const MAX_TOPIC_DEPTH: usize = 100;

/// Topic 最大段长度
const MAX_SEGMENT_LENGTH: usize = 100;

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
        debug_assert!(
            t.split('/').all(|seg| seg.len() <= MAX_SEGMENT_LENGTH),
            "topic 段长度超过 {} 限制",
            MAX_SEGMENT_LENGTH
        );
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

    /// Kernel 通知订阅者有新 Publisher 上线
    pub fn sys_publisher_change() -> Self {
        Self::new("sys/publisher_change")
    }

    /// 消息确认（ACK）
    pub fn sys_ack() -> Self {
        Self::new("sys/ack")
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

    /// Agent 权限请求（ToolNode → Acp/TUI）
    pub fn agent_permission(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/permission"))
    }

    /// Agent 取消（Acp/TUI → Thinker/Sampler/Tool）
    pub fn agent_cancel(agent_id: &str) -> Self {
        Self::new(format!("agent/{agent_id}/cancel"))
    }

    // ---- SubAgent Topic ----

    /// 向指定子 Agent 派发任务
    pub fn subagent_task(subagent_id: &str) -> Self {
        Self::new(format!("subagent/{subagent_id}/task"))
    }

    /// 子 Agent 的输出流
    pub fn subagent_output(subagent_id: &str) -> Self {
        Self::new(format!("subagent/{subagent_id}/output"))
    }

    /// 子 Agent 请求执行工具
    pub fn subagent_tool_call(subagent_id: &str) -> Self {
        Self::new(format!("subagent/{subagent_id}/tool_call"))
    }

    /// 工具执行结果返回给子 Agent
    pub fn subagent_tool_result(subagent_id: &str) -> Self {
        Self::new(format!("subagent/{subagent_id}/tool_result"))
    }

    /// 子 Agent 完成事件（携带最终输出）
    pub fn subagent_completed(subagent_id: &str) -> Self {
        Self::new(format!("subagent/{subagent_id}/completed"))
    }

    /// 子 Agent 崩溃事件（携带错误信息）
    pub fn subagent_crashed(subagent_id: &str) -> Self {
        Self::new(format!("subagent/{subagent_id}/crashed"))
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

    /// 取消进行中的采样
    pub fn sampler_cancel(request_id: &str) -> Self {
        Self::new(format!("sampler/{request_id}/cancel"))
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

    /// 请求压缩会话上下文
    pub fn state_compact() -> Self {
        Self::new("state/compact")
    }

    // ---- Tool Topic ----

    /// 工具注册
    pub fn tool_register() -> Self {
        Self::new("tool/register")
    }

    // ---- Service Topic (ROS 风格) ----

    /// 服务请求
    pub fn service_request(service_name: &str) -> Self {
        Self::new(format!("service/{service_name}/request"))
    }

    /// 服务响应
    pub fn service_response(service_name: &str) -> Self {
        Self::new(format!("service/{service_name}/response"))
    }

    // ---- 仿生器官 Topic ----

    /// EyeNode：观察请求/结果
    pub fn eye_observe() -> Self { Self::new("eye/observe") }
    /// EyeNode：文件变化事件
    pub fn eye_file_changed() -> Self { Self::new("eye/file_changed") }
    /// EyeNode：终端输出流
    pub fn eye_terminal_output() -> Self { Self::new("eye/terminal_output") }

    /// EarNode：用户输入
    pub fn ear_hear() -> Self { Self::new("ear/hear") }
    /// EarNode：系统通知
    pub fn ear_notification() -> Self { Self::new("ear/notification") }

    /// NoseNode：嗅探结果
    pub fn nose_smell() -> Self { Self::new("nose/smell") }
    /// NoseNode：编译错误
    pub fn nose_compile_error() -> Self { Self::new("nose/compile_error") }
    /// NoseNode：测试失败
    pub fn nose_test_failure() -> Self { Self::new("nose/test_failure") }

    /// SkinNode：触觉反馈（工具结果/进程退出）
    pub fn skin_touch() -> Self { Self::new("skin/touch") }
    /// SkinNode：进程退出
    pub fn skin_process_exit() -> Self { Self::new("skin/process_exit") }
    /// SkinNode：内存压力
    pub fn skin_memory_pressure() -> Self { Self::new("skin/memory_pressure") }

    /// MouthNode：文本输出
    pub fn mouth_speak() -> Self { Self::new("mouth/speak") }
    /// MouthNode：代码写入
    pub fn mouth_code_write() -> Self { Self::new("mouth/code_write") }
    /// MouthNode：状态报告
    pub fn mouth_status() -> Self { Self::new("mouth/status") }

    /// HandNode：编辑操作
    pub fn hand_edit() -> Self { Self::new("hand/edit") }
    /// HandNode：搜索操作
    pub fn hand_search() -> Self { Self::new("hand/search") }
    /// HandNode：重构操作
    pub fn hand_restructure() -> Self { Self::new("hand/restructure") }

    /// LimbNode：命令执行
    pub fn limb_execute() -> Self { Self::new("limb/execute") }
    /// LimbNode：构建操作
    pub fn limb_build() -> Self { Self::new("limb/build") }
    /// LimbNode：Git 操作
    pub fn limb_git() -> Self { Self::new("limb/git") }

    /// ThinkerNode：思考指令
    pub fn cortex_think(agent_id: &str) -> Self { Self::new(format!("cortex/{agent_id}/think")) }
    /// ThinkerNode：规划指令
    pub fn cortex_plan(agent_id: &str) -> Self { Self::new(format!("cortex/{agent_id}/plan")) }
    /// ThinkerNode：决策指令
    pub fn cortex_decide(agent_id: &str) -> Self { Self::new(format!("cortex/{agent_id}/decide")) }
    /// ThinkerNode：感官综合信号
    pub fn cortex_sensory(agent_id: &str) -> Self { Self::new(format!("cortex/{agent_id}/sensory")) }
    /// ThinkerNode：输入（替代 agent/{id}/input）
    pub fn cortex_input(agent_id: &str) -> Self { Self::new(format!("cortex/{agent_id}/input")) }
    /// ThinkerNode：说话指令（输出到 MouthNode）
    pub fn cortex_speak(agent_id: &str) -> Self { Self::new(format!("cortex/{agent_id}/speak")) }

    // ---- Param Topic (ROS 风格参数服务器) ----

    /// 设置参数
    pub fn param_set() -> Self {
        Self::new("param/set")
    }

    /// 获取参数
    pub fn param_get() -> Self {
        Self::new("param/get")
    }

    /// 参数变更通知
    pub fn param_changed() -> Self {
        Self::new("param/changed")
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

    /// 匹配所有 agent 权限请求
    pub fn all_agent_permissions() -> Self {
        Self::new("agent/*/permission")
    }

    /// 匹配所有 agent 取消
    pub fn all_agent_cancels() -> Self {
        Self::new("agent/*/cancel")
    }

    /// 判断 topic 是否匹配此模式
    pub fn matches(&self, topic: &Topic) -> bool {
        topic_matches(&self.0, topic.as_str())
    }
}

/// 通配符匹配：* 匹配单段，** 匹配多段
/// 带深度限制（防止栈溢出）
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
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
    
    match_parts(&pattern_parts, &topic_parts, 0)
}

/// 递归匹配（带深度检查）
fn match_parts(pattern: &[&str], topic: &[&str], depth: usize) -> bool {
    // ✅ 深度检查
    if depth > MAX_TOPIC_DEPTH {
        return false;
    }
    
    match (pattern.first(), topic.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            // ** 匹配零或多个段
            match_parts(&pattern[1..], topic, depth + 1)
                || (!topic.is_empty() && match_parts(pattern, &topic[1..], depth + 1))
        }
        (Some(&"*"), Some(_)) => match_parts(&pattern[1..], &topic[1..], depth + 1),
        (Some(p), Some(t)) if p == t => match_parts(&pattern[1..], &topic[1..], depth + 1),
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
