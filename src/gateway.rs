//! # Le serveur MCP exposé aux clients (aval)
//!
//! [`Gateway`] implémente le trait `ServerHandler` de rmcp : rmcp s'occupe du protocole
//! (JSON-RPC, transport HTTP, négociation de version) et appelle nos méthodes pour chaque
//! requête. On implémente le trait **à la main** (sans les macros `#[tool]`) parce que la
//! liste des outils exposés est dynamique : elle dépend des amonts.
//!
//! ## Outils exposés
//!
//! | `tool_search.enabled` | `tools/list` renvoie |
//! | --- | --- |
//! | `true` (défaut) | `search_tools`, `call_tool`, `send_feedback` |
//! | `false` | `send_feedback` + tous les outils amont, sous leur nom `{serveur}_{outil}` |
//!
//! Dans les deux modes, un appel direct à `{serveur}_{outil}` est accepté.
//!
//! ## Typage de `call_tool`
//!
//! Les arguments de l'outil cible ne sont connus qu'à l'exécution (son `inputSchema`) : on ne
//! peut pas les typer statiquement. On reprend donc **exactement** la forme du protocole
//! (`CallToolRequestParams` : `name` + `arguments` objet JSON) — voir [`CallToolArgs`] — et on
//! valide `arguments` contre le schéma de l'outil cible avant de l'appeler.
//!
//! ## Deux niveaux d'erreur (recommandation de la spec MCP)
//!
//! - **Erreur d'outil** (`CallToolResult { isError: true }`) : tout ce que le LLM peut
//!   corriger ou contourner — outil inconnu, arguments invalides, amont injoignable, timeout,
//!   saturation. Le message est rédigé pour lui.
//! - **Erreur JSON-RPC** (`Err(McpError)`) : requêtes mal formées (paramètres de nos propres
//!   outils non désérialisables). Les clients les affichent souvent de façon opaque.
//!
//! ## Cycle d'une requête `call_tool`
//!
//! ```text
//! 1. lire le catalogue courant (ArcSwap::load, sans verrou)
//! 2. résoudre le nom exposé → (serveur, nom amont)        sinon : erreur + noms proches
//! 3. valider les arguments (JSON Schema compilé)           sinon : erreur détaillée
//! 4. acquérir les places (global puis serveur)             sinon : « saturé »
//! 5. appeler l'amont (timeout, progression, annulation)
//! 6. renvoyer le résultat amont TEL QUEL (content, structuredContent, isError, _meta…)
//! ```

use std::{sync::Arc, time::Instant};

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::common::schema_for_type,
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        Implementation, JsonObject, ListResourceTemplatesResult, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{catalog, state::AppState, telemetry::metric};

/// Nom des outils propres à la passerelle.
pub const SEARCH_TOOLS: &str = "search_tools";
pub const CALL_TOOL: &str = "call_tool";
pub const SEND_FEEDBACK: &str = "send_feedback";

// ---------------------------------------------------------------------------------------------
// Arguments des outils de la passerelle
// ---------------------------------------------------------------------------------------------
// Les `///` sur les champs deviennent les descriptions du JSON Schema exposé au client
// (via `schemars`) : ils sont donc rédigés pour le LLM.

/// Arguments de `search_tools`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchToolsArgs {
    /// Mots-clés décrivant ce que vous voulez faire, de préférence en français
    /// (ex. « géocoder une adresse », « jeux de données population »).
    pub query: String,
    /// Nombre maximal de résultats (plafonné par la configuration de la passerelle).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Arguments de `call_tool` : même forme que `CallToolRequestParams` du protocole MCP.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallToolArgs {
    /// Nom complet de l'outil à appeler, tel que renvoyé par search_tools : "{serveur}_{outil}".
    pub name: String,
    /// Arguments de l'outil : un objet JSON conforme à son inputSchema (pas une chaîne).
    #[serde(default)]
    pub arguments: JsonObject,
}

/// Arguments de `send_feedback`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendFeedbackArgs {
    /// Nom du serveur concerné (un serveur du registre, voir catalog://servers).
    pub server: String,
    /// Auteur du retour (nom, identifiant ou rôle).
    pub author: String,
    /// Le retour : problème rencontré, comportement inattendu, suggestion…
    pub feedback: String,
}

// ---------------------------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------------------------

/// Handler MCP de la passerelle. Cloné à bas coût (un `Arc`) pour chaque requête HTTP.
#[derive(Clone)]
pub struct Gateway {
    state: Arc<AppState>,
}

