//! World State diff — 仅重发变化的 context fragment
//!
//! 借鉴 Codex 的 World State 模式。追踪系统提示中各 fragment 的内容指纹，
//! 每次构建请求时仅重发变化的 fragment，保持前缀字节稳定以最大化 prompt cache 命中。
//!
//! ## 工作原理
//!
//! 1. 系统提示被分解为多个 `Fragment`（每个有唯一 key）
//! 2. 每个 fragment 的内容计算 FNV-1a 指纹
//! 3. 构建请求时，仅当 fragment 指纹变化时才更新对应的消息内容
//! 4. 未变化的 fragment 保持字节不变 → API 侧 prompt cache 命中

use std::collections::HashMap;

/// FNV-1a 哈希（64 位）
fn fnv1a_hash(content: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in content.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// 上下文 fragment — 系统提示的一个逻辑分区
#[derive(Debug, Clone)]
pub struct Fragment {
    /// 唯一标识（如 "system_prompt", "environment", "agents_md", "memory"）
    pub key: String,
    /// fragment 内容
    pub content: String,
    /// 内容指纹（FNV-1a hash）
    pub fingerprint: u64,
    /// 缓存优先级
    pub priority: FragmentPriority,
}

/// Fragment 缓存优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FragmentPriority {
    /// 几乎不变（系统提示核心）
    Immutable,
    /// 偶尔变化（环境信息、用户指令）
    Stable,
    /// 频繁变化（记忆注入、动态上下文）
    Variable,
}

impl Fragment {
    pub fn new(key: impl Into<String>, content: impl Into<String>, priority: FragmentPriority) -> Self {
        let content = content.into();
        let fingerprint = fnv1a_hash(&content);
        Self {
            key: key.into(),
            content,
            fingerprint,
            priority,
        }
    }

    /// 检查内容是否变化
    pub fn has_changed(&self, new_content: &str) -> bool {
        self.fingerprint != fnv1a_hash(new_content)
    }
}

/// World State — 追踪所有 fragment 的状态
///
/// 维护 fragment 的当前指纹，检测变化，生成 diff。
#[derive(Debug, Clone)]
pub struct WorldState {
    fragments: HashMap<String, Fragment>,
    /// 按优先级排序的 key 列表（用于稳定输出顺序）
    ordered_keys: Vec<String>,
}

impl Default for WorldState {
    fn default() -> Self {
        Self::new()
    }
}

impl WorldState {
    pub fn new() -> Self {
        Self {
            fragments: HashMap::new(),
            ordered_keys: Vec::new(),
        }
    }

    /// 注册或更新一个 fragment。返回 `true` 如果内容发生了变化。
    pub fn update_fragment(&mut self, fragment: Fragment) -> bool {
        let key = fragment.key.clone();
        let changed = match self.fragments.get(&key) {
            Some(existing) => existing.fingerprint != fragment.fingerprint,
            None => true,
        };

        if !self.fragments.contains_key(&key) {
            self.ordered_keys.push(key.clone());
        }

        self.fragments.insert(key, fragment);

        if self.ordered_keys.len() > 1 {
            self.ordered_keys.sort_by(|a, b| {
                let pa = self.fragments.get(a).map(|f| f.priority).unwrap_or(FragmentPriority::Variable);
                let pb = self.fragments.get(b).map(|f| f.priority).unwrap_or(FragmentPriority::Variable);
                pa.cmp(&pb).then_with(|| a.cmp(b))
            });
        }
        changed
    }

    /// 获取所有变化的 fragment key（与上次快照比较）
    pub fn changed_keys(&self, previous_fingerprints: &HashMap<String, u64>) -> Vec<String> {
        self.ordered_keys.iter()
            .filter(|key| {
                match (self.fragments.get(*key), previous_fingerprints.get(*key)) {
                    (Some(current), Some(prev)) => current.fingerprint != *prev,
                    (Some(_), None) => true,
                    _ => false,
                }
            })
            .cloned()
            .collect()
    }

    /// 获取当前所有 fragment 的指纹快照
    pub fn fingerprint_snapshot(&self) -> HashMap<String, u64> {
        self.fragments.iter()
            .map(|(k, v)| (k.clone(), v.fingerprint))
            .collect()
    }

    /// 获取按优先级排序的所有 fragment 内容
    pub fn ordered_content(&self) -> Vec<&Fragment> {
        self.ordered_keys.iter()
            .filter_map(|key| self.fragments.get(key))
            .collect()
    }

    /// 组装完整系统提示（按优先级排序拼接）
    pub fn assemble_system_prompt(&self) -> String {
        self.ordered_content()
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// fragment 数量
    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    /// 获取指定 fragment
    pub fn get(&self, key: &str) -> Option<&Fragment> {
        self.fragments.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_creation() {
        let f = Fragment::new("test", "hello world", FragmentPriority::Stable);
        assert_eq!(f.key, "test");
        assert_eq!(f.content, "hello world");
        assert!(f.fingerprint != 0);
    }

    #[test]
    fn fragment_change_detection() {
        let f = Fragment::new("test", "hello", FragmentPriority::Stable);
        assert!(!f.has_changed("hello"));
        assert!(f.has_changed("world"));
    }

    #[test]
    fn world_state_update() {
        let mut ws = WorldState::new();
        let f1 = Fragment::new("sys", "system prompt", FragmentPriority::Immutable);
        assert!(ws.update_fragment(f1)); // first insert = changed

        let f2 = Fragment::new("sys", "system prompt", FragmentPriority::Immutable);
        assert!(!ws.update_fragment(f2)); // same content = not changed

        let f3 = Fragment::new("sys", "updated system prompt", FragmentPriority::Immutable);
        assert!(ws.update_fragment(f3)); // different content = changed
    }

    #[test]
    fn world_state_ordering() {
        let mut ws = WorldState::new();
        ws.update_fragment(Fragment::new("var", "variable", FragmentPriority::Variable));
        ws.update_fragment(Fragment::new("imm", "immutable", FragmentPriority::Immutable));
        ws.update_fragment(Fragment::new("sta", "stable", FragmentPriority::Stable));

        let ordered = ws.ordered_content();
        assert_eq!(ordered[0].key, "imm");
        assert_eq!(ordered[1].key, "sta");
        assert_eq!(ordered[2].key, "var");
    }

    #[test]
    fn changed_keys_detection() {
        let mut ws = WorldState::new();
        ws.update_fragment(Fragment::new("a", "content_a", FragmentPriority::Stable));
        ws.update_fragment(Fragment::new("b", "content_b", FragmentPriority::Stable));

        let snap1 = ws.fingerprint_snapshot();

        ws.update_fragment(Fragment::new("a", "content_a_changed", FragmentPriority::Stable));

        let changed = ws.changed_keys(&snap1);
        assert_eq!(changed, vec!["a"]);
    }

    #[test]
    fn system_prompt_assembly() {
        let mut ws = WorldState::new();
        ws.update_fragment(Fragment::new("env", "OS: macOS", FragmentPriority::Stable));
        ws.update_fragment(Fragment::new("sys", "You are helpful.", FragmentPriority::Immutable));

        let prompt = ws.assemble_system_prompt();
        // Immutable should come first
        let sys_pos = prompt.find("You are helpful.").unwrap();
        let env_pos = prompt.find("OS: macOS").unwrap();
        assert!(sys_pos < env_pos);
    }

    #[test]
    fn fnv1a_deterministic() {
        assert_eq!(fnv1a_hash("hello"), fnv1a_hash("hello"));
        assert_ne!(fnv1a_hash("hello"), fnv1a_hash("world"));
    }
}
