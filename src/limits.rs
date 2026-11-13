//! # Concurrency limits
//!
//! V1: one limit **per upstream server** (`max_concurrency`), not tied to the user (no
//! authentication). Before each upstream call, a permit is taken from the server's semaphore:
//!
//! ```text
//!   call ──▶ [server: max_concurrency] ──▶ upstream
//! ```
//!
//! A semaphore is a counter of "permits": `acquire` waits for a permit to become free, and the
//! permit is returned automatically when the `SemaphorePermit` is dropped, i.e. at the end of
//! the call, even on error or cancellation.
//!
//! We never wait indefinitely: past `acquire_timeout_seconds`, the call is rejected with an
//! explicit message ("server saturated") that the LLM can understand.
//!
//! ## Why there is no global limit
//!
//! The servers' limits are independent: a slow or failing server can only exhaust its own
//! permits, never those of the others. A global limit would add nothing useful:
//!
//! - below the sum of the servers' limits, it would only **couple** them: calls waiting on a
//!   failing server would hold global permits and get calls to healthy servers rejected;
//! - above that sum, it would never apply.
//!
//! An in-flight call costs the gateway almost nothing (a tokio task and an HTTP request). If
//! the gateway itself ever needs protecting, it belongs on incoming requests (HTTP layer),
//! not here.

use std::time::Duration;

use tokio::sync::SemaphorePermit;

use crate::{telemetry::metric, upstream::Upstream};

/// Call rejected for lack of a free permit on its server.
#[derive(Debug, thiserror::Error)]
#[error(
    "server '{server}' is saturated (at most {limit} concurrent calls): try again in a few seconds"
)]
pub struct Rejected {
    pub server: String,
    pub limit: usize,
}

/// Concurrency settings shared by every call.
pub struct Limits {
    acquire_timeout: Duration,
}

/// Permit obtained for a call. Dropping this value releases it.
pub struct CallPermit<'a> {
    _permit: SemaphorePermit<'a>,
    server: String,
}

impl Drop for CallPermit<'_> {
    fn drop(&mut self) {
        metrics::gauge!(metric::INFLIGHT, "server" => self.server.clone()).decrement(1.0);
    }
}

impl Limits {
    pub fn new(acquire_timeout: Duration) -> Self {
        Self { acquire_timeout }
    }

    /// Acquires a permit on `upstream`, within the configured timeout.
    pub async fn acquire<'a>(&self, upstream: &'a Upstream) -> Result<CallPermit<'a>, Rejected> {
        let server = upstream.name().to_owned();
        let permit = tokio::time::timeout(self.acquire_timeout, upstream.semaphore.acquire())
            .await
            .ok()
            .and_then(Result::ok) // Err = semaphore closed, cannot happen here
            .ok_or_else(|| {
                metrics::counter!(metric::RATELIMIT_REJECTIONS, "scope" => "server").increment(1);
                Rejected {
                    server: server.clone(),
                    limit: upstream.config.max_concurrency,
                }
            })?;

        metrics::gauge!(metric::INFLIGHT, "server" => server.clone()).increment(1.0);
        Ok(CallPermit {
            _permit: permit,
            server,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::Config;

    fn upstream(name: &str, max_concurrency: usize) -> Upstream {
        let yaml = format!(
            "gateway: {{name: T}}\nservers:\n  - {{name: {name}, url: http://x/mcp, max_concurrency: {max_concurrency}}}\n"
        );
        let cfg = Config::from_yaml(&yaml).unwrap();
        Upstream::new(cfg.servers[0].clone(), 2)
    }

    #[tokio::test]
    async fn server_limit_rejects_after_timeout() {
        let limits = Limits::new(Duration::from_millis(50));
        let up = upstream("a", 1);
        let _first = limits.acquire(&up).await.unwrap();
        let second = limits.acquire(&up).await;
        assert!(matches!(second, Err(Rejected { limit: 1, .. })));
    }

    #[tokio::test]
    async fn saturated_server_does_not_affect_others() {
        let limits = Limits::new(Duration::from_millis(50));
        let (a, b) = (upstream("a", 1), upstream("b", 1));
        let _held = limits.acquire(&a).await.unwrap();
        assert!(limits.acquire(&a).await.is_err());
        assert!(limits.acquire(&b).await.is_ok());
    }

    #[tokio::test]
    async fn permits_are_released_on_drop() {
        let limits = Limits::new(Duration::from_millis(50));
        let up = upstream("a", 1);
        drop(limits.acquire(&up).await.unwrap());
        assert!(limits.acquire(&up).await.is_ok());
    }

    #[tokio::test]
    async fn waiting_call_gets_the_released_place() {
        let limits = Arc::new(Limits::new(Duration::from_secs(2)));
        let up = Arc::new(upstream("a", 1));
        let first = limits.acquire(&up).await.unwrap();
        let (l2, u2) = (limits.clone(), up.clone());
        let waiter = tokio::spawn(async move { l2.acquire(&u2).await.is_ok() });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(first);
        assert!(waiter.await.unwrap());
    }
}
