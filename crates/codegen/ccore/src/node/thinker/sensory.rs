//! 感官模块 — 听觉/触觉/嗅觉/视觉 内置实现（路线 A：不拆独立进程）

use super::*;

/// 感官信号缓冲最大容量
pub(crate) const SENSORY_BUFFER_CAPACITY: usize = 20;

/// 感官信号（内部使用，不经消息总线）
#[derive(Debug, Clone)]
pub struct SensorySignal {
    /// 来源器官（如 "nose", "skin", "eye"）
    pub source_organ: String,
    /// 信号类型（如 "compile_error", "touch"）
    pub signal_type: String,
    /// 供 LLM 理解的摘要
    pub summary: String,
    /// 严重程度（"info", "warning", "error"）
    pub severity: String,
}

impl ThinkerNode {
    /// 听觉（Ear）：处理用户输入
    ///
    /// 将用户输入存入工作记忆，同时从长期记忆中搜索相关内容注入热区。
    pub(crate) fn listen(&mut self, content: &str, role: &str) {
        let _entry_id = self.short_term_memory.store(
            role.to_string(),
            content.to_string(),
            Self::estimate_tokens(content),
            false,
        );

        self.working_memory.push_hot(
            MessageRole::try_from(role).unwrap_or(MessageRole::User),
            content.to_string(),
            Self::estimate_tokens(content),
        );

        // 从长期记忆搜索相关内容，注入工作记忆（冷区→热区）
        let relevant = self.memory_bridge.search_relevant(content, 5);
        for memory in relevant {
            let token_count = Self::estimate_tokens(&memory);
            self.working_memory.push_system(
                format!("[长期记忆] {}", memory),
                token_count,
            );
            tracing::debug!("注入长期记忆：{} 字符", memory.len());
        }

        // Context Engine：意图扩展 + 代码块检索，注入代码级上下文
        let intents = IntentRetriever::expand_intents(content);
        if !intents.is_empty() {
            // 简单用零向量占位（真实场景需调 embedding 模型生成 query embedding）
            let query_embedding = vec![0.0f32; 1536];
            let results = self.intent_retriever.search_by_intents(&intents, &query_embedding, 5);
            for result in results {
                let token_count = Self::estimate_tokens(&result.preview);
                self.working_memory.push_system(
                    format!("[代码上下文] {} ({}:{}, 相关度:{:.2})", result.name, result.file_path.display(), result.source_intent, result.relevance_score),
                    token_count,
                );
                tracing::debug!("注入代码上下文：{} (score={:.2})", result.name, result.relevance_score);
            }
        }

        self.state = AgentState::Thinking;
        tracing::debug!("Thinker {} 听到输入，轮次 {}", self.id, self.turns_executed);
    }

