//! Entry point of the `mcportal` binary.
//!
//! ```text
//! mcportal --config registry.yaml          (or MCPORTAL_CONFIG=registry.yaml)
//! ```
//!
//! Steps: read the registry → logs → metrics → shared state → HTTP servers → wait for
//! SIGTERM → graceful shutdown. All the logic is in the library (`src/lib.rs`).

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use mcportal::{config::Config, server, state::AppState, telemetry};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(version, about = "MCP gateway: aggregates several MCP servers")]
struct Args {
    /// Path to the YAML registry.
    #[arg(long, env = "MCPORTAL_CONFIG", default_value = "registry.yaml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // An invalid config stops startup with a clear message (the pod does not become Ready).
    let config =
        Config::load(&args.config).with_context(|| format!("loading {}", args.config.display()))?;

    telemetry::init_logging(&config.gateway.metrics);
    let prometheus = telemetry::install_prometheus(&config.gateway.metrics)?;

    let listener = TcpListener::bind(&config.gateway.http.bind)
        .await
        .with_context(|| format!("listening on {}", config.gateway.http.bind))?;
    let metrics = match prometheus {
        Some(handle) => {
            let addr = format!("0.0.0.0:{}", config.gateway.metrics.prometheus.port);
            let metrics_listener = TcpListener::bind(&addr)
                .await
                .with_context(|| format!("metrics listening on {addr}"))?;
            tracing::info!(addr = ?server::local_addr(&metrics_listener), "Prometheus metrics on /metrics");
            Some((handle, metrics_listener))
        }
        None => None,
    };
    tracing::info!(
        addr = ?server::local_addr(&listener),
        path = %config.gateway.http.path,
        servers = config.enabled_servers().count(),
        "MCP gateway started"
    );

    let state = AppState::new(config);
    let stop = CancellationToken::new();
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            server::shutdown_signal().await;
            stop.cancel();
        });
    }
    server::serve(state, listener, metrics, stop).await
}
