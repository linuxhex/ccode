//! Doom Loop 检测 - 检测 Agent 是否陷入重复循环

use std::collections::{HashMap, VecDeque};
use serde::{Deserialize, Serialize};

/// 工具调用的签名，用于检测重复
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ToolCallSignature {
    pub tool_name: String,
    pub args_hash: u64,
}

/// Doom Loop 检测器
pub struct DoomLoopDetector {
    /// 最近的工具调用签名历史
    history: VecDeque<ToolCallSignature>,
    /// 检测窗口大小
    window_size: usize,
    /// 重复阈值：在窗口内同一签名出现 N 次则判定为 doom loop
    repeat_threshold: usize,
}

/// Doom Loop 检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoomLoopResult {
    pub detected: bool,
    pub repeated_tool: Option<String>,
    pub repeat_count: usize,
}

impl DoomLoopDetector {
    pub fn new(window_size: usize, repeat_threshold: usize) -> Self {
        Self {
            history: VecDeque::new(),
            window_size,
            repeat_threshold,
        }
    }

    /// 记录一次工具调用
    pub fn record(&mut self, tool_name: String, args_hash: u64) {
        self.history.push_back(ToolCallSignature { tool_name, args_hash });
        // 只保留窗口大小的历史
        if self.history.len() > self.window_size {
            self.history.pop_front();
        }
    }

    /// 检测是否存在 doom loop
    pub fn detect(&self) -> DoomLoopResult {
        if self.history.len() < self.repeat_threshold {
            return DoomLoopResult {
                detected: false,
                repeated_tool: None,
                repeat_count: 0,
            };
        }

        // 统计窗口内每个签名出现的次数
        let mut counts: HashMap<ToolCallSignature, usize> = HashMap::new();
        for sig in &self.history {
            *counts.entry(sig.clone()).or_insert(0) += 1;
        }

        // 找到重复最多的签名
        if let Some((sig, count)) = counts.iter().max_by_key(|(_, c)| *c) {
            if *count >= self.repeat_threshold {
                return DoomLoopResult {
                    detected: true,
                    repeated_tool: Some(sig.tool_name.clone()),
                    repeat_count: *count,
                };
            }
        }

        DoomLoopResult {
            detected: false,
            repeated_tool: None,
            repeat_count: 0,
        }
    }

    /// 重置检测器
    pub fn reset(&mut self) {
        self.history.clear();
    }
}
