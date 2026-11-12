//! Outils partagés par les tests d'intégration et le benchmark :
//!
//! - [`MockUpstream`] : un vrai serveur MCP (rmcp) avec quelques outils de test ;
//! - [`spawn_upstream`] / [`spawn_gateway`] : démarrent amont et passerelle sur des ports
//!   aléatoires de 127.0.0.1 ;
//! - [`connect`] : un client MCP (rmcp) connecté à une URL.
#![allow(dead_code)]

use std::{sync::Arc, time::Duration};

use mcportal::{config::Config, server, state::AppState};
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, RoleClient, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ProtocolVersion, ServerCapabilities,
        ServerConfig,
    },
    schemars,
    service::RunningService,
    tool, tool_handler, tool_router,
    transport::{
        StreamableHttpClientTransport,
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
        },
    },
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------------------------
// Serveur amont simulé
// ---------------------------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EchoArgs {
    /// Texte à renvoyer
    pub text: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    /// Premier entier
    pub a: i64,
    /// Second entier
    pub b: i64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SlowArgs {
    /// Durée d'attente en millisecondes
    pub ms: u64,
}

#[derive(Clone)]
pub struct MockUpstream {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MockUpstream {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Renvoie le texte reçu (écho)")]
    fn echo(&self, Parameters(EchoArgs { text }): Parameters<EchoArgs>) -> String {
        text
    }

    #[tool(description = "Additionne deux nombres entiers (addition)")]
    fn add(&self, Parameters(AddArgs { a, b }): Parameters<AddArgs>) -> String {
        (a + b).to_string()
    }

    #[tool(description = "Échoue toujours : renvoie une erreur d'outil")]
    fn fail(&self) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text("échec volontaire")])
    }

    #[tool(description = "Envoie trois notifications de progression puis répond")]
    async fn progress(
        &self,
        meta: rmcp::model::RequestMetaObject,
        peer: rmcp::Peer<rmcp::RoleServer>,
    ) -> String {
        if let Some(token) = meta.get_progress_token() {
            for step in 1..=3 {
                let _ = peer
                    .notify_progress(
                        rmcp::model::ProgressNotificationParam::new(token.clone(), step as f64)
                            .with_total(3.0),
                    )
                    .await;
            }
        }
        "terminé".into()
    }

    #[tool(description = "Attend un certain temps avant de répondre (lenteur)")]
    async fn slow(&self, Parameters(SlowArgs { ms }): Parameters<SlowArgs>) -> String {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        "fini".into()
    }
}

#[tool_handler]
impl ServerHandler for MockUpstream {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

/// Démarre un amont simulé. Renvoie son URL MCP ; il s'arrête quand le jeton est annulé.
pub async fn spawn_upstream(ct: &CancellationToken) -> String {
    let service = StreamableHttpService::new(
        || Ok(MockUpstream::new()),
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_sse_keep_alive(None)
            .with_cancellation_token(ct.child_token()),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ct = ct.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(ct.cancelled_owned())
            .await;
    });
    format!("http://{addr}/mcp")
}

// ---------------------------------------------------------------------------------------------
// Passerelle
// ---------------------------------------------------------------------------------------------

pub struct TestGateway {
    /// URL MCP de la passerelle.
    pub url: String,
    /// URL de base HTTP (pour /health, /ready).
    pub base: String,
    pub state: Arc<AppState>,
    pub stop: CancellationToken,
    pub handle: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// Démarre une passerelle avec cette config et attend qu'elle soit prête.
pub async fn spawn_gateway(config: Config) -> TestGateway {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let path = config.gateway.http.path.clone();
    let state = AppState::new(config);
    let stop = CancellationToken::new();
    let handle = tokio::spawn(server::serve(state.clone(), listener, None, stop.clone()));

    let base = format!("http://{addr}");
    let http = reqwest::Client::new();
    for _ in 0..200 {
        if let Ok(r) = http.get(format!("{base}/ready")).send().await
            && r.status().is_success()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    TestGateway {
        url: format!("{base}{path}"),
        base,
        state,
        stop,
        handle,
    }
}

/// Config de test : un serveur « mock » (ou plus) pointant vers les URLs données.
/// `extra_gateway` et `extra_server` sont insérés tels quels dans le YAML.
pub fn config(upstreams: &[(&str, &str)], extra_gateway: &str, extra_server: &str) -> Config {
    let mut yaml = format!(
        "gateway:\n  name: Test Gateway\n  health: {{probe_interval_seconds: 3600, failure_threshold: 1}}\n{extra_gateway}\nservers:\n"
    );
    for (name, url) in upstreams {
        yaml.push_str(&format!(
            "  - name: {name}\n    url: {url}\n    domain: test\n    description: Serveur de test\n{extra_server}\n"
        ));
    }
    Config::from_yaml(&yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"))
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

pub type Client = RunningService<RoleClient, ()>;

/// Client MCP 2026-07-28 (stateless).
pub async fn connect(url: &str) -> Client {
    connect_with(
        url,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
}

pub async fn connect_with(url: &str, lifecycle: ClientLifecycleMode) -> Client {
    ().serve_with_lifecycle(
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(
            url,
        )),
        lifecycle,
    )
    .await
    .expect("connexion MCP")
}

/// Appelle un outil avec des arguments JSON.
pub async fn call(client: &Client, name: &str, arguments: Value) -> CallToolResult {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
        .await
        .expect("appel MCP")
}

/// Appelle `call_tool` de la passerelle.
pub async fn call_via_gateway(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    call(
        client,
        "call_tool",
        json!({ "name": tool, "arguments": arguments }),
    )
    .await
}

/// Texte du premier bloc de contenu.
pub fn text(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default()
}

pub fn is_error(result: &CallToolResult) -> bool {
    result.is_error == Some(true)
}
