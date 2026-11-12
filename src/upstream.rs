//! # Serveurs amont : connexion, appels, santé
//!
//! Un [`Upstream`] représente **un** serveur MCP du registre. Il regroupe :
//!
//! - sa configuration ([`UpstreamConfig`]) ;
//! - un **client rmcp partagé** par tous les utilisateurs de la passerelle. En Streamable HTTP
//!   chaque appel est un POST indépendant : un seul client suffit pour des appels concurrents ;
//! - un **sémaphore** qui borne les appels simultanés (`max_concurrency`) ;
//! - son **état de santé** et le **cache de sa liste d'outils**, mis à jour par la boucle de
//!   rafraîchissement (voir [`crate::state`]).
//!
//! ## Cycle de vie de la connexion
//!
//! La connexion est ouverte **paresseusement** (au premier besoin) et refaite si le transport
//! se ferme. Un amont injoignable au démarrage ne bloque donc pas la passerelle : il sera
//! simplement marqué « unavailable » et retenté à la sonde suivante.
//!
//! On négocie la version de protocole en mode `Auto` : on tente d'abord `server/discover`
//! (MCP 2026-07-28, sans session) et on retombe sur l'ancienne poignée de main `initialize`
//! si l'amont est plus ancien. rmcp gère aussi le renouvellement d'une session expirée
//! (`reinit_on_expired_session`) pour ces amonts anciens.
//!
//! ## Relais des notifications de progression
//!
//! Quand le client aval fournit un `progressToken`, il faut relayer les
//! `notifications/progress` de l'amont vers lui. Deux subtilités :
//!
//! - le client rmcp **impose son propre jeton** à chaque requête amont (un compteur propre à
//!   la connexion) : on ne le connaît qu'après l'envoi (`RequestHandle::progress_token`) ;
//! - ce compteur repart de zéro à chaque connexion : la table de correspondance
//!   ([`ProgressRouter`]) est donc **propre à chaque connexion** amont.
//!
//! Le [`ProgressRouter`] associe « jeton amont → (client aval, jeton aval) » et restitue au
//! client aval son jeton d'origine.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

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

/// Une connexion ouverte vers un amont : le client rmcp et la table de relais de progression
/// propre à cette connexion.
struct Connection {
    client: RunningService<RoleClient, UpstreamClientHandler>,
    progress: Arc<ProgressRouter>,
}

// ---------------------------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------------------------

/// Ce qui peut mal se passer en parlant à un amont.
///
/// Toutes ces erreurs finissent en `CallToolResult { isError: true }` côté aval : ce sont des
/// erreurs **exploitables par le LLM** (il peut réessayer, changer d'outil…), pas des
/// requêtes mal formées.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("serveur amont '{server}' injoignable: {reason}")]
    Unavailable { server: String, reason: String },
    #[error("le serveur amont '{server}' n'a pas répondu en {timeout:?}")]
    Timeout { server: String, timeout: Duration },
    #[error("le serveur amont '{server}' a renvoyé une erreur: {message}")]
    Protocol { server: String, message: String },
}

// ---------------------------------------------------------------------------------------------
// Relais de progression
// ---------------------------------------------------------------------------------------------

/// Table « jeton amont → (client aval, jeton aval) » d'**une** connexion amont.
#[derive(Default)]
pub struct ProgressRouter {
    inner: Mutex<RouterInner>,
}

#[derive(Default)]
struct RouterInner {
    routes: HashMap<ProgressToken, (Peer<RoleServer>, ProgressToken)>,
    /// Notifications reçues **avant** l'enregistrement de leur route : la requête part vers
    /// l'amont avant qu'on connaisse son jeton, une notification très rapide peut donc
    /// arriver en premier. On la garde de côté (taille bornée) au lieu de la perdre.
    early: HashMap<ProgressToken, Vec<ProgressNotificationParam>>,
}

/// Au-delà, les notifications « orphelines » sont oubliées (jetons de requêtes sans relais).
const MAX_EARLY_TOKENS: usize = 256;

impl ProgressRouter {
    /// Enregistre la route d'un appel. Elle est supprimée quand le [`ProgressRoute`] renvoyé
    /// est détruit (fin de l'appel). Les notifications arrivées en avance sont relayées.
    pub fn register(
        self: &Arc<Self>,
        upstream_token: ProgressToken,
        downstream: Peer<RoleServer>,
        downstream_token: ProgressToken,
    ) -> ProgressRoute {
        let early = {
            let mut inner = self.inner.lock().expect("progress lock");
            inner.routes.insert(
                upstream_token.clone(),
                (downstream.clone(), downstream_token.clone()),
            );
            inner.early.remove(&upstream_token).unwrap_or_default()
        };
        if !early.is_empty() {
            tokio::spawn(async move {
                for mut params in early {
                    params.progress_token = downstream_token.clone();
                    let _ = downstream.notify_progress(params).await;
                }
            });
        }
        ProgressRoute {
            router: self.clone(),
            token: upstream_token,
        }
    }

    /// Renvoie la notification au client aval concerné.
    async fn forward(&self, mut params: ProgressNotificationParam) {
        let route = {
            let mut inner = self.inner.lock().expect("progress lock");
            match inner.routes.get(&params.progress_token) {
                Some(route) => Some(route.clone()),
                None => {
                    if inner.early.len() >= MAX_EARLY_TOKENS {
                        inner.early.clear();
                    }
                    inner
                        .early
                        .entry(params.progress_token.clone())
                        .or_default()
                        .push(params.clone());
                    None
                }
            }
        };
        if let Some((peer, downstream_token)) = route {
            params.progress_token = downstream_token;
            if let Err(e) = peer.notify_progress(params).await {
                tracing::debug!(error = %e, "relais de progression impossible");
            }
        }
    }
}

/// Garde RAII : tant qu'elle vit, la route de progression existe.
pub struct ProgressRoute {
    router: Arc<ProgressRouter>,
    token: ProgressToken,
}

impl Drop for ProgressRoute {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.router.inner.lock() {
            inner.routes.remove(&self.token);
            inner.early.remove(&self.token);
        }
    }
}

/// Envoie `notifications/cancelled` à l'amont si l'appel est abandonné (client aval parti,
/// timeout) avant d'avoir reçu sa réponse. `Drop` ne peut pas attendre : on lance une tâche.
struct CancelOnDrop {
    peer: Peer<RoleClient>,
    id: Option<RequestId>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let peer = self.peer.clone();
            tokio::spawn(async move {
                let reason = Some("appel abandonné par la passerelle".to_owned());
                let _ = peer
                    .notify_cancelled(CancelledNotificationParam::new(Some(id), reason))
                    .await;
            });
        }
    }
}

/// Handler côté **client** rmcp : décrit la passerelle auprès de l'amont et reçoit ses
/// notifications. On n'implémente que `on_progress` ; tout le reste garde le comportement
/// par défaut de rmcp.
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
// État de santé
// ---------------------------------------------------------------------------------------------

/// État observable d'un amont (lu par le catalogue et `/health`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Pas encore sondé.
    Unknown,
    Available,
    /// `failure_threshold` échecs consécutifs (ou transport non supporté).
    Unavailable,
    /// `enabled: false` dans le registre.
    Disabled,
}

/// Instantané de la santé d'un amont.
#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    pub status: Availability,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    /// Horodatage UNIX (secondes) de la dernière sonde.
    pub last_probe_unix: Option<u64>,
    pub last_success_unix: Option<u64>,
    /// Nombre d'outils dans le cache (None si jamais listés).
    pub tool_count: Option<usize>,
    /// Compteurs d'appels depuis le démarrage du pod.
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

/// Cache de la liste d'outils d'un amont.
#[derive(Debug, Clone)]
struct ToolCache {
    tools: Arc<Vec<Tool>>,
    fetched_at: Instant,
}

// ---------------------------------------------------------------------------------------------
// Upstream
// ---------------------------------------------------------------------------------------------

/// Un serveur MCP amont. Voir la documentation du module.
pub struct Upstream {
    pub config: UpstreamConfig,
    failure_threshold: u32,
    /// Limite `max_concurrency` de ce serveur.
    pub semaphore: Semaphore,
    /// Connexion courante. `tokio::sync::Mutex` car on la tient pendant l'`await` de connexion :
    /// deux requêtes simultanées ne déclenchent ainsi qu'une seule connexion.
    connection: tokio::sync::Mutex<Option<Arc<Connection>>>,
    /// `std::sync::Mutex` : sections critiques très courtes, jamais d'`await` dedans.
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
            connection: tokio::sync::Mutex::new(None),
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

    // --- Connexion ------------------------------------------------------------------------

    /// Renvoie la connexion courante, en (re)connectant si nécessaire.
    async fn connection(&self) -> Result<Arc<Connection>, UpstreamError> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_ref()
            && !connection.client.is_closed()
        {
            return Ok(connection.clone());
        }
        let connection = Arc::new(self.connect().await?);
        *guard = Some(connection.clone());
        Ok(connection)
    }

    /// Oublie la connexion courante (elle sera refaite au prochain appel).
    async fn invalidate(&self) {
        self.connection.lock().await.take();
    }

    async fn connect(&self) -> Result<Connection, UpstreamError> {
        let unavailable = |reason: String| UpstreamError::Unavailable {
            server: self.config.name.clone(),
            reason,
        };
        if self.config.transport == Transport::Sse {
            return Err(unavailable(
                "transport 'sse' déprécié et non supporté par rmcp 3.x : utilisez 'http'".into(),
            ));
        }

        let mut transport_config =
            StreamableHttpClientTransportConfig::with_uri(self.config.url.as_str())
                // Le transport ne doit pas être le goulot : notre sémaphore borne déjà les appels.
                .max_concurrent_requests(self.config.max_concurrency.max(1) + 1);
        // Amonts anciens (avec sessions) : rouvrir une session expirée automatiquement.
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
        match tokio::time::timeout(self.config.timeout(), connecting).await {
            Ok(Ok(client)) => {
                tracing::info!(server = %self.config.name, "connecté à l'amont");
                Ok(Connection { client, progress })
            }
            Ok(Err(e)) => Err(unavailable(e.to_string())),
            Err(_) => Err(unavailable(format!(
                "pas de réponse à l'initialisation en {:?}",
                self.config.timeout()
            ))),
        }
    }

    // --- Opérations MCP -------------------------------------------------------------------

    /// Liste **tous** les outils de l'amont (toutes les pages), avec timeout.
    pub async fn fetch_tools(&self) -> Result<Vec<Tool>, UpstreamError> {
        let connection = self.connection().await?;
        let result =
            tokio::time::timeout(self.config.timeout(), connection.client.list_all_tools()).await;
        self.map_result(result).await
    }

    /// Sonde légère : la première page de `tools/list`.
    ///
    /// `ping` n'existe plus dans MCP 2026-07-28 et `server/discover` n'existe pas dans les
    /// versions antérieures : `tools/list` est la seule requête que tous nos amonts servent.
    pub async fn probe(&self) -> Result<(), UpstreamError> {
        let connection = self.connection().await?;
        let result =
            tokio::time::timeout(self.config.timeout(), connection.client.list_tools(None)).await;
        self.map_result(result).await.map(|_| ())
    }

    /// Appelle un outil amont **sans** gérer les tours MRTR : si l'amont renvoie un
    /// `InputRequiredResult`, on le renvoie tel quel au client aval, qui fournira les réponses
    /// en rappelant `call_tool` (avec `inputResponses` / `requestState`), que l'on retransmet.
    ///
    /// La limite de concurrence n'est **pas** gérée ici mais dans [`crate::limits`].
    pub async fn call_tool(
        &self,
        params: CallToolRequestParams,
        progress: Option<(Peer<RoleServer>, ProgressToken)>,
    ) -> Result<CallToolResponse, UpstreamError> {
        self.calls_total.fetch_add(1, Ordering::Relaxed);
        let connection = self
            .connection()
            .await
            .inspect_err(|_| self.count_failure())?;

        let call = async {
            match progress {
                // Cas courant : `call_tool_once` gère lui-même l'annulation si on l'abandonne.
                None => connection.client.call_tool_once(params).await,
                Some(downstream) => Self::call_with_progress(&connection, params, downstream).await,
            }
        };
        let result = tokio::time::timeout(self.config.timeout(), call).await;
        self.map_result(result)
            .await
            .inspect_err(|_| self.count_failure())
    }

    /// Variante de `call_tool_once` qui relaie la progression : on envoie la requête à la main
    /// pour connaître le jeton choisi par rmcp, puis on enregistre la route avant d'attendre.
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
        // Les deux gardes vivent jusqu'à la fin de la fonction (ou son abandon).
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
        cancel.id = None; // réponse reçue : rien à annuler
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

    /// Convertit (timeout | erreur rmcp | succès) en `Result<T, UpstreamError>`, et invalide la
    /// connexion si l'erreur vient du transport.
    async fn map_result<T>(
        &self,
        result: Result<Result<T, ServiceError>, tokio::time::error::Elapsed>,
    ) -> Result<T, UpstreamError> {
        let server = self.config.name.clone();
        match result {
            Ok(Ok(value)) => Ok(value),
            Err(_) => Err(UpstreamError::Timeout {
                server,
                timeout: self.config.timeout(),
            }),
            // Erreur JSON-RPC renvoyée par l'amont : la connexion est saine.
            Ok(Err(ServiceError::McpError(e))) => Err(UpstreamError::Protocol {
                server,
                message: e.message.to_string(),
            }),
            // Toute autre erreur (transport fermé, réponse inattendue…) : on reconnectera.
            Ok(Err(e)) => {
                self.invalidate().await;
                Err(UpstreamError::Unavailable {
                    server,
                    reason: e.to_string(),
                })
            }
        }
    }

    // --- Cache d'outils et santé ----------------------------------------------------------

    /// Outils en cache (dernière liste connue, même si l'amont est tombé depuis).
    pub fn cached_tools(&self) -> Option<Arc<Vec<Tool>>> {
        self.tools
            .lock()
            .expect("tools lock")
            .as_ref()
            .map(|c| c.tools.clone())
    }

    /// Le cache a-t-il dépassé `cache_ttl` (ou est-il vide) ?
    pub fn cache_expired(&self) -> bool {
        match self.tools.lock().expect("tools lock").as_ref() {
            None => true,
            Some(c) => c.fetched_at.elapsed() >= self.config.cache_ttl(),
        }
    }

    /// Remplace le cache. Renvoie `true` si la liste a changé (→ reconstruire l'index).
    pub fn store_tools(&self, tools: Vec<Tool>) -> bool {
        let mut cache = self.tools.lock().expect("tools lock");
        let changed = cache.as_ref().map(|c| *c.tools != tools).unwrap_or(true);
        *cache = Some(ToolCache {
            tools: Arc::new(tools),
            fetched_at: Instant::now(),
        });
        changed
    }

    /// Enregistre le résultat d'une sonde.
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

    /// L'amont est-il considéré comme disponible ?
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

    /// Photographie de la santé, pour le catalogue et `/health`.
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