    /// 触觉（Skin）：感知工具执行结果
    ///
    /// 解析工具输出，提取关键信息注入工作记忆。
    /// 对于编译/检查类工具输出，自动调用 sniff() 解析错误。
    /// 同时通过消息总线向 Kernel 发送感官信号，让反射弧和经验学习有输入。
    pub(crate) async fn feel(&mut self, tool_name: &str, output: &str, success: bool, transport: &NodeTransportHandle) {
        let summary = if success {
            format!("[工具 {} 执行成功]", tool_name)
        } else {
            format!("[工具 {} 执行失败]", tool_name)
        };

        // 记录触觉信号
        self.push_sensory(SensorySignal {
            source_organ: "skin".into(),
            signal_type: "tool_result".into(),
            summary: summary.clone(),
            severity: if success { "info" } else { "error" }.into(),
        });

        // 向 Kernel 发送感官信号（让反射弧和经验学习有输入）
        if let Err(e) = self.publish_sensory_to_kernel(
            &format!("sensory/skin/{}", tool_name),
            &serde_json::json!({
                "tool_name": tool_name,
                "success": success,
                "output_preview": output.chars().take(200).collect::<String>(),
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送感官信号到 Kernel 失败：{}", e);
        }

        // 如果是编译/检查类工具，自动嗅探
        if !success && Self::is_compile_related_tool(tool_name) {
            self.sniff(output, transport).await;
        }
    }

    /// 嗅觉（Nose）：解析编译错误和代码异味
    ///
    /// 从工具输出中提取错误信息，格式化后注入工作记忆。
    /// 同时向 Kernel 发送感官信号，让反射弧和经验学习有输入。
    /// 编译错误格式：error[E0xxx]: message
    pub(crate) async fn sniff(&mut self, output: &str, transport: &NodeTransportHandle) {
        let error_lines: Vec<&str> = output
            .lines()
            .filter(|line| line.contains("error[") || line.contains("error:"))
            .take(5) // 最多取 5 条错误
            .collect();

        if error_lines.is_empty() {
            return;
        }

        let summary = format!("编译/检查发现 {} 个错误：\n{}", error_lines.len(), error_lines.join("\n"));

        self.push_sensory(SensorySignal {
            source_organ: "nose".into(),
            signal_type: "compile_error".into(),
            summary: summary.clone(),
            severity: "error".into(),
        });

        // 注入工作记忆供 LLM 处理（感官信号作为 system 消息）
        let token_count = Self::estimate_tokens(&summary);
        self.working_memory.push_system(summary.clone(), token_count);

        // 向 Kernel 发送感官信号（让反射弧和经验学习有输入）
        if let Err(e) = self.publish_sensory_to_kernel(
            "sensory/nose/compile_error",
            &serde_json::json!({
                "error_count": error_lines.len(),
                "errors": error_lines,
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送嗅探感官信号到 Kernel 失败：{}", e);
        }
    }

    /// 视觉（Eye）：观察工具结果中的文件内容
    ///
    /// 从 Read/Glob/Grep 等工具输出中提取文件内容摘要，
    /// 并向 Kernel 发送感官信号
    pub(crate) async fn observe(&mut self, tool_name: &str, output: &str, transport: &NodeTransportHandle) {
        let summary = match tool_name {
            "Read" => {
                let line_count = output.lines().count();
                format!("[观察到文件内容，共 {} 行]", line_count)
            }
            "Glob" | "Grep" => {
                let match_count = output.lines().count();
                format!("[观察到 {} 条匹配结果]", match_count)
            }
            _ => return, // 非观察类工具，不处理
        };

        self.push_sensory(SensorySignal {
            source_organ: "eye".into(),
            signal_type: "file_observation".into(),
            summary,
            severity: "info".into(),
        });

        // 向 Kernel 发送感官信号
        if let Err(e) = self.publish_sensory_to_kernel(
            &format!("sensory/eye/{}", tool_name.to_lowercase()),
            &serde_json::json!({
                "tool_name": tool_name,
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送视觉感官信号到 Kernel 失败：{}", e);
        }
    }

    /// 向 Kernel 发送感官信号
    ///
    /// ThinkerNode 内置感官处理后，通过控制面消息通知 Kernel，
    /// 让 Kernel 的 ReflexRouter 和 ExperienceLog 有输入源。
    /// topic 格式：sensory/{organ}/{detail}，如 sensory/nose/compile_error
    pub(crate) async fn publish_sensory_to_kernel(
        &self,
        topic: &str,
        payload: &serde_json::Value,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let msg = FrameCodec::new_message(
            Topic::new(topic),
            self.id.as_str(),
            payload,
        )?;
        // 感官信号走控制面（经 Kernel ROUTER），确保 Kernel 一定收到
        transport.send_message(&msg).await?;
        Ok(())
    }

    /// 请求元认知评估（通过消息总线，异步回调）
    ///
    /// ThinkerNode 不直接持有 MetaCognitiveController，
    /// 而是通过 bus 发送 cortex/meta_assess 请求，
    /// Kernel 收到后评估并将结果通过 cortex/meta_result 返回。
    pub(crate) async fn request_meta_assessment(&self, context: &str, transport: &NodeTransportHandle) {
        let msg = FrameCodec::new_message(
            Topic::new("cortex/meta_assess"),
            self.id.as_str(),
            &serde_json::json!({
                "agent_id": self.id.to_string(),
                "context": context,
            }),
        );
        if let Ok(msg) = msg {
            if let Err(e) = transport.send_message(&msg).await {
                tracing::debug!("发送元认知评估请求失败：{}", e);
            }
        }
    }

    /// 判断工具是否与编译/检查相关
    pub(crate) fn is_compile_related_tool(tool_name: &str) -> bool {
        matches!(tool_name, "Bash" | "RunCommand" | "CargoCheck" | "CargoBuild" | "CargoTest")
    }

    /// 向感官缓冲中追加信号
    pub(crate) fn push_sensory(&mut self, signal: SensorySignal) {
        if self.sensory_buffer.len() >= SENSORY_BUFFER_CAPACITY {
            self.sensory_buffer.remove(0);
        }
        self.sensory_buffer.push(signal);
    }
}
