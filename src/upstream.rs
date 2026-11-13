//! # Upstream servers: connection, calls, health
//!
//! An [`Upstream`] represents **one** MCP server from the registry. It groups:
//!
//! - its configuration ([`UpstreamConfig`]);
//! - one **rmcp client shared** by all the gateway's users. With Streamable HTTP every call
//!   is an independent POST: a single client is enough for concurrent calls;
//! - a **semaphore** that bounds concurrent calls (`max_concurrency`);
//! - its **health state** and the **cache of its tool list**, updated by the refresh loop
//!   (see [`crate::state`]).
//!
//! ## Connection lifecycle
//!
//! The connection is opened **lazily** (when first needed) and reopened if the transport
//! closes. An upstream that is unreachable at startup therefore does not block the gateway:
//! it is simply marked "unavailable" and retried at the next probe.
//!
//! The protocol version is negotiated in `Auto` mode: `server/discover` (MCP 2026-07-28,
//! sessionless) is tried first, falling back to the old `initialize` handshake if the
//! upstream is older. rmcp also handles renewing an expired session
//! (`reinit_on_expired_session`) for these older upstreams.
//!
//!

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use arc_swap::ArcSwapOption;
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, Peer, RoleClient, RoleServer,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResponse, CancelledNotificationParam,
        ClientCapabilities, ClientConfig, ClientRequest, Implementation, ProgressNotificationParam,
        ProgressToken, ProtocolVersion, RequestId, ServerResult, Tool,
    },
    service::{NotificationContext, PeerRequestOptions, RunningService, ServiceError},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::Serialize;
use tokio::sync::Semaphore;

use crate::config::{Transport, UpstreamConfig};

/// An open connection to an upstream: the rmcp client and the progress relay table specific
/// to this connection.
struct Connection {
    client: RunningService<RoleClient, UpstreamClientHandler>,
    progress: Arc<ProgressRouter>,
}

/// Pause after a failed connection attempt. During this window, calls fail immediately
/// instead of each retrying in turn: without it, N calls queued behind an upstream that does
/// not answer would wait N × `timeout`.
const RECONNECT_COOLDOWN: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// What can go wrong when talking to an upstream.
///
/// All these errors end up as `CallToolResult { isError: true }` downstream: they are errors
/// **the LLM can act on** (retry, switch tools…), not malformed requests.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("upstream server '{server}' unreachable: {reason}")]
    Unavailable { server: String, reason: String },
    #[error("upstream server '{server}' did not respond within {timeout:?}")]
    Timeout { server: String, timeout: Duration },
    #[error("upstream server '{server}' returned an error: {message}")]
    Protocol { server: String, message: String },
}

// ---------------------------------------------------------------------------------------------
// Progress relay
// ---------------------------------------------------------------------------------------------

/// "upstream token → (downstream client, downstream token)" table of **one** upstream connection.
///
/// A notification without a route is dropped. This covers calls whose client asked for no
/// progress (rmcp still sends a token upstream) and, in theory, a notification arriving
/// between the send and `register`: that window is a few microseconds, versus an HTTP round
/// trip, and losing one informational notification breaks nothing.
#[derive(Default)]
pub struct ProgressRouter {
    routes: Mutex<HashMap<ProgressToken, (Peer<RoleServer>, ProgressToken)>>,
}

impl ProgressRouter {
    /// Registers the route of a call. It is removed when the returned [`ProgressRoute`] is
    /// dropped (end of the call).
    pub fn register(
        self: &Arc<Self>,
        upstream_token: ProgressToken,
        downstream: Peer<RoleServer>,
        downstream_token: ProgressToken,
    ) -> ProgressRoute {
        self.routes
            .lock()
            .expect("progress lock")
            .insert(upstream_token.clone(), (downstream, downstream_token));
        ProgressRoute {
            router: self.clone(),
            token: upstream_token,
        }
    }

    /// Forwards the notification to the relevant downstream client, if any.
    async fn forward(&self, mut params: ProgressNotificationParam) {
        let route = self
            .routes
            .lock()
            .expect("progress lock")
            .get(&params.progress_token)
            .cloned();
        if let Some((peer, downstream_token)) = route {
            params.progress_token = downstream_token;
            if let Err(e) = peer.notify_progress(params).await {
                tracing::debug!(error = %e, "progress relay failed");
            }
        }
    }
}

/// RAII guard: as long as it lives, the progress route exists.
pub struct ProgressRoute {
    router: Arc<ProgressRouter>,
    token: ProgressToken,
}

impl Drop for ProgressRoute {
    fn drop(&mut self) {
        if let Ok(mut routes) = self.router.routes.lock() {
            routes.remove(&self.token);
        }
    }
}

/// Sends `notifications/cancelled` to the upstream if the call is dropped (downstream client
/// gone, timeout) before its response was received. `Drop` cannot await: a task is spawned.
struct CancelOnDrop {
    peer: Peer<RoleClient>,
    id: Option<RequestId>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let peer = self.peer.clone();
            tokio::spawn(async move {
                let reason = Some("call abandoned by the gateway".to_owned());
                let _ = peer
                    .notify_cancelled(CancelledNotificationParam::new(Some(id), reason))
                    .await;
            });
        }
    }
}

/// rmcp **client**-side handler: describes the gateway to the upstream and receives its
/// notifications. Only `on_progress` is implemented; everything else keeps rmcp's default
/// behavior.
pub struct UpstreamClientHandler {
    progress: Arc<ProgressRouter>,
}

impl ClientHandler for UpstreamClientHandler {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("mcportal", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.progress.forward(params).await;
    }
}

// ---------------------------------------------------------------------------------------------
// Health state
// ---------------------------------------------------------------------------------------------

/// Observable state of an upstream (read by the catalog and `/health`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Not probed yet.
    Unknown,
    Available,
    /// `failure_threshold` consecutive failures (or unsupported transport).
    Unavailable,
    /// `enabled: false` in the registry.
    Disabled,
}

/// Snapshot of an upstream's health.
#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    pub status: Availability,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    /// UNIX timestamp (seconds) of the last probe.
    pub last_probe_unix: Option<u64>,
    pub last_success_unix: Option<u64>,
    /// Number of tools in the cache (None if never listed).
    pub tool_count: Option<usize>,
    /// Call counters since the pod started.
    pub calls_total: u64,
    pub calls_failed: u64,
}

#[derive(Debug)]
struct Health {
    consecutive_failures: u32,
    last_error: Option<String>,
    last_probe: Option<SystemTime>,
    last_success: Option<SystemTime>,
    ever_probed: bool,
}

/// Cache of an upstream's tool list.
#[derive(Debug, Clone)]
struct ToolCache {
    tools: Arc<Vec<Tool>>,
    fetched_at: Instant,
}

// ---------------------------------------------------------------------------------------------
// Upstream
// ---------------------------------------------------------------------------------------------

/// An upstream MCP server. See the module documentation.
pub struct Upstream {
    pub config: UpstreamConfig,
    failure_threshold: u32,
    /// `max_concurrency` limit of this server.
    pub semaphore: Semaphore,
    /// Current connection. `ArcSwapOption`: read lock-free by every call, replaced on
    /// (re)connection and cleared when the transport fails.
    connection: ArcSwapOption<Connection>,
    /// Serializes connection attempts (two simultaneous calls open only one connection) and
    /// holds the time and reason of the last failed attempt (see [`RECONNECT_COOLDOWN`]).
    /// `tokio::sync::Mutex` because it is held across the connection `await`.
    connect_lock: tokio::sync::Mutex<Option<(Instant, String)>>,
    /// `std::sync::Mutex`: very short critical sections, never an `await` inside.
    health: Mutex<Health>,
    tools: Mutex<Option<ToolCache>>,
    calls_total: AtomicU64,
    calls_failed: AtomicU64,
}

impl Upstream {
    pub fn new(config: UpstreamConfig, failure_threshold: u32) -> Self {
        Self {
            semaphore: Semaphore::new(config.max_concurrency),
            config,
            failure_threshold,
            connection: ArcSwapOption::empty(),
            connect_lock: tokio::sync::Mutex::new(None),
            health: Mutex::new(Health {
                consecutive_failures: 0,
                last_error: None,
                last_probe: None,
                last_success: None,
                ever_probed: false,
            }),
            tools: Mutex::new(None),
            calls_total: AtomicU64::new(0),
            calls_failed: AtomicU64::new(0),
        }
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    // --- Connection -----------------------------------------------------------------------

    /// Deadline of an operation started now: it covers **everything**, the wait for a
    /// connection included, so that no call lasts longer than `timeout`.
    fn deadline(&self) -> tokio::time::Instant {
        tokio::time::Instant::now() + self.config.timeout()
    }

    fn timeout_error(&self) -> UpstreamError {
        UpstreamError::Timeout {
            server: self.config.name.clone(),
            timeout: self.config.timeout(),
        }
    }

    /// The open connection, if any (lock-free).
    fn current_connection(&self) -> Option<Arc<Connection>> {
        self.connection
            .load_full()
            .filter(|connection| !connection.client.is_closed())
    }

    /// Returns the current connection, (re)connecting if needed, before `deadline`.
    ///
    /// - Common case: the connection is open, read lock-free.
    /// - Otherwise one task connects while the others wait for its result on `connect_lock`.
    ///   If the attempt fails, the failure is kept for [`RECONNECT_COOLDOWN`]: the waiting
    ///   tasks, and the calls that arrive during that window, fail immediately instead of
    ///   each retrying in turn.
    ///
    /// The connection attempt is bounded by `deadline` itself (never cut by an outer timeout),
    /// so a failure is always recorded. Only a downstream cancellation can interrupt it, in
    /// which case the next call simply tries again.
    async fn connection(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<Arc<Connection>, UpstreamError> {
        if let Some(connection) = self.current_connection() {
            return Ok(connection);
        }
        let mut last_failure = tokio::time::timeout_at(deadline, self.connect_lock.lock())
            .await
            .map_err(|_| self.timeout_error())?;
        // Another task may have connected while we were waiting.
        if let Some(connection) = self.current_connection() {
            return Ok(connection);
        }
        let unavailable = |reason: String| UpstreamError::Unavailable {
            server: self.config.name.clone(),
            reason,
        };
        if let Some((at, reason)) = last_failure.as_ref()
            && at.elapsed() < RECONNECT_COOLDOWN
        {
            let retry_in = RECONNECT_COOLDOWN.saturating_sub(at.elapsed());
            return Err(unavailable(format!(
                "{reason} (next connection attempt in {}s)",
                retry_in.as_secs().max(1)
            )));
        }
        match self.connect(deadline).await {
            Ok(connection) => {
                let connection = Arc::new(connection);
                self.connection.store(Some(connection.clone()));
                *last_failure = None;
                Ok(connection)
            }
            Err(reason) => {
                self.connection.store(None);
                *last_failure = Some((Instant::now(), reason.clone()));
                Err(unavailable(reason))
            }
        }
    }

    /// Forgets the connection `used` (it will be reopened on the next call), **unless** it has
    /// already been replaced: a late failure on an old connection must not close the new one.
    fn invalidate(&self, used: &Arc<Connection>) {
        self.connection.compare_and_swap(&Some(used.clone()), None);
    }

    /// Opens a connection before `deadline`. Returns the reason of the failure.
    async fn connect(&self, deadline: tokio::time::Instant) -> Result<Connection, String> {
        if self.config.transport == Transport::Sse {
            return Err(
                "'sse' transport is deprecated and not supported by rmcp 3.x: use 'http'".into(),
            );
        }

        let mut transport_config =
            StreamableHttpClientTransportConfig::with_uri(self.config.url.as_str())
                // The transport must not be the bottleneck: our semaphore already bounds the calls.
                .max_concurrent_requests(self.config.max_concurrency.max(1) + 1);
        // Older upstreams (with sessions): reopen an expired session automatically.
        transport_config.reinit_on_expired_session = true;
        let transport = StreamableHttpClientTransport::from_config(transport_config);

        let progress = Arc::new(ProgressRouter::default());
        let handler = UpstreamClientHandler {
            progress: progress.clone(),
        };
        let lifecycle = ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        };
        let connecting = handler.serve_with_lifecycle(transport, lifecycle);
        match tokio::time::timeout_at(deadline, connecting).await {
            Ok(Ok(client)) => {
                tracing::info!(server = %self.config.name, "connected to upstream");
                Ok(Connection { client, progress })
            }
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!(
                "no response to initialization within {:?}",
                self.config.timeout()
            )),
        }
    }

