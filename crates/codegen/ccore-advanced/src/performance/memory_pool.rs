//! 内存池实现
//!
//! 池化消息和缓冲区，减少频繁内存分配。
//! 使用 crossbeam-queue 实现无锁队列。

use crossbeam_queue::ArrayQueue;
use std::sync::Arc;
use tracing::debug;

/// 消息内存池
///
/// 预分配固定大小的消息缓冲区，重复利用减少分配开销。
pub struct MessagePool {
    /// 消息缓冲区队列
    pool: Arc<ArrayQueue<Vec<u8>>>,
    /// 单个缓冲区大小
    buffer_size: usize,
    /// 池容量
    capacity: usize,
}

impl MessagePool {
    /// 创建消息内存池
    ///
    /// # 参数
    /// - capacity: 池容量（预分配数量）
    /// - buffer_size: 单个缓冲区大小（字节）
    pub fn new(capacity: usize, buffer_size: usize) -> Self {
        let pool = Arc::new(ArrayQueue::new(capacity));

        // 预分配缓冲区
        for _ in 0..capacity {
            let _ = pool.push(Vec::with_capacity(buffer_size));
        }

        debug!(
            capacity,
            buffer_size,
            total_bytes = capacity * buffer_size,
            "消息内存池已创建"
        );

        Self {
            pool,
            buffer_size,
            capacity,
        }
    }

    /// 获取一个缓冲区
    ///
    /// 优先从池中获取，池空时分配新缓冲区
    pub fn acquire(&self) -> Vec<u8> {
        match self.pool.pop() {
            Some(mut buf) => {
                buf.clear();
                buf
            }
            None => Vec::with_capacity(self.buffer_size),
        }
    }

    /// 归还缓冲区到池中
    ///
    /// 池满时缓冲区被丢弃（由 GC 回收）
    pub fn release(&self, mut buf: Vec<u8>) {
        buf.clear();
        if buf.capacity() > self.buffer_size * 2 {
            // 缓冲区过大，不回收
            return;
        }
        let _ = self.pool.push(buf);
    }

    /// 获取池使用率
    pub fn usage(&self) -> f32 {
        let used = self.capacity - self.pool.len();
        used as f32 / self.capacity as f32
    }

    /// 获取池容量
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 获取当前可用缓冲区数
    pub fn available(&self) -> usize {
        self.pool.len()
    }
}

/// 缓冲区守卫
///
/// RAII 模式，drop 时自动归还缓冲区到池中
pub struct BufferGuard<'a> {
    buf: Vec<u8>,
    pool: &'a MessagePool,
}

impl<'a> BufferGuard<'a> {
    pub fn new(pool: &'a MessagePool) -> Self {
        Self {
            buf: pool.acquire(),
            pool,
        }
    }

    pub fn as_mut(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }

    pub fn as_ref(&self) -> &Vec<u8> {
        &self.buf
    }
}

impl<'a> Drop for BufferGuard<'a> {
    fn drop(&mut self) {
        let buf = std::mem::take(&mut self.buf);
        self.pool.release(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_pool() {
        let pool = MessagePool::new(10, 1024);

        let mut buf = pool.acquire();
        buf.extend_from_slice(b"hello");
        pool.release(buf);

        let buf2 = pool.acquire();
        assert!(buf2.is_empty());
    }

    #[test]
    fn test_buffer_guard() {
        let pool = MessagePool::new(10, 1024);
        {
            let mut guard = BufferGuard::new(&pool);
            guard.as_mut().extend_from_slice(b"test");
        }
        // guard drop 后缓冲区应回到池中
        assert_eq!(pool.available(), 10);
    }
}