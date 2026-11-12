//! Tests d'intégration de bout en bout :
//! client MCP (rmcp) ──▶ passerelle (HTTP) ──▶ serveur amont simulé (HTTP).

mod common;

use std::time::Duration;

use common::*;
use rmcp::{
    ClientLifecycleMode,
    model::{ProtocolVersion, ReadResourceRequestParams, ResourceContents},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Lit une ressource texte et la parse en JSON si possible.
async fn read(client: &Client, uri: &str) -> String {
    let result = client
        .read_resource(ReadResourceRequestParams::new(uri))
        .await
        .expect("lecture ressource");
    match &result.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("contenu inattendu: {other:?}"),
    }
}

async fn read_json(client: &Client, uri: &str) -> Value {
    serde_json::from_str(&read(client, uri).await).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Outils exposés et recherche
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn search_mode_exposes_only_gateway_tools() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;
    let client = connect(&gw.url).await;

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, vec!["search_tools", "call_tool", "send_feedback"]);
    ct.cancel();
}

#[tokio::test]
async fn search_then_call_tool() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;
    let client = connect(&gw.url).await;

    // 1. Recherche : le résultat doit contenir le nom à appeler ET l'inputSchema.
    let found = call(
        &client,
        "search_tools",
        json!({"query": "additionner deux entiers"}),
    )
    .await;
    let structured = found.structured_content.clone().unwrap();
    let first = &structured["results"][0];
    assert_eq!(first["name"], "mock_add");
    assert_eq!(first["server"], "mock");
    assert!(first["input_schema"]["properties"]["a"].is_object());

    // 2. Appel via call_tool.
    let result = call_via_gateway(&client, "mock_add", json!({"a": 2, "b": 40})).await;
    assert!(!is_error(&result));
    assert_eq!(text(&result), "42");
    ct.cancel();
}

#[tokio::test]
async fn direct_mode_lists_and_calls_upstream_tools() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(
        &[("mock", &up)],
        "  tool_search: {enabled: false}",
        "",
    ))
    .await;
    let client = connect(&gw.url).await;

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"mock_echo"), "{names:?}");
    assert!(names.contains(&"send_feedback"));
    assert!(!names.contains(&"search_tools"));

    let result = call(&client, "mock_echo", json!({"text": "bonjour"})).await;
    assert_eq!(text(&result), "bonjour");
    ct.cancel();
}

#[tokio::test]
async fn legacy_client_with_initialize_handshake_works() {
    // Un client antérieur à 2026-07-28 (poignée de main initialize) doit fonctionner aussi.
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;
    let client = connect_with(&gw.url, ClientLifecycleMode::Initialize).await;
    let _ = ProtocolVersion::V_2025_11_25; // documentaire : version négociée par défaut
    let result = call_via_gateway(&client, "mock_echo", json!({"text": "ancien"})).await;
    assert_eq!(text(&result), "ancien");
    ct.cancel();
}

// ---------------------------------------------------------------------------------------------
// Erreurs exploitables par le LLM (isError: true)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn unknown_tool_suggests_close_names() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;
    let client = connect(&gw.url).await;

    let result = call_via_gateway(&client, "mock_ecoh", json!({})).await;
    assert!(is_error(&result));
    let message = text(&result);
    assert!(message.contains("outil inconnu"), "{message}");
    ct.cancel();
}

#[tokio::test]
async fn invalid_arguments_are_rejected_before_upstream_call() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;
    let client = connect(&gw.url).await;

    let result = call_via_gateway(&client, "mock_add", json!({"a": "deux"})).await;
    assert!(is_error(&result));
    let message = text(&result);
    assert!(message.contains("arguments invalides"), "{message}");
    // Ni `b` manquant ni `a` du mauvais type ne doivent passer inaperçus.
    assert!(message.contains("/a") && message.contains("b"), "{message}");
    // L'amont n'a pas été appelé.
    let detail = read_json(&client, "catalog://servers/mock").await;
    assert_eq!(detail["metrics"]["calls_total"], 0);
    ct.cancel();
}

#[tokio::test]
async fn upstream_tool_error_is_passed_through() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;
    let client = connect(&gw.url).await;

    let result = call_via_gateway(&client, "mock_fail", json!({})).await;
    assert!(is_error(&result));
    assert_eq!(text(&result), "échec volontaire");
    ct.cancel();
}

#[tokio::test]
async fn slow_upstream_times_out() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "    timeout: 0.3")).await;
    let client = connect(&gw.url).await;

    let result = call_via_gateway(&client, "mock_slow", json!({"ms": 2000})).await;
    assert!(is_error(&result));
    assert!(
        text(&result).contains("n'a pas répondu"),
        "{}",
        text(&result)
    );
    ct.cancel();
}

#[tokio::test]
async fn server_concurrency_limit_rejects_excess_calls() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(
        &[("mock", &up)],
        "  concurrency: {global_limit: 10, acquire_timeout_seconds: 0.1}",
        "    max_concurrency: 1",
    ))
    .await;
    let client = connect(&gw.url).await;

    // Le premier appel occupe l'unique place pendant 1 s ; le second doit être refusé.
    let slow = call_via_gateway(&client, "mock_slow", json!({"ms": 1000}));
    let second = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        call_via_gateway(&client, "mock_echo", json!({"text": "x"})).await
    };
    let (slow, second) = tokio::join!(slow, second);
    assert!(!is_error(&slow));
    assert!(is_error(&second));
    assert!(text(&second).contains("saturé"), "{}", text(&second));
    ct.cancel();
}

// ---------------------------------------------------------------------------------------------
// Catalogue, santé, send_feedback
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn unreachable_upstream_does_not_break_catalog() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    // Port 1 : connexion refusée immédiatement.
    let gw = spawn_gateway(config(
        &[("mock", &up), ("down", "http://127.0.0.1:1/mcp")],
        "",
        "    timeout: 2",
    ))
    .await;
    let client = connect(&gw.url).await;

    let servers = read_json(&client, "catalog://servers").await;
    let by_name = |n: &str| {
        servers["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == n)
            .cloned()
            .unwrap()
    };
    assert_eq!(by_name("mock")["status"], "available");
    assert_eq!(by_name("down")["status"], "unavailable");
    assert!(
        by_name("down")["error"]
            .as_str()
            .unwrap()
            .contains("injoignable")
    );

    let tools = read_json(&client, "catalog://servers/down/tools").await;
    assert_eq!(tools["status"], "unavailable");

    // L'amont sain reste utilisable.
    let result = call_via_gateway(&client, "mock_echo", json!({"text": "ok"})).await;
    assert_eq!(text(&result), "ok");
    ct.cancel();
}

#[tokio::test]
async fn catalog_resources_describe_servers_and_tools() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "    tags: [calcul, test]")).await;
    let client = connect(&gw.url).await;

    let resources = client.list_all_resources().await.unwrap();
    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    for expected in [
        "catalog://servers",
        "catalog://domains",
        "catalog://guide",
        "catalog://servers/mock/tools",
    ] {
        assert!(uris.contains(&expected), "{expected} absent de {uris:?}");
    }

    let domains = read_json(&client, "catalog://domains").await;
    assert_eq!(domains["domains"]["test"][0]["name"], "mock");

    let inventory = read_json(&client, "catalog://servers/mock/tools").await;
    let add = inventory["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["upstream_name"] == "add")
        .unwrap();
    assert_eq!(add["name"], "mock_add");
    assert!(add["input_schema"]["properties"]["b"].is_object());

    let guide = read(&client, "catalog://guide").await;
    assert!(guide.contains("Utiliser la passerelle MCP"));
    ct.cancel();
}

#[tokio::test]
async fn guide_appends_deployment_guide_reread_on_each_read() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let dir = std::env::temp_dir().join(format!("mcportal-guide-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("deploy.md"), "# Démarches v1").unwrap();
    std::fs::write(
        dir.join("registry.yaml"),
        format!("gateway: {{name: T, catalog: {{guide_path: deploy.md}}}}\nservers:\n  - {{name: mock, url: {up}}}\n"),
    )
    .unwrap();
    let cfg = mcportal::config::Config::load(&dir.join("registry.yaml")).unwrap();
    let gw = spawn_gateway(cfg).await;
    let client = connect(&gw.url).await;

    let guide = read(&client, "catalog://guide").await;
    assert!(guide.contains("Utiliser la passerelle MCP") && guide.contains("# Démarches v1"));

    // Modifié sur le disque → visible sans redémarrage.
    std::fs::write(dir.join("deploy.md"), "# Démarches v2").unwrap();
    assert!(
        read(&client, "catalog://guide")
            .await
            .contains("# Démarches v2")
    );
    std::fs::remove_dir_all(&dir).ok();
    ct.cancel();
}

#[tokio::test]
async fn send_feedback_validates_server_name() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;
    let client = connect(&gw.url).await;

    let ok = call(
        &client,
        "send_feedback",
        json!({"server": "mock", "author": "test", "feedback": "RAS"}),
    )
    .await;
    assert!(!is_error(&ok));
    let ko = call(
        &client,
        "send_feedback",
        json!({"server": "inconnu", "author": "test", "feedback": "?"}),
    )
    .await;
    assert!(is_error(&ko));
    assert!(text(&ko).contains("mock"));
    ct.cancel();
}

#[tokio::test]
async fn health_ready_and_graceful_shutdown() {
    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;
    let http = reqwest::Client::new();

    let health: Value = http
        .get(format!("{}/health", gw.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["ready"], true);
    assert_eq!(health["servers"]["mock"]["status"], "available");
    assert_eq!(health["tools"], 5);

    // Un appel lent en cours doit se terminer malgré la demande d'arrêt (drain).
    let client = connect(&gw.url).await;
    let slow =
        tokio::spawn(
            async move { call_via_gateway(&client, "mock_slow", json!({"ms": 500})).await },
        );
    tokio::time::sleep(Duration::from_millis(100)).await;
    gw.stop.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !gw.state.is_ready(),
        "/ready doit passer à 503 dès le signal"
    );

    let result = slow.await.unwrap();
    assert_eq!(text(&result), "fini");
    tokio::time::timeout(Duration::from_secs(5), gw.handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    ct.cancel();
}

// ---------------------------------------------------------------------------------------------
// Relais de la progression
// ---------------------------------------------------------------------------------------------

/// Client qui enregistre les notifications de progression reçues.
#[derive(Clone, Default)]
struct ProgressClient {
    received: std::sync::Arc<std::sync::Mutex<Vec<(rmcp::model::ProgressToken, f64)>>>,
}

impl rmcp::ClientHandler for ProgressClient {
    async fn on_progress(
        &self,
        params: rmcp::model::ProgressNotificationParam,
        _context: rmcp::service::NotificationContext<rmcp::RoleClient>,
    ) {
        self.received
            .lock()
            .unwrap()
            .push((params.progress_token, params.progress));
    }
}

#[tokio::test]
async fn progress_notifications_are_relayed_with_client_token() {
    use rmcp::{
        ClientServiceExt,
        model::CallToolRequestParams,
        transport::{
            StreamableHttpClientTransport,
            streamable_http_client::StreamableHttpClientTransportConfig,
        },
    };

    let ct = CancellationToken::new();
    let up = spawn_upstream(&ct).await;
    let gw = spawn_gateway(config(&[("mock", &up)], "", "")).await;

    let handler = ProgressClient::default();
    let received = handler.received.clone();
    let client = handler
        .serve_with_lifecycle(
            StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(gw.url.as_str()),
            ),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap();

    // rmcp choisit lui-même le jeton de progression de chaque requête : on envoie la requête
    // « à la main » pour le connaître. La passerelle en utilise un autre vers l'amont et doit
    // restituer celui-ci au client.
    let params = CallToolRequestParams::new("call_tool").with_arguments(
        json!({"name": "mock_progress", "arguments": {}})
            .as_object()
            .cloned()
            .unwrap(),
    );
    let handle = client
        .send_request_with_option(
            rmcp::model::ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(params)),
            rmcp::service::PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    let token = handle.progress_token.clone();
    let result = match handle.await_response().await.unwrap() {
        rmcp::model::ServerResult::CallToolResult(r) => r,
        other => panic!("réponse inattendue: {other:?}"),
    };
    assert_eq!(text(&result), "terminé");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let received = received.lock().unwrap().clone();
    assert_eq!(received.len(), 3, "{received:?}");
    assert!(received.iter().all(|(t, _)| *t == token), "{received:?}");
    assert_eq!(
        received.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
        vec![1.0, 2.0, 3.0]
    );
    ct.cancel();
}
