//! 锁顺序规范（防止死锁）
//!
//! 所有 Mutex/RwLock 必须按以下顺序获取：
//! 1. READ_TRACKER    - tools/read_tracker.rs
//! 2. GITIGNORE       - tools/builtin.rs  
//! 3. BASH_CACHE      - tools/builtin.rs
//! 4. PERMISSION_CHECKER - tools/bridge.rs
//! 5. DreamLock       - memory/dream.rs
//! 6. ConnectionPool  - sampler/pool.rs
//!
//! 违反此顺序可能导致死锁！
//! 
//! 检测方法：运行 `RUST_LOG=ccore::lock=debug` 查看锁获取顺序

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

static LOCK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 记录锁获取事件
/// 
/// 用法：在 Mutex::lock() 后调用
/// ```ignore
/// let guard = mutex.lock().unwrap();
/// lock_order::record_acquire("DreamLock");
/// ```
pub fn record_acquire(lock_name: &str) -> u64 {
    let seq = LOCK_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
    tracing::trace!(target: "ccore::lock", seq, lock = lock_name, "lock acquired");
    seq
}

/// 记录锁释放事件
pub fn record_release(lock_name: &str, seq: u64) {
    tracing::trace!(target: "ccore::lock", seq, lock = lock_name, "lock released");
}

/// 锁顺序检查器（Debug构建时检查）
#[cfg(debug_assertions)]
mod checker {
    use std::cell::RefCell;
    
    thread_local! {
        static HELD_LOCKS: RefCell<Vec<&'static str>> = RefCell::new(Vec::new());
    }
    
    /// 检查锁获取顺序是否合法
    pub fn check_order(lock_name: &'static str) {
        const ORDER: &[&str] = &[
            "READ_TRACKER",
            "GITIGNORE", 
            "BASH_CACHE",
            "PERMISSION_CHECKER",
            "DreamLock",
            "ConnectionPool",
        ];
        
        HELD_LOCKS.with(|held| {
            let held = held.borrow();
            if let Some(new_idx) = ORDER.iter().position(|&l| l == lock_name) {
                for held_lock in held.iter() {
                    if let Some(held_idx) = ORDER.iter().position(|l| l == held_lock) {
                        if new_idx < held_idx {
                            tracing::error!(
                                target: "ccore::lock",
                                new_lock = lock_name,
                                held_lock = %held_lock,
                                "⚠️ LOCK ORDER VIOLATION! {} acquired while {} is held ({} should come before {})",
                                lock_name, held_lock, lock_name, held_lock
                            );
                        }
                    }
                }
            }
        });
    }
    
    pub fn push_held(lock_name: &'static str) {
        HELD_LOCKS.with(|held| {
            held.borrow_mut().push(lock_name);
        });
    }
    
    pub fn pop_held(lock_name: &'static str) {
        HELD_LOCKS.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(pos) = held.iter().rposition(|&l| l == lock_name) {
                held.remove(pos);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_acquire_release() {
        let seq = record_acquire("READ_TRACKER");
        record_release("READ_TRACKER", seq);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_checker_push_pop() {
        checker::push_held("READ_TRACKER");
        checker::pop_held("READ_TRACKER");
    }
}
