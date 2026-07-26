//! ROS 风格的参数服务器
//!
//! 全局共享的键值存储，类似 ROS 的 Parameter Server：
//! - 所有 Node 可以设置/获取参数
//! - 参数变更时通知订阅者
//! - 支持参数命名空间（层级结构）
//!
//! 参数 Topic：
//! - param/set：设置参数
//! - param/get：获取参数
//! - param/changed：参数变更通知

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 参数值（支持多种类型）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Integer(i64),
    Float(f64),
    Boolean(bool),
    String(String),
    Array(Vec<ParamValue>),
    Map(HashMap<String, ParamValue>),
    Null,
}

/// 参数服务器
pub struct ParamServer {
    /// 参数存储
    params: HashMap<String, ParamValue>,
}

impl Default for ParamServer {
    fn default() -> Self {
        Self::new()
    }
}

impl ParamServer {
    pub fn new() -> Self {
        Self {
            params: HashMap::new(),
        }
    }

    /// 设置参数
    pub fn set(&mut self, key: impl Into<String>, value: ParamValue) {
        let key = key.into();
        tracing::debug!("参数设置：{} = {:?}", key, value);
        self.params.insert(key, value);
    }

    /// 获取参数
    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        self.params.get(key)
    }

    /// 删除参数
    pub fn delete(&mut self, key: &str) -> Option<ParamValue> {
        self.params.remove(key)
    }

    /// 检查参数是否存在
    pub fn has(&self, key: &str) -> bool {
        self.params.contains_key(key)
    }

    /// 列出所有参数名
    pub fn list(&self) -> Vec<&str> {
        self.params.keys().map(|s| s.as_str()).collect()
    }

    /// 列出指定前缀下的参数名
    pub fn list_prefix(&self, prefix: &str) -> Vec<&str> {
        self.params
            .keys()
            .filter(|k| k.starts_with(prefix))
            .map(|s| s.as_str())
            .collect()
    }

    /// 搜索参数（支持简单通配符 *，匹配单个路径段）
    pub fn search<'a>(&'a self, pattern: &'a str) -> Vec<(&'a str, &'a ParamValue)> {
        if pattern.contains('*') {
            // 简单的通配符匹配：将 * 替换为任意单段
            self.params
                .iter()
                .filter(|(k, _)| {
                    let pattern_parts: Vec<&str> = pattern.split('/').collect();
                    let key_parts: Vec<&str> = k.split('/').collect();
                    if pattern_parts.len() != key_parts.len() {
                        return false;
                    }
                    for (p, k) in pattern_parts.iter().zip(key_parts.iter()) {
                        if *p != "*" && p != k {
                            return false;
                        }
                    }
                    true
                })
                .map(|(k, v)| (k.as_str(), v))
                .collect()
        } else {
            self.params
                .get(pattern)
                .map(|v| vec![(pattern, v)])
                .unwrap_or_default()
        }
    }

    /// 从 serde_json::Value 获取参数值
    pub fn get_value(&self, key: &str) -> Option<&ParamValue> {
        self.params.get(key)
    }

    /// 获取参数数量
    pub fn len(&self) -> usize {
        self.params.len()
    }

    /// 判断参数是否为空
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }
}

/// 参数变更通知
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamChangeNotification {
    /// 变更的参数键
    pub key: String,
    /// 新值
    pub new_value: Option<ParamValue>,
    /// 变更类型
    pub change_type: ParamChangeType,
}

/// 参数变更类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParamChangeType {
    /// 新增参数
    Created,
    /// 修改参数
    Updated,
    /// 删除参数
    Deleted,
}