impl Gateway {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    fn search_enabled(&self) -> bool {
        self.state.config.gateway.tool_search.enabled
    }

    /// Définitions des outils propres à la passerelle.
    fn gateway_tools(&self) -> Vec<Tool> {
        let mut tools = Vec::new();
        if self.search_enabled() {
            tools.push(Tool::new(
                SEARCH_TOOLS,
                "Recherche des outils parmi tous les serveurs de la passerelle (recherche plein texte). \
                 Renvoie pour chaque outil son nom à appeler et son inputSchema. \
                 À utiliser avant call_tool.",
                schema_for_type::<SearchToolsArgs>(),
            ));
            tools.push(Tool::new(
                CALL_TOOL,
                "Appelle un outil d'un serveur de la passerelle. `name` est le nom complet renvoyé par \
                 search_tools ({serveur}_{outil}) ; `arguments` est un objet JSON conforme à son inputSchema.",
                schema_for_type::<CallToolArgs>(),
            ));
        }
        tools.push(Tool::new(
            SEND_FEEDBACK,
            "Signale un problème ou une remarque concernant un serveur de la passerelle. \
             Le retour est consigné dans le journal de la passerelle.",
            schema_for_type::<SendFeedbackArgs>(),
        ));
        tools
    }

    /// Instructions envoyées au client à la connexion : la mécanique en bref + la liste des
    /// serveurs (générée depuis le registre, jamais écrite à la main).
    fn instructions(&self) -> String {
        let cfg = &self.state.config;
        let mut text = format!(
            "{} : passerelle MCP qui agrège plusieurs serveurs.\n",
            cfg.gateway.name
        );
        if self.search_enabled() {
            text.push_str(
                "Les outils des serveurs ne sont pas listés directement : cherchez-les avec \
                 search_tools, puis appelez-les avec call_tool (nom complet {serveur}_{outil}).\n",
            );
        } else {
            text.push_str("Les outils des serveurs sont exposés sous le nom {serveur}_{outil}.\n");
        }
        text.push_str("\nServeurs disponibles :\n");
        for s in cfg.enabled_servers() {
            let domain = s
                .domain
                .as_deref()
                .map(|d| format!(" [{d}]"))
                .unwrap_or_default();
            let description = s.description.as_deref().unwrap_or("");
            text.push_str(&format!("- {}{domain} : {description}\n", s.name));
        }
        let catalog = &cfg.gateway.catalog;
        if catalog.enabled {
            text.push_str("\nCatalogue en ressources : catalog://servers, catalog://domains");
            if catalog.include_tool_inventory {
                text.push_str(", catalog://servers/{nom}/tools");
            }
            text.push('.');
            if catalog.include_guide {
                text.push_str(" Lisez catalog://guide pour le mode d'emploi détaillé.");
            }
            text.push('\n');
        }
        text
    }

    // --- search_tools ---------------------------------------------------------------------

    fn search_tools(&self, args: SearchToolsArgs) -> CallToolResult {
        metrics::counter!(metric::SEARCH_QUERIES).increment(1);
        let max = self.state.config.gateway.tool_search.max_results;
        let limit = args.limit.unwrap_or(max).clamp(1, max);

        let catalog = self.state.catalog.load();
        let results: Vec<Value> = catalog
            .index
            .search(&args.query, limit)
            .into_iter()
            .filter_map(|hit| catalog.tools.get(&hit.name).map(|entry| (hit, entry)))
            .map(|(hit, entry)| {
                // On renvoie l'inputSchema complet : sans lui, le LLM ne peut pas construire
                // les `arguments` de call_tool correctement.
                json!({
                    "name": hit.name,
                    "server": entry.server,
                    "title": entry.tool.title,
                    "description": entry.tool.description,
                    "input_schema": entry.tool.input_schema,
                    "annotations": entry.tool.annotations,
                    "score": hit.score,
                })
            })
            .collect();

        let mut value = json!({ "query": args.query, "results": results });
        if results.is_empty() {
            value["hint"] = json!(
                "Aucun outil trouvé : reformulez avec d'autres mots-clés ou consultez catalog://servers."
            );
        }
        // `structured` remplit structuredContent ET un bloc texte avec le même JSON (pour les
        // clients qui ne lisent que `content`).
        CallToolResult::structured(value)
    }

    // --- send_feedback --------------------------------------------------------------------

    fn send_feedback(&self, args: SendFeedbackArgs) -> CallToolResult {
        if !self.state.upstreams.contains_key(&args.server) {
            let known: Vec<&str> = self.state.upstreams.keys().map(String::as_str).collect();
            return tool_error(format!(
                "serveur inconnu '{}'. Serveurs du registre : {}",
                args.server,
                known.join(", ")
            ));
        }
        // Le label FEEDBACK permet de filtrer ces lignes dans Loki/Grafana.
        tracing::info!(
            label = "FEEDBACK",
            server = %args.server,
            author = %args.author,
            feedback = %args.feedback,
            "FEEDBACK"
        );
        metrics::counter!(metric::FEEDBACKS, "server" => args.server.clone()).increment(1);
        CallToolResult::success(vec![ContentBlock::text(
            "Merci, votre retour a été consigné.",
        )])
    }

    // --- call_tool (relais vers un amont) -------------------------------------------------

    /// Relaie un appel vers l'outil amont `name`. Voir « Cycle d'une requête » plus haut.
    async fn forward(
        &self,
        name: &str,
        arguments: JsonObject,
        original: &CallToolRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> CallToolResponse {
        let started = Instant::now();

        // 1-2. Résolution du nom dans le catalogue courant.
        let catalog = self.state.catalog.load();
        let Some(entry) = catalog.tools.get(name) else {
            let suggestions: Vec<String> = catalog
                .index
                .search(name, 3)
                .into_iter()
                .map(|h| h.name)
                .collect();
            let hint = if suggestions.is_empty() {
                "Utilisez search_tools pour trouver le bon nom.".to_owned()
            } else {
                format!(
                    "Noms proches : {}. Utilisez search_tools en cas de doute.",
                    suggestions.join(", ")
                )
            };
            record_call("unknown", name, "invalid", started, None);
            return tool_error(format!("outil inconnu '{name}'. {hint}")).into();
        };
        let server = entry.server.as_str();

        // 3. Validation des arguments contre l'inputSchema de l'outil cible.
        let arguments = Value::Object(arguments);
        if let Some(validator) = &entry.validator {
            let errors: Vec<String> = validator
                .iter_errors(&arguments)
                .map(|e| {
                    let path = e.instance_path().to_string();
                    let path = if path.is_empty() {
                        "(racine)".to_owned()
                    } else {
                        path
                    };
                    format!("- {path} : {e}")
                })
                .collect();
            if !errors.is_empty() {
                record_call(server, name, "invalid", started, None);
                return tool_error(format!(
                    "arguments invalides pour '{name}' :\n{}\nConsultez son inputSchema (search_tools).",
                    errors.join("\n")
                ))
                .into();
            }
        }
        let Value::Object(arguments) = arguments else {
            unreachable!()
        };

        // 4. Places de concurrence (rendues automatiquement à la fin de la fonction).
        let upstream = &self.state.upstreams[server];
        let _permit = match self.state.limits.acquire(upstream).await {
            Ok(permit) => permit,
            Err(rejected) => {
                record_call(server, name, "rejected", started, None);
                return tool_error(rejected.to_string()).into();
            }
        };

        // 5. Appel amont. On retransmet les champs MRTR (inputResponses / requestState) pour
        //    qu'un échange multi-tours amont ↔ client aval fonctionne à travers la passerelle.
        let mut params = CallToolRequestParams::new(entry.upstream_name.clone());
        params.arguments = Some(arguments);
        params.input_responses = original.input_responses.clone();
        params.request_state = original.request_state.clone();
        let progress = context
            .meta
            .get_progress_token()
            .map(|token| (context.peer.clone(), token));

        let upstream_started = Instant::now();
        let outcome = tokio::select! {
            r = upstream.call_tool(params, progress) => Some(r),
            // Le client aval a annulé : on abandonne l'appel amont (rmcp le notifie à l'amont).
            _ = context.ct.cancelled() => None,
        };
        let upstream_elapsed = upstream_started.elapsed();

        // 6. Résultat amont renvoyé tel quel ; erreurs de transport → erreur d'outil.
        match outcome {
            None => {
                record_call(server, name, "cancelled", started, Some(upstream_elapsed));
                tool_error("appel annulé par le client".to_owned()).into()
            }
            Some(Ok(response)) => {
                let status = match &response {
                    CallToolResponse::Complete(r) if r.is_error == Some(true) => "tool_error",
                    _ => "ok",
                };
                record_call(server, name, status, started, Some(upstream_elapsed));
                response
            }
            Some(Err(e)) => {
                let status = match e {
                    crate::upstream::UpstreamError::Timeout { .. } => "timeout",
                    _ => "upstream_error",
                };
                record_call(server, name, status, started, Some(upstream_elapsed));
                tool_error(format!(
                    "{e}. Consultez catalog://servers pour l'état des serveurs."
                ))
                .into()
            }
        }
    }
}

/// Résultat d'erreur lisible par le LLM (`isError: true`).
fn tool_error(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Désérialise les arguments d'un outil de la passerelle. Échec = requête mal formée
/// → erreur JSON-RPC `invalid_params`.
fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    arguments: Option<JsonObject>,
) -> Result<T, McpError> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default())).map_err(|e| {
        McpError::invalid_params(format!("arguments invalides pour {tool}: {e}"), None)
    })
}

/// Log + métriques d'un appel relayé.
fn record_call(
    server: &str,
    tool: &str,
    status: &'static str,
    started: Instant,
    upstream_elapsed: Option<std::time::Duration>,
) {
    let total = started.elapsed();
    metrics::counter!(metric::TOOL_CALLS, "server" => server.to_owned(), "tool" => tool.to_owned(), "status" => status)
        .increment(1);
    metrics::histogram!(metric::TOOL_CALL_DURATION, "server" => server.to_owned())
        .record(total.as_secs_f64());
    if let Some(upstream) = upstream_elapsed {
        metrics::histogram!(metric::GATEWAY_OVERHEAD)
            .record(total.saturating_sub(upstream).as_secs_f64());
    }
    tracing::info!(
        server,
        tool,
        status,
        duration_ms = total.as_millis() as u64,
        upstream_ms = upstream_elapsed.map(|d| d.as_millis() as u64),
        "appel d'outil"
    );
}

impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder().enable_tools();
        let capabilities = if self.state.config.gateway.catalog.enabled {
            capabilities.enable_resources().build()
        } else {
            capabilities.build()
        };
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new(
                self.state.config.gateway.name.clone(),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(self.instructions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools = self.gateway_tools();
        let ttl_ms = if self.search_enabled() {
            // Les trois outils de la passerelle ne changent pas pendant la vie du pod.
            3_600_000
        } else {
            let catalog = self.state.catalog.load();
            tools.extend(catalog.tools.values().map(|e| e.tool.clone()));
            // Les outils amont peuvent changer au prochain rafraîchissement.
            let min_ttl = self
                .state
                .enabled_upstreams()
                .map(|u| u.config.cache_ttl)
                .min();
            min_ttl.unwrap_or(300) * 1000
        };
        // La liste est la même pour tous les clients → cache partagé autorisé (`public`).
        Ok(ListToolsResult::with_all_items(tools)
            .with_ttl_ms(ttl_ms)
            .with_cache_scope(CacheScope::Public))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let search = self.search_enabled();
        match request.name.as_ref() {
            SEARCH_TOOLS if search => {
                let args: SearchToolsArgs = parse_args(SEARCH_TOOLS, request.arguments.clone())?;
                Ok(self.search_tools(args).into())
            }
            CALL_TOOL if search => {
                let args: CallToolArgs = parse_args(CALL_TOOL, request.arguments.clone())?;
                Ok(self
                    .forward(&args.name, args.arguments, &request, &context)
                    .await)
            }
            SEND_FEEDBACK => {
                let args: SendFeedbackArgs = parse_args(SEND_FEEDBACK, request.arguments.clone())?;
                Ok(self.send_feedback(args).into())
            }
            // Appel direct d'un outil amont par son nom exposé.
            name => {
                let arguments = request.arguments.clone().unwrap_or_default();
                Ok(self.forward(name, arguments, &request, &context).await)
            }
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        // Les ressources dépendent du registre (fixe pour la vie du pod) : cache d'une heure.
        Ok(
            ListResourcesResult::with_all_items(catalog::list_resources(&self.state))
                .with_ttl_ms(3_600_000)
                .with_cache_scope(CacheScope::Public),
        )
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(
            ListResourceTemplatesResult::with_all_items(catalog::list_templates(&self.state))
                .with_ttl_ms(3_600_000)
                .with_cache_scope(CacheScope::Public),
        )
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let contents = catalog::read(&self.state, &request.uri).await?;
        // L'état des serveurs change : pas de cache (ttl 0).
        let mut result = ReadResourceResult::new(vec![contents]);
        result.ttl_ms = Some(0);
        result.cache_scope = Some(CacheScope::Public);
        Ok(result.into())
    }
}
