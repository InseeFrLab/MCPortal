//! Helpers shared by the integration tests and the benchmark:
//!
//! - [`MockUpstream`]: a real MCP server (rmcp) with a few test tools;
//! - [`spawn_upstream`] / [`spawn_gateway`]: start an upstream and a gateway on random
//!   127.0.0.1 ports;
//! - [`connect`]: an MCP client (rmcp) connected to a URL.
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
// Mock upstream server
// ---------------------------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EchoArgs {
    /// Text to send back
    pub text: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    /// First integer
    pub a: i64,
    /// Second integer
    pub b: i64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SlowArgs {
    /// Wait duration in milliseconds
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

    #[tool(description = "Returns the received text (echo)")]
    fn echo(&self, Parameters(EchoArgs { text }): Parameters<EchoArgs>) -> String {
        text
    }

    #[tool(description = "Adds two integers (addition)")]
    fn add(&self, Parameters(AddArgs { a, b }): Parameters<AddArgs>) -> String {
        (a + b).to_string()
    }

    #[tool(description = "Always fails: returns a tool error")]
    fn fail(&self) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text("intentional failure")])
    }

    #[tool(description = "Sends three progress notifications then responds")]
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
        "done".into()
    }

    #[tool(description = "Waits for a while before responding (slow)")]
    async fn slow(&self, Parameters(SlowArgs { ms }): Parameters<SlowArgs>) -> String {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        "finished".into()
    }
}

#[tool_handler]
impl ServerHandler for MockUpstream {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

/// Starts a mock upstream. Returns its MCP URL; it stops when the token is cancelled.
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
// Gateway
// ---------------------------------------------------------------------------------------------

pub struct TestGateway {
    /// MCP URL of the gateway.
    pub url: String,
    /// HTTP base URL (for /health, /ready).
    pub base: String,
    pub state: Arc<AppState>,
    pub stop: CancellationToken,
    pub handle: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// Starts a gateway with this config and waits until it is ready.
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

/// Test config: one "mock" server (or more) pointing to the given URLs.
/// `extra_gateway` and `extra_server` are inserted as is into the YAML.
pub fn config(upstreams: &[(&str, &str)], extra_gateway: &str, extra_server: &str) -> Config {
    let mut yaml = format!(
        "gateway:\n  name: Test Gateway\n  health: {{probe_interval_seconds: 3600, failure_threshold: 1}}\n{extra_gateway}\nservers:\n"
    );
    for (name, url) in upstreams {
        yaml.push_str(&format!(
            "  - name: {name}\n    url: {url}\n    domain: test\n    description: Test server\n{extra_server}\n"
        ));
    }
    Config::from_yaml(&yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"))
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

pub type Client = RunningService<RoleClient, ()>;

/// MCP 2026-07-28 client (stateless).
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
    .expect("MCP connection")
}

/// Calls a tool with JSON arguments.
pub async fn call(client: &Client, name: &str, arguments: Value) -> CallToolResult {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
        .await
        .expect("MCP call")
}

/// Calls the gateway's `call_tool`.
pub async fn call_via_gateway(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    call(
        client,
        "call_tool",
        json!({ "name": tool, "arguments": arguments }),
    )
    .await
}

/// Text of the first content block.
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
