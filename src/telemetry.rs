//! # Observabilité : logs et métriques
//!
//! ## Logs (`tracing`)
//!
//! Les logs partent sur la **sortie standard**, une ligne JSON par événement
//! (`json_logs: true`), pour être collectés par Loki et lus dans Grafana. Exemple :
//!
//! ```json
//! {"timestamp":"…","level":"INFO","message":"appel d'outil","server":"insee","tool":"…","status":"ok","duration_ms":412}
//! ```
//!
//! Le niveau vient de `gateway.metrics.log_level`, sauf si la variable `RUST_LOG` est
//! définie (pratique pour déboguer un module précis : `RUST_LOG=mcportal=debug,rmcp=info`).
//!
//! Les retours de `send_feedback` sont des logs normaux portant `label = "FEEDBACK"` : un
//! filtre Loki `| json | label="FEEDBACK"` suffit à les isoler.
//!
//! ## Métriques (`metrics` + Prometheus)
//!
//! Le code appelle les macros `metrics::counter!`, `gauge!`, `histogram!` partout où c'est
//! utile ; ce module installe l'« enregistreur » Prometheus qui les agrège. Le rendu texte
//! est servi sur `/metrics` par [`crate::server`], sur un port séparé.
//!
//! Aucune métrique ne porte d'identifiant d'utilisateur (risque d'explosion de cardinalité).

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::EnvFilter;

use crate::config::MetricsConfig;

/// Noms des métriques, regroupés ici pour avoir l'inventaire complet en un coup d'œil.
pub mod metric {
    /// counter — labels: server, tool, status (ok | tool_error | upstream_error | timeout | rejected | invalid)
    pub const TOOL_CALLS: &str = "mcportal_tool_calls_total";
    /// histogram (s) — label: server — durée totale vue par le client aval
    pub const TOOL_CALL_DURATION: &str = "mcportal_tool_call_duration_seconds";
    /// histogram (s) — temps passé dans la passerelle elle-même (total − temps amont)
    pub const GATEWAY_OVERHEAD: &str = "mcportal_gateway_overhead_seconds";
    /// gauge — label: server — 1 si l'amont est disponible
    pub const UPSTREAM_UP: &str = "mcportal_upstream_up";
    /// gauge — label: server — appels en cours
    pub const INFLIGHT: &str = "mcportal_inflight";
    /// counter — label: scope (global | server)
    pub const RATELIMIT_REJECTIONS: &str = "mcportal_ratelimit_rejections_total";
    /// counter
    pub const SEARCH_QUERIES: &str = "mcportal_search_queries_total";
    /// gauge — nombre d'outils dans le catalogue
    pub const CATALOG_TOOLS: &str = "mcportal_catalog_tools";
    /// counter — label: server
    pub const FEEDBACKS: &str = "mcportal_feedback_total";
}

/// Installe le subscriber `tracing`. À appeler une seule fois, au tout début de `main`.
pub fn init_logging(config: &MetricsConfig) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(config.log_level.to_lowercase()));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stdout);
    if config.json_logs {
        // flatten_event : les champs sont à la racine de l'objet JSON (plus simple dans Loki).
        builder
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .init();
    } else {
        builder.init();
    }
}

/// Installe l'enregistreur Prometheus global et renvoie la poignée qui sert à produire le
/// texte de `/metrics`. Renvoie `None` si l'export est désactivé.
pub fn install_prometheus(config: &MetricsConfig) -> anyhow::Result<Option<PrometheusHandle>> {
    if !config.prometheus.enabled {
        return Ok(None);
    }
    // Seaux d'histogramme adaptés à des appels d'outils (de 1 ms à 60 s).
    let buckets = [
        0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
    ];
    let handle = PrometheusBuilder::new()
        .set_buckets(&buckets)?
        .install_recorder()?;

    // L'exporter a besoin d'un entretien périodique (purge des histogrammes) quand on ne
    // passe pas par son serveur HTTP intégré.
    let upkeep = handle.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            upkeep.run_upkeep();
        }
    });
    Ok(Some(handle))
}
