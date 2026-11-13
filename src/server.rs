//! # HTTP wiring: MCP, health, metrics, graceful shutdown
//!
//! Two ports:
//!
//! | Port | Routes | Exposed |
//! | --- | --- | --- |
//! | `gateway.http.bind` (8000) | `/mcp` (MCP Streamable HTTP), `/health`, `/ready` | through the ingress |
//! | `metrics.prometheus.port` (9090) | `/metrics` | cluster-internal |
//!
//! ## Stateless mode
//!
//! rmcp's "session" mode is disabled (`legacy_session_mode = false`): every HTTP request is
//! self-contained, whatever the client's protocol version. Any pod can therefore serve any
//! request: several replicas, no sticky sessions.
//!
//! ## Kubernetes probes
//!
//! - `/health` (liveness): 200 as long as the process responds. The JSON body details the
//!   upstreams' status, but a failing upstream **does not** fail the probe (otherwise
//!   Kubernetes would restart the gateway for an outage that is not its own).
//! - `/ready` (readiness): 200 once the first tool load is done, 503 before that and during
//!   shutdown.
//!
//! ## Graceful shutdown (rolling update without outage)
//!
//! ```text
//! SIGTERM ─▶ /ready = 503 ─▶ axum stops accepting connections and waits for in-flight requests
//!         ─▶ background tasks cancelled ─▶ exit
//! ```
//!
//! On the Kubernetes side: a `preStop: sleep 5` gives the ingress time to remove the pod, and
//! `terminationGracePeriodSeconds` must exceed the largest upstream `timeout`.

use std::{net::SocketAddr, sync::Arc, sync::atomic::Ordering};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use metrics_exporter_prometheus::PrometheusHandle;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{gateway::Gateway, state::AppState};

/// Builds the router of the main port (MCP + health).
///
/// `shutdown` is the token that stops in-flight MCP streams; it must only be cancelled
/// **after** the HTTP drain has finished (see [`serve`]).
pub fn router(state: Arc<AppState>, shutdown: CancellationToken) -> Router {
    let http = &state.config.gateway.http;
    let mut mcp_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        // Plain `application/json` response when the tool emits no notification (the most
        // common case): cheaper than an SSE stream. rmcp switches back to SSE automatically
        // if a notification (e.g. progress) must precede the response.
        .with_json_response(true)
        .with_cancellation_token(shutdown);
    mcp_config = if http.allowed_hosts.is_empty() {
        mcp_config.disable_allowed_hosts()
    } else {
        mcp_config.with_allowed_hosts(http.allowed_hosts.clone())
    };

    // rmcp calls this factory for every request: we clone an `Arc`, which is free.
    let factory_state = state.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(Gateway::new(factory_state.clone())),
        Arc::new(NeverSessionManager::default()),
        mcp_config,
    );

    Router::new()
        .nest_service(&http.path, mcp)
        .route(&state.config.gateway.health.path, get(health))
        .route("/ready", get(ready))
        .with_state(state)
}

/// `/health`: liveness + detailed status.
async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let servers: serde_json::Map<String, serde_json::Value> = state
        .upstreams
        .iter()
        .map(|(name, u)| {
            (
                name.clone(),
                serde_json::to_value(u.health_snapshot()).unwrap_or_default(),
            )
        })
        .collect();
    Json(json!({
        "status": "ok",
        "ready": state.is_ready(),
        "tools": state.catalog.load().tools.len(),
        "servers": servers,
    }))
}

/// `/ready`: readiness.
async fn ready(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if state.is_ready() {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

/// Router of the metrics port.
pub fn metrics_router(handle: PrometheusHandle) -> Router {
    Router::new().route("/metrics", get(move || std::future::ready(handle.render())))
}

/// Serves the gateway until `stop` is cancelled, then shuts down gracefully.
///
/// Listeners are passed already bound, so tests can use a random port (`127.0.0.1:0`).
pub async fn serve(
    state: Arc<AppState>,
    listener: TcpListener,
    metrics: Option<(PrometheusHandle, TcpListener)>,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    // "Internal" token: cancelled only after the drain, to stop the background tasks and the
    // remaining MCP streams.
    let background = CancellationToken::new();

    // Background tasks: initial tool load, then periodic refresh.
    {
        let state = state.clone();
        let background = background.clone();
        tokio::spawn(async move {
            state.initial_load().await;
            crate::state::spawn_refresh_loops(state, background);
        });
    }

    // Metrics port (stops with the internal token).
    if let Some((handle, metrics_listener)) = metrics {
        let background = background.clone();
        tokio::spawn(async move {
            let server = axum::serve(metrics_listener, metrics_router(handle))
                .with_graceful_shutdown(background.cancelled_owned());
            if let Err(e) = server.await {
                tracing::error!(error = %e, "metrics server stopped");
            }
        });
    }

    let app = router(state.clone(), background.child_token());
    let drain_state = state.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop.cancelled().await;
            // As soon as the signal arrives: report "not ready"; axum stops accepting and
            // waits for in-flight requests to finish.
            drain_state.ready.store(false, Ordering::Release);
            tracing::info!("shutdown requested: draining in-flight requests");
        })
        .await?;

    background.cancel();
    tracing::info!("shutdown complete");
    Ok(())
}

/// Waits for SIGTERM (Kubernetes) or Ctrl-C (local).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Local address of a listener (for logs).
pub fn local_addr(listener: &TcpListener) -> Option<SocketAddr> {
    listener.local_addr().ok()
}
