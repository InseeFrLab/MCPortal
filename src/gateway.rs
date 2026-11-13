//! # The MCP server exposed to (downstream) clients
//!
//! [`Gateway`] implements rmcp's `ServerHandler` trait: rmcp handles the protocol (JSON-RPC,
//! HTTP transport, version negotiation) and calls our methods for each request. The trait is
//! implemented **by hand** (without the `#[tool]` macros) because the list of exposed tools
//! is dynamic: it depends on the upstreams.
//!
//! ## Exposed tools
//!
//! | `tool_search.enabled` | `tools/list` returns |
//! | --- | --- |
//! | `true` (default) | `search_tools`, `call_tool`, `send_feedback` |
//! | `false` | `send_feedback` + every upstream tool, under its name `{server}_{tool}` |
//!
//! In both modes, a direct call to `{server}_{tool}` is accepted.
//!
//! ## Typing of `call_tool`
//!
//! The target tool's arguments are only known at runtime (its `inputSchema`): they cannot be
//! typed statically. So we mirror the protocol shape **exactly** (`CallToolRequestParams`:
//! `name` + `arguments` JSON object) — see [`CallToolArgs`] — and validate `arguments`
//! against the target tool's schema before calling it.
//!
//! ## Two levels of error (as recommended by the MCP spec)
//!
//! - **Tool error** (`CallToolResult { isError: true }`): anything the LLM can fix or work
//!   around — unknown tool, invalid arguments, unreachable upstream, timeout, saturation.
//!   The message is written for it.
//! - **JSON-RPC error** (`Err(McpError)`): malformed requests (parameters of our own tools
//!   that cannot be deserialized). Clients often display them opaquely.
//!
//! ## Lifecycle of a `call_tool` request
//!
//! ```text
//! 1. read the current catalog (ArcSwap::load, lock-free)
//! 2. resolve the exposed name → (server, upstream name)   otherwise: error + close names
//! 3. validate the arguments (compiled JSON Schema)        otherwise: detailed error
//! 4. acquire permits (global then server)                 otherwise: "saturated"
//! 5. call the upstream (timeout, progress, cancellation)
//! 6. return the upstream result AS IS (content, structuredContent, isError, _meta…)
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

/// Names of the gateway's own tools.
pub const SEARCH_TOOLS: &str = "search_tools";
pub const CALL_TOOL: &str = "call_tool";
pub const SEND_FEEDBACK: &str = "send_feedback";

// ---------------------------------------------------------------------------------------------
// Arguments of the gateway's tools
// ---------------------------------------------------------------------------------------------
// The `///` comments on fields become the descriptions of the JSON Schema exposed to the
// client (through `schemars`): they are therefore written for the LLM.

/// Arguments of `search_tools`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchToolsArgs {
    /// Keywords describing what you want to do, preferably in the language of the tool
    /// descriptions (e.g. "geocode an address", "population datasets").
    pub query: String,
    /// Maximum number of results (capped by the gateway configuration).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Arguments of `call_tool`: same shape as the MCP protocol's `CallToolRequestParams`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallToolArgs {
    /// Full name of the tool to call, as returned by search_tools: "{server}_{tool}".
    pub name: String,
    /// The tool's arguments: a JSON object matching its inputSchema (not a string).
    #[serde(default)]
    pub arguments: JsonObject,
}

/// Arguments of `send_feedback`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendFeedbackArgs {
    /// Name of the server concerned (a registry server, see catalog://servers).
    pub server: String,
    /// Author of the feedback (name, identifier or role).
    pub author: String,
    /// The feedback: problem encountered, unexpected behavior, suggestion…
    pub feedback: String,
}

// ---------------------------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------------------------

