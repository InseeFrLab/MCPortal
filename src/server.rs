//! # Assemblage HTTP : MCP, santé, métriques, arrêt propre
//!
//! Deux ports :
//!
//! | Port | Routes | Exposé |
//! | --- | --- | --- |
//! | `gateway.http.bind` (8000) | `/mcp` (MCP Streamable HTTP), `/health`, `/ready` | via l'ingress |
//! | `metrics.prometheus.port` (9090) | `/metrics` | interne au cluster |
//!
//! ## Mode stateless
//!
//! On désactive le mode « session » de rmcp (`legacy_session_mode = false`) : chaque requête
//! HTTP est autonome, quelle que soit la version du protocole du client. N'importe quel pod
//! peut donc servir n'importe quelle requête : plusieurs réplicas, pas de sessions sticky.
//!
//! ## Sondes Kubernetes
//!
//! - `/health` (liveness) : 200 tant que le process répond. Le corps JSON détaille l'état des
//!   amonts, mais un amont en panne **ne fait pas** échouer la sonde (sinon Kubernetes
//!   redémarrerait la passerelle pour une panne qui n'est pas la sienne).
//! - `/ready` (readiness) : 200 quand le premier chargement des outils est fait, 503 avant et
//!   pendant l'arrêt.
//!
//! ## Arrêt propre (rolling update sans coupure)
//!
//! ```text
//! SIGTERM ─▶ /ready = 503 ─▶ axum n'accepte plus de connexions et attend la fin des requêtes
//!         ─▶ annulation des tâches de fond ─▶ sortie
//! ```
//!
//! Côté Kubernetes : un `preStop: sleep 5` laisse le temps à l'ingress de retirer le pod, et
//! `terminationGracePeriodSeconds` doit dépasser le plus grand `timeout` amont.

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

/// Construit le routeur du port principal (MCP + santé).
///
/// `shutdown` est le jeton d'arrêt des flux MCP en cours ; il ne doit être annulé qu'**après**
/// la fin du drain HTTP (voir [`serve`]).
pub fn router(state: Arc<AppState>, shutdown: CancellationToken) -> Router {
    let http = &state.config.gateway.http;
    let mut mcp_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        // Réponse en `application/json` simple quand l'outil n'émet aucune notification
        // (cas le plus courant) : moins coûteux qu'un flux SSE. rmcp repasse automatiquement en
        // SSE si une notification (ex. progression) doit précéder la réponse.
        .with_json_response(true)
        .with_cancellation_token(shutdown);
    mcp_config = if http.allowed_hosts.is_empty() {
        mcp_config.disable_allowed_hosts()
    } else {
        mcp_config.with_allowed_hosts(http.allowed_hosts.clone())
    };

    // rmcp appelle cette fabrique pour chaque requête : on clone un `Arc`, c'est gratuit.
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

/// `/health` : liveness + état détaillé.
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

/// `/ready` : readiness.
async fn ready(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if state.is_ready() {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

/// Routeur du port métriques.
pub fn metrics_router(handle: PrometheusHandle) -> Router {
    Router::new().route("/metrics", get(move || std::future::ready(handle.render())))
}

/// Sert la passerelle jusqu'à ce que `stop` soit annulé, puis s'arrête proprement.
///
/// Les écouteurs sont passés déjà ouverts : les tests peuvent ainsi utiliser un port
/// aléatoire (`127.0.0.1:0`).
pub async fn serve(
    state: Arc<AppState>,
    listener: TcpListener,
    metrics: Option<(PrometheusHandle, TcpListener)>,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    // Jeton « interne » : annulé seulement après le drain, pour arrêter les tâches de fond et
    // les flux MCP restants.
    let background = CancellationToken::new();

    // Tâches de fond : premier chargement des outils, puis rafraîchissement périodique.
    {
        let state = state.clone();
        let background = background.clone();
        tokio::spawn(async move {
            state.initial_load().await;
            crate::state::spawn_refresh_loops(state, background);
        });
    }

    // Port métriques (s'arrête avec le jeton interne).
    if let Some((handle, metrics_listener)) = metrics {
        let background = background.clone();
        tokio::spawn(async move {
            let server = axum::serve(metrics_listener, metrics_router(handle))
                .with_graceful_shutdown(background.cancelled_owned());
            if let Err(e) = server.await {
                tracing::error!(error = %e, "serveur de métriques arrêté");
            }
        });
    }

    let app = router(state.clone(), background.child_token());
    let drain_state = state.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop.cancelled().await;
            // Dès le signal : on se déclare « non prêt » ; axum arrête d'accepter et attend la
            // fin des requêtes en cours.
            drain_state.ready.store(false, Ordering::Release);
            tracing::info!("arrêt demandé : drain des requêtes en cours");
        })
        .await?;

    background.cancel();
    tracing::info!("arrêt terminé");
    Ok(())
}

/// Attend SIGTERM (Kubernetes) ou Ctrl-C (local).
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

/// Adresse locale d'un écouteur (pour les logs).
pub fn local_addr(listener: &TcpListener) -> Option<SocketAddr> {
    listener.local_addr().ok()
}
