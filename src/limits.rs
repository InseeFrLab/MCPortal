//! # Concurrency limits
//!
//! V1: **global** limits, not tied to the user (no authentication).
//! Two semaphores are acquired one after the other before each upstream call:
//!
//! ```text
//!   call ──▶ [global: gateway.concurrency.global_limit] ──▶ [server: max_concurrency] ──▶ upstream
//! ```
//!
//! A semaphore is a counter of "permits": `acquire` waits for a permit to become free, and the
//! permit is returned automatically when the `SemaphorePermit` is dropped, i.e. at the end of
//! the call, even on error or cancellation.
//!
//! We never wait indefinitely: past `acquire_timeout_seconds`, the call is rejected with an
//! explicit message ("gateway saturated") that the LLM can understand.
//!
//! The global → server order matters: we never take a "server" permit without already holding
//! a "global" one, which prevents a slow server from hogging global permits while waiting.

use std::time::Duration;

use tokio::sync::{Semaphore, SemaphorePermit};

use crate::{telemetry::metric, upstream::Upstream};

/// Call rejected for lack of a free permit.
#[derive(Debug, thiserror::Error)]
pub enum Rejected {
    #[error("gateway saturated (at most {limit} concurrent calls): try again in a few seconds")]
    Global { limit: usize },
    #[error(
        "server '{server}' is saturated (at most {limit} concurrent calls): try again in a few seconds"
    )]
    Server { server: String, limit: usize },
}

/// Global limits of the gateway.
pub struct Limits {
    global: Semaphore,
    global_limit: usize,
    acquire_timeout: Duration,
}

/// Permits obtained for a call. Dropping this value releases them.
pub struct CallPermit<'a> {
    _global: SemaphorePermit<'a>,
    _server: SemaphorePermit<'a>,
    server: String,
}

impl Drop for CallPermit<'_> {
    fn drop(&mut self) {
        metrics::gauge!(metric::INFLIGHT, "server" => self.server.clone()).decrement(1.0);
    }
}

impl Limits {
    pub fn new(global_limit: usize, acquire_timeout: Duration) -> Self {
        Self {
            global: Semaphore::new(global_limit),
            global_limit,
            acquire_timeout,
        }
    }

    /// Acquires a global permit then a permit on `upstream`, within the configured timeout
    /// (the timeout covers **both** waits).
    pub async fn acquire<'a>(&'a self, upstream: &'a Upstream) -> Result<CallPermit<'a>, Rejected> {
        let deadline = tokio::time::Instant::now() + self.acquire_timeout;
        let server = upstream.name().to_owned();

        let global = tokio::time::timeout_at(deadline, self.global.acquire())
            .await
            .ok()
            .and_then(Result::ok) // Err = semaphore closed, cannot happen here
            .ok_or_else(|| {
                metrics::counter!(metric::RATELIMIT_REJECTIONS, "scope" => "global").increment(1);
                Rejected::Global {
                    limit: self.global_limit,
                }
            })?;

        let server_permit = tokio::time::timeout_at(deadline, upstream.semaphore.acquire())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| {
                metrics::counter!(metric::RATELIMIT_REJECTIONS, "scope" => "server").increment(1);
                Rejected::Server {
                    server: server.clone(),
                    limit: upstream.config.max_concurrency,
                }
            })?;

        metrics::gauge!(metric::INFLIGHT, "server" => server.clone()).increment(1.0);
        Ok(CallPermit {
            _global: global,
            _server: server_permit,
            server,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::Config;

    fn upstream(max_concurrency: usize) -> Upstream {
        let yaml = format!(
            "gateway: {{name: T}}\nservers:\n  - {{name: a, url: http://x/mcp, max_concurrency: {max_concurrency}}}\n"
        );
        let cfg = Config::from_yaml(&yaml).unwrap();
        Upstream::new(cfg.servers[0].clone(), 2)
    }

    #[tokio::test]
    async fn server_limit_rejects_after_timeout() {
        let limits = Limits::new(10, Duration::from_millis(50));
        let up = upstream(1);
        let _first = limits.acquire(&up).await.unwrap();
        let second = limits.acquire(&up).await;
        assert!(matches!(second, Err(Rejected::Server { limit: 1, .. })));
    }

    #[tokio::test]
    async fn global_limit_applies_across_servers() {
        let limits = Limits::new(1, Duration::from_millis(50));
        let (a, b) = (upstream(5), upstream(5));
        let _first = limits.acquire(&a).await.unwrap();
        assert!(matches!(
            limits.acquire(&b).await,
            Err(Rejected::Global { limit: 1 })
        ));
    }

    #[tokio::test]
    async fn permits_are_released_on_drop() {
        let limits = Limits::new(1, Duration::from_millis(50));
        let up = upstream(1);
        drop(limits.acquire(&up).await.unwrap());
        assert!(limits.acquire(&up).await.is_ok());
    }

    #[tokio::test]
    async fn waiting_call_gets_the_released_place() {
        let limits = Arc::new(Limits::new(1, Duration::from_secs(2)));
        let up = Arc::new(upstream(1));
        let first = limits.acquire(&up).await.unwrap();
        let (l2, u2) = (limits.clone(), up.clone());
        let waiter = tokio::spawn(async move { l2.acquire(&u2).await.is_ok() });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(first);
        assert!(waiter.await.unwrap());
    }
}
