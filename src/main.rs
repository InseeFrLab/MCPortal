//! Point d'entrée du binaire `mcportal`.
//!
//! ```text
//! mcportal --config registry.yaml          (ou MCPORTAL_CONFIG=registry.yaml)
//! ```
//!
//! Étapes : lire le registre → logs → métriques → état partagé → serveurs HTTP → attendre
//! SIGTERM → arrêt propre. Toute la logique est dans la bibliothèque (`src/lib.rs`).

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use mcportal::{config::Config, server, state::AppState, telemetry};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(version, about = "Passerelle MCP : agrège plusieurs serveurs MCP")]
struct Args {
    /// Chemin du registre YAML.
    #[arg(long, env = "MCPORTAL_CONFIG", default_value = "registry.yaml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Une config invalide arrête le démarrage avec un message clair (le pod ne passe pas Ready).
    let config = Config::load(&args.config)
        .with_context(|| format!("chargement de {}", args.config.display()))?;

    telemetry::init_logging(&config.gateway.metrics);
    let prometheus = telemetry::install_prometheus(&config.gateway.metrics)?;

    let listener = TcpListener::bind(&config.gateway.http.bind)
        .await
        .with_context(|| format!("écoute sur {}", config.gateway.http.bind))?;
    let metrics = match prometheus {
        Some(handle) => {
            let addr = format!("0.0.0.0:{}", config.gateway.metrics.prometheus.port);
            let metrics_listener = TcpListener::bind(&addr)
                .await
                .with_context(|| format!("écoute métriques sur {addr}"))?;
            tracing::info!(addr = ?server::local_addr(&metrics_listener), "métriques Prometheus sur /metrics");
            Some((handle, metrics_listener))
        }
        None => None,
    };
    tracing::info!(
        addr = ?server::local_addr(&listener),
        path = %config.gateway.http.path,
        servers = config.enabled_servers().count(),
        "passerelle MCP démarrée"
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