    // --- MCP operations -------------------------------------------------------------------

    /// Lists **all** the upstream's tools (every page), with a timeout.
    pub async fn fetch_tools(&self) -> Result<Vec<Tool>, UpstreamError> {
        let deadline = self.deadline();
        let connection = self.connection(deadline).await?;
        let result = tokio::time::timeout_at(deadline, connection.client.list_all_tools()).await;
        self.map_result(result, &connection)
    }

    /// Lightweight probe: the first page of `tools/list`.
    ///
    /// `ping` no longer exists in MCP 2026-07-28 and `server/discover` does not exist in
    /// earlier versions: `tools/list` is the only request all our upstreams serve.
    pub async fn probe(&self) -> Result<(), UpstreamError> {
        let deadline = self.deadline();
        let connection = self.connection(deadline).await?;
        let result = tokio::time::timeout_at(deadline, connection.client.list_tools(None)).await;
        self.map_result(result, &connection).map(|_| ())
    }

    /// Calls an upstream tool **without** handling MRTR rounds: if the upstream returns an
    /// `InputRequiredResult`, it is returned as is to the downstream client, which will
    /// provide the answers by calling `call_tool` again (with `inputResponses` /
    /// `requestState`), which we forward.
    ///
    /// The concurrency limit is **not** handled here but in [`crate::limits`].
    pub async fn call_tool(
        &self,
        params: CallToolRequestParams,
        progress: Option<(Peer<RoleServer>, ProgressToken)>,
    ) -> Result<CallToolResponse, UpstreamError> {
        self.calls_total.fetch_add(1, Ordering::Relaxed);
        let deadline = self.deadline();
        let connection = self
            .connection(deadline)
            .await
            .inspect_err(|_| self.count_failure())?;

        let call = async {
            match progress {
                // Common case: `call_tool_once` handles cancellation itself if dropped.
                None => connection.client.call_tool_once(params).await,
                Some(downstream) => Self::call_with_progress(&connection, params, downstream).await,
            }
        };
        let result = tokio::time::timeout_at(deadline, call).await;
        self.map_result(result, &connection)
            .inspect_err(|_| self.count_failure())
    }

    /// Variant of `call_tool_once` that relays progress: the request is sent by hand to learn
    /// the token chosen by rmcp, then the route is registered before waiting.
    async fn call_with_progress(
        connection: &Connection,
        params: CallToolRequestParams,
        (downstream, downstream_token): (Peer<RoleServer>, ProgressToken),
    ) -> Result<CallToolResponse, ServiceError> {
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
        let handle = connection
            .client
            .send_request_with_option(request, PeerRequestOptions::no_options())
            .await?;
        // Both guards live until the end of the function (or until it is dropped).
        let _route = connection.progress.register(
            handle.progress_token.clone(),
            downstream,
            downstream_token,
        );
        let mut cancel = CancelOnDrop {
            peer: connection.client.peer().clone(),
            id: Some(handle.id.clone()),
        };
        let result = handle.await_response().await;
        cancel.id = None; // response received: nothing to cancel
        match result? {
            ServerResult::CallToolResult(r) => Ok(CallToolResponse::Complete(r)),
            ServerResult::InputRequiredResult(r) => Ok(CallToolResponse::InputRequired(r)),
            ServerResult::CreateTaskResult(r) => Ok(CallToolResponse::Task(r)),
            _ => Err(ServiceError::UnexpectedResponse),
        }
    }