/// The gateway's MCP handler. Cheaply cloned (an `Arc`) for each HTTP request.
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

    /// Definitions of the gateway's own tools.
    fn gateway_tools(&self) -> Vec<Tool> {
        let mut tools = Vec::new();
        if self.search_enabled() {
            tools.push(Tool::new(
                SEARCH_TOOLS,
                "Searches for tools across all the gateway's servers (full-text search). \
                 Returns, for each tool, the name to call and its inputSchema. \
                 Use it before call_tool.",
                schema_for_type::<SearchToolsArgs>(),
            ));
            tools.push(Tool::new(
                CALL_TOOL,
                "Calls a tool of one of the gateway's servers. `name` is the full name returned by \
                 search_tools ({server}_{tool}); `arguments` is a JSON object matching its inputSchema.",
                schema_for_type::<CallToolArgs>(),
            ));
        }
        tools.push(Tool::new(
            SEND_FEEDBACK,
            "Reports a problem or a remark about one of the gateway's servers. \
             The feedback is recorded in the gateway log.",
            schema_for_type::<SendFeedbackArgs>(),
        ));
        tools
    }

    /// Instructions sent to the client on connection: the mechanics in brief + the list of
    /// servers (generated from the registry, never written by hand).
    fn instructions(&self) -> String {
        let cfg = &self.state.config;
        let mut text = format!(
            "{}: MCP gateway that aggregates several servers.\n",
            cfg.gateway.name
        );
        if self.search_enabled() {
            text.push_str(
                "The servers' tools are not listed directly: search for them with \
                 search_tools, then call them with call_tool (full name {server}_{tool}).\n",
            );
        } else {
            text.push_str("The servers' tools are exposed under the name {server}_{tool}.\n");
        }
        text.push_str("\nAvailable servers:\n");
        for s in cfg.enabled_servers() {
            let domain = s
                .domain
                .as_deref()
                .map(|d| format!(" [{d}]"))
                .unwrap_or_default();
            let description = s.description.as_deref().unwrap_or("");
            text.push_str(&format!("- {}{domain}: {description}\n", s.name));
        }
        let catalog = &cfg.gateway.catalog;
        if catalog.enabled {
            text.push_str("\nCatalog as resources: catalog://servers, catalog://domains");
            if catalog.include_tool_inventory {
                text.push_str(", catalog://servers/{name}/tools");
            }
            text.push('.');
            if catalog.include_guide {
                text.push_str(" Read catalog://guide for detailed usage instructions.");
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
                // Return the full inputSchema: without it, the LLM cannot build call_tool's
                // `arguments` correctly.
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
            value["hint"] =
                json!("No tool found: rephrase with other keywords or check catalog://servers.");
        }
        // `structured` fills structuredContent AND a text block with the same JSON (for
        // clients that only read `content`).
        CallToolResult::structured(value)
    }

    // --- send_feedback --------------------------------------------------------------------

    fn send_feedback(&self, args: SendFeedbackArgs) -> CallToolResult {
        if !self.state.upstreams.contains_key(&args.server) {
            let known: Vec<&str> = self.state.upstreams.keys().map(String::as_str).collect();
            return tool_error(format!(
                "unknown server '{}'. Registry servers: {}",
                args.server,
                known.join(", ")
            ));
        }
        // The FEEDBACK label makes it possible to filter these lines in Loki/Grafana.
        tracing::info!(
            label = "FEEDBACK",
            server = %args.server,
            author = %args.author,
            feedback = %args.feedback,
            "FEEDBACK"
        );
        metrics::counter!(metric::FEEDBACKS, "server" => args.server.clone()).increment(1);
        CallToolResult::success(vec![ContentBlock::text(
            "Thank you, your feedback has been recorded.",
        )])
    }

    // --- call_tool (relay to an upstream) ------------------------------------------------

    /// Relays a call to the upstream tool `name`. See "Lifecycle of a request" above.
    async fn forward(
        &self,
        name: &str,
        arguments: JsonObject,
        original: &CallToolRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> CallToolResponse {
        let started = Instant::now();

        // 1-2. Resolve the name in the current catalog.
        let catalog = self.state.catalog.load();
        let Some(entry) = catalog.tools.get(name) else {
            let suggestions: Vec<String> = catalog
                .index
                .search(name, 3)
                .into_iter()
                .map(|h| h.name)
                .collect();
            let hint = if suggestions.is_empty() {
                "Use search_tools to find the right name.".to_owned()
            } else {
                format!(
                    "Close names: {}. Use search_tools if in doubt.",
                    suggestions.join(", ")
                )
            };
            record_call("unknown", name, "invalid", started, None);
            return tool_error(format!("unknown tool '{name}'. {hint}")).into();
        };
        let server = entry.server.as_str();

        // 3. Validate the arguments against the target tool's inputSchema.
        let arguments = Value::Object(arguments);
        if let Some(validator) = &entry.validator {
            let errors: Vec<String> = validator
                .iter_errors(&arguments)
                .map(|e| {
                    let path = e.instance_path().to_string();
                    let path = if path.is_empty() {
                        "(root)".to_owned()
                    } else {
                        path
                    };
                    format!("- {path}: {e}")
                })
                .collect();
            if !errors.is_empty() {
                record_call(server, name, "invalid", started, None);
                return tool_error(format!(
                    "invalid arguments for '{name}':\n{}\nCheck its inputSchema (search_tools).",
                    errors.join("\n")
                ))
                .into();
            }
        }
        let Value::Object(arguments) = arguments else {
            unreachable!()
        };

        // 4. Concurrency permits (released automatically at the end of the function).
        let upstream = &self.state.upstreams[server];
        let _permit = match self.state.limits.acquire(upstream).await {
            Ok(permit) => permit,
            Err(rejected) => {
                record_call(server, name, "rejected", started, None);
                return tool_error(rejected.to_string()).into();
            }
        };

        // 5. Upstream call. The MRTR fields (inputResponses / requestState) are forwarded so
        //    that a multi-round upstream ↔ downstream client exchange works through the gateway.
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
            // The downstream client cancelled: drop the upstream call (rmcp notifies the upstream).
            _ = context.ct.cancelled() => None,
        };
        let upstream_elapsed = upstream_started.elapsed();

        // 6. Upstream result returned as is; transport errors → tool error.
        match outcome {
            None => {
                record_call(server, name, "cancelled", started, Some(upstream_elapsed));
                tool_error("call cancelled by the client".to_owned()).into()
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
                    "{e}. Check catalog://servers for the servers' status."
                ))
                .into()
            }
        }
    }
}

