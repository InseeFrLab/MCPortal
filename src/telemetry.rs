//! # Observability: logs and metrics
//!
//! ## Logs (`tracing`)
//!
//! Logs go to **standard output**, one JSON line per event (`json_logs: true`), to be
//! collected by Loki and read in Grafana. Example:
//!
//! ```json
//! {"timestamp":"…","level":"INFO","message":"tool call","server":"insee","tool":"…","status":"ok","duration_ms":412}
//! ```
//!
//! The level comes from `gateway.metrics.log_level`, unless the `RUST_LOG` variable is set
//! (handy to debug a specific module: `RUST_LOG=mcportal=debug,rmcp=info`).
//!
//! Feedback from `send_feedback` is logged normally with `label = "FEEDBACK"`: a Loki
//! filter `| json | label="FEEDBACK"` is enough to isolate it.
//!
//! ## Metrics (`metrics` + Prometheus)
//!
//! The code calls the `metrics::counter!`, `gauge!`, `histogram!` macros wherever useful;
//! this module installs the Prometheus "recorder" that aggregates them. The text rendering
//! is served on `/metrics` by [`crate::server`], on a separate port.
//!
//! No metric carries a user identifier (risk of cardinality explosion).

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::EnvFilter;

use crate::config::MetricsConfig;

/// Metric names, grouped here to see the full inventory at a glance.
pub mod metric {
    /// counter — labels: server, tool, status (ok | tool_error | upstream_error | timeout | rejected | invalid)
    pub const TOOL_CALLS: &str = "mcportal_tool_calls_total";
    /// histogram (s) — label: server — total duration seen by the downstream client
    pub const TOOL_CALL_DURATION: &str = "mcportal_tool_call_duration_seconds";
    /// histogram (s) — time spent in the gateway itself (total − upstream time)
    pub const GATEWAY_OVERHEAD: &str = "mcportal_gateway_overhead_seconds";
    /// gauge — label: server — 1 if the upstream is available
    pub const UPSTREAM_UP: &str = "mcportal_upstream_up";
    /// gauge — label: server — in-flight calls
    pub const INFLIGHT: &str = "mcportal_inflight";
    /// counter — label: scope (always "server")
    pub const RATELIMIT_REJECTIONS: &str = "mcportal_ratelimit_rejections_total";
    /// counter
    pub const SEARCH_QUERIES: &str = "mcportal_search_queries_total";
    /// gauge — number of tools in the catalog
    pub const CATALOG_TOOLS: &str = "mcportal_catalog_tools";
    /// counter — label: server
    pub const FEEDBACKS: &str = "mcportal_feedback_total";
}

/// Installs the `tracing` subscriber. Call it once, at the very start of `main`.
pub fn init_logging(config: &MetricsConfig) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(config.log_level.to_lowercase()));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stdout);
    if config.json_logs {
        // flatten_event: fields are at the root of the JSON object (simpler in Loki).
        builder
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .init();
    } else {
        builder.init();
    }
}

/// Installs the global Prometheus recorder and returns the handle used to render the
/// `/metrics` text. Returns `None` if export is disabled.
pub fn install_prometheus(config: &MetricsConfig) -> anyhow::Result<Option<PrometheusHandle>> {
    if !config.prometheus.enabled {
        return Ok(None);
    }
    // Histogram buckets suited to tool calls (from 1 ms to 60 s).
    let buckets = [
        0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
    ];
    let handle = PrometheusBuilder::new()
        .set_buckets(&buckets)?
        .install_recorder()?;

    // The exporter needs periodic upkeep (histogram draining) when its built-in HTTP server
    // is not used.
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