    fn count_failure(&self) {
        self.calls_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Converts (timeout | rmcp error | success) into `Result<T, UpstreamError>`, and
    /// invalidates `connection` if the error comes from the transport.
    fn map_result<T>(
        &self,
        result: Result<Result<T, ServiceError>, tokio::time::error::Elapsed>,
        connection: &Arc<Connection>,
    ) -> Result<T, UpstreamError> {
        let server = self.config.name.clone();
        match result {
            Ok(Ok(value)) => Ok(value),
            Err(_) => Err(self.timeout_error()),
            // JSON-RPC error returned by the upstream: the connection is healthy.
            Ok(Err(ServiceError::McpError(e))) => Err(UpstreamError::Protocol {
                server,
                message: e.message.to_string(),
            }),
            // Any other error (transport closed, unexpected response…): reconnect later.
            Ok(Err(e)) => {
                self.invalidate(connection);
                Err(UpstreamError::Unavailable {
                    server,
                    reason: e.to_string(),
                })
            }
        }
    }

    // --- Tool cache and health ------------------------------------------------------------

    /// Cached tools (last known list, even if the upstream has gone down since).
    pub fn cached_tools(&self) -> Option<Arc<Vec<Tool>>> {
        self.tools
            .lock()
            .expect("tools lock")
            .as_ref()
            .map(|c| c.tools.clone())
    }

    /// Is the cache older than `cache_ttl` (or empty)?
    pub fn cache_expired(&self) -> bool {
        match self.tools.lock().expect("tools lock").as_ref() {
            None => true,
            Some(c) => c.fetched_at.elapsed() >= self.config.cache_ttl(),
        }
    }

    /// Replaces the cache. Returns `true` if the list changed (→ rebuild the index).
    pub fn store_tools(&self, tools: Vec<Tool>) -> bool {
        let mut cache = self.tools.lock().expect("tools lock");
        let changed = cache.as_ref().map(|c| *c.tools != tools).unwrap_or(true);
        *cache = Some(ToolCache {
            tools: Arc::new(tools),
            fetched_at: Instant::now(),
        });
        changed
    }

    /// Records the outcome of a probe.
    pub fn record_probe(&self, outcome: &Result<(), UpstreamError>) {
        let mut h = self.health.lock().expect("health lock");
        let now = SystemTime::now();
        h.ever_probed = true;
        h.last_probe = Some(now);
        match outcome {
            Ok(()) => {
                h.consecutive_failures = 0;
                h.last_error = None;
                h.last_success = Some(now);
            }
            Err(e) => {
                h.consecutive_failures = h.consecutive_failures.saturating_add(1);
                h.last_error = Some(e.to_string());
            }
        }
    }

    /// Is the upstream considered available?
    pub fn availability(&self) -> Availability {
        if !self.config.enabled {
            return Availability::Disabled;
        }
        let h = self.health.lock().expect("health lock");
        if !h.ever_probed {
            Availability::Unknown
        } else if h.consecutive_failures >= self.failure_threshold {
            Availability::Unavailable
        } else {
            Availability::Available
        }
    }

    /// Health snapshot, for the catalog and `/health`.
    pub fn health_snapshot(&self) -> HealthSnapshot {
        let status = self.availability();
        let h = self.health.lock().expect("health lock");
        let unix = |t: Option<SystemTime>| {
            t.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
        };
        HealthSnapshot {
            status,
            consecutive_failures: h.consecutive_failures,
            last_error: h.last_error.clone(),
            last_probe_unix: unix(h.last_probe),
            last_success_unix: unix(h.last_success),
            tool_count: self.cached_tools().map(|t| t.len()),
            calls_total: self.calls_total.load(Ordering::Relaxed),
            calls_failed: self.calls_failed.load(Ordering::Relaxed),
        }
    }
}