/// Error result readable by the LLM (`isError: true`).
fn tool_error(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Deserializes the arguments of a gateway tool. Failure = malformed request
/// → JSON-RPC `invalid_params` error.
fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    arguments: Option<JsonObject>,
) -> Result<T, McpError> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default()))
        .map_err(|e| McpError::invalid_params(format!("invalid arguments for {tool}: {e}"), None))
}

/// Log + metrics of a relayed call.
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
        "tool call"
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
            // The gateway's three tools do not change during the pod's lifetime.
            3_600_000
        } else {
            let catalog = self.state.catalog.load();
            tools.extend(catalog.tools.values().map(|e| e.tool.clone()));
            // Upstream tools may change at the next refresh.
            let min_ttl = self
                .state
                .enabled_upstreams()
                .map(|u| u.config.cache_ttl)
                .min();
            min_ttl.unwrap_or(300) * 1000
        };
        // The list is the same for every client → shared cache allowed (`public`).
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
            // Direct call of an upstream tool by its exposed name.
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
        // Resources depend on the registry (fixed for the pod's lifetime): one-hour cache.
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
        // Server status changes: no cache (ttl 0).
        let mut result = ReadResourceResult::new(vec![contents]);
        result.ttl_ms = Some(0);
        result.cache_scope = Some(CacheScope::Public);
        Ok(result.into())
    }
}
