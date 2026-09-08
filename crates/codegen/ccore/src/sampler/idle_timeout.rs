//! 流空闲超时 — 包装 Stream，每个 chunk 间隔受 timeout 保护
//!
//! 借鉴 ccode-sampler 的 `DEFAULT_IDLE_TIMEOUT_SECS = 300` 和 per-chunk timeout 模式。
//! ccore 遗留 provider 此前无此保护，服务端停发数据时 agent 会永久挂起。

use anyhow::Result;
use futures::stream::Stream;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::Instant;

pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;

/// 包装一个 `Stream<Item = Result<T>>`，使每次 `.next()` 调用受 idle timeout 保护。
/// 如果两个 chunk 之间的间隔超过 `timeout`，返回 `Err("idle timeout after {elapsed}s")`。
pub struct IdleTimeoutStream<S> {
    inner: S,
    timeout: Duration,
    deadline: Instant,
    request_id: String,
}

impl<S> IdleTimeoutStream<S> {
    pub fn new(inner: S, timeout: Duration, request_id: String) -> Self {
        Self {
            inner,
            timeout,
            deadline: Instant::now() + timeout,
            request_id,
        }
    }
}

impl<S, T> Stream for IdleTimeoutStream<S>
where
    S: Stream<Item = Result<T>> + Unpin,
{
    type Item = Result<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let now = Instant::now();
        if now >= self.deadline {
            let elapsed = self.timeout.as_secs();
            return Poll::Ready(Some(Err(anyhow::anyhow!(
                "idle timeout after {}s (request {})",
                elapsed,
                self.request_id
            ))));
        }

        let remaining = self.deadline - now;
        let mut deadline_fut = Box::pin(tokio::time::sleep(remaining));

        // 用 select 同时等待 inner stream 和 deadline
        let inner_fut = Pin::new(&mut self.inner).poll_next(cx);

        match inner_fut {
            Poll::Ready(Some(Ok(item))) => {
                self.deadline = Instant::now() + self.timeout;
                Poll::Ready(Some(Ok(item)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                // 检查 deadline 是否已到
                if deadline_fut.as_mut().poll(cx).is_ready() {
                    let elapsed = self.timeout.as_secs();
                    Poll::Ready(Some(Err(anyhow::anyhow!(
                        "idle timeout after {}s (request {})",
                        elapsed,
                        self.request_id
                    ))))
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

/// 用 idle timeout 包装一个流。
///
/// 如果 `timeout_secs` 为 `None`，使用 `DEFAULT_IDLE_TIMEOUT_SECS`（300s）。
pub fn with_idle_timeout<S>(
    stream: S,
    timeout_secs: Option<u64>,
    request_id: String,
) -> IdleTimeoutStream<S>
where
    S: Stream,
{
    let secs = timeout_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);
    IdleTimeoutStream::new(stream, Duration::from_secs(secs), request_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use futures::StreamExt;

    #[tokio::test]
    async fn normal_stream_passes_through() {
        let s = stream::iter(vec![Ok::<_, anyhow::Error>(1), Ok(2), Ok(3)]);
        let timed = with_idle_timeout(s, Some(5), "test".into());
        let results: Vec<_> = timed.collect().await;
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn empty_stream_returns_none() {
        let s = stream::iter(Vec::<Result<i32>>::new());
        let timed = with_idle_timeout(s, Some(1), "test".into());
        let results: Vec<_> = timed.collect().await;
        assert!(results.is_empty());
    }
}
