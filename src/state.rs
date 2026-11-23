//! # Shared gateway state
//!
//! [`AppState`] is created once at startup and shared (through `Arc`) by every request.
//! It holds two kinds of data:
//!
//! 1. data that is **fixed** for the pod's lifetime: the config, the [`Upstream`]s, the limits;
//! 2. a **tool catalog** ([`ToolCatalog`]) that changes when an upstream publishes new
//!    tools. It is stored behind an [`ArcSwap`].
//!
//! ## Why `ArcSwap`?
//!
//! The catalog is read on **every** request and rewritten **rarely** (refresh every
//! `cache_ttl` seconds). `ArcSwap` is made for this:
//!
//! - read: `catalog.load()` returns a pointer to the current version, lock-free (a few
//!   nanoseconds);
//! - write: a **new** complete catalog is built alongside, then `store()` publishes it
//!   atomically. In-flight requests keep the old version until they finish.
//!
//! No request ever sees a "half-rebuilt" catalog.
//!
//! ## Refresh loop
//!
//! One task per enabled upstream ([`spawn_refresh_loops`]) wakes up every
//! `probe_interval_seconds`:
//!
//! - if the tool cache is older than `cache_ttl` → full `tools/list`, and catalog rebuild if
//!   the list changed;
//! - otherwise → lightweight probe (first page of `tools/list`).
//!
//! In both cases the result updates the upstream's health.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use rmcp::model::{JsonObject, Tool};
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    limits::Limits,
    search::{Bm25ToolIndex, IndexEntry, ToolIndex},
    telemetry::metric,
    upstream::{Availability, Upstream},
};

/// A tool as exposed by the gateway.
pub struct ToolEntry {
    /// Name of the upstream server (= prefix).
    pub server: String,
    /// Name of the tool **on the upstream** (without prefix), used for the upstream call.
    pub upstream_name: String,
    /// Exposed definition: identical to the upstream's, but `name` = `{server}_{tool}`.
    pub tool: Tool,
    /// JSON Schema validator compiled from `inputSchema` (None if the schema is invalid:
    /// the upstream is then left to validate by itself).
    pub validator: Option<Arc<jsonschema::Validator>>,
    /// JSON Schema validator compiled from `outputSchema`; None if the tool declares none or
    /// if it cannot be compiled. Not used to validate `call_tool` responses, which are
    /// relayed as is.
    pub output_validator: Option<Arc<jsonschema::Validator>>,
}

/// Immutable catalog: every exposed tool + the search index.
pub struct ToolCatalog {
    /// Key = exposed name. `BTreeMap` → deterministic order for `tools/list` (recommended by
    /// the 2026-07-28 spec for client-side prompt caching).
    pub tools: BTreeMap<String, ToolEntry>,
    pub index: Box<dyn ToolIndex>,
}

impl ToolCatalog {
    pub fn empty() -> Self {
        Self {
            tools: BTreeMap::new(),
            index: Box::new(Bm25ToolIndex::build([])),
        }
    }
}

/// Builds the exposed name of an upstream tool.
pub fn exposed_name(server: &str, tool: &str) -> String {
    format!("{server}_{tool}")
}

/// Global state. See the module documentation.
pub struct AppState {
    pub config: Arc<Config>,
    /// **All** registry servers, including disabled ones (visible in the catalog).
    pub upstreams: BTreeMap<String, Arc<Upstream>>,
    pub limits: Limits,
    pub catalog: ArcSwap<ToolCatalog>,
    /// Becomes `true` after the first tool load attempt, and goes back to `false` when the
    /// pod starts shutting down (see `/ready`).
    pub ready: AtomicBool,
    /// Serializes catalog rebuilds (two upstreams refreshing at the same time must not
    /// publish a stale version one after the other).
    rebuild_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    pub fn new(config: Config) -> Arc<Self> {
        let threshold = config.gateway.health.failure_threshold;
        let upstreams = config
            .servers
            .iter()
            .map(|s| {
                let upstream = Upstream::new(s.clone(), threshold);
                (s.name.clone(), Arc::new(upstream))
            })
            .collect();
        let c = &config.gateway.concurrency;
        if c.global_limit.is_some() {
            tracing::warn!(
                "gateway.concurrency.global_limit is deprecated and ignored: only the per-server \
                 max_concurrency limits apply"
            );
        }
        Arc::new(Self {
            limits: Limits::new(c.acquire_timeout()),
            upstreams,
            catalog: ArcSwap::from_pointee(ToolCatalog::empty()),
            ready: AtomicBool::new(false),
            rebuild_lock: tokio::sync::Mutex::new(()),
            config: Arc::new(config),
        })
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Enabled servers only.
    pub fn enabled_upstreams(&self) -> impl Iterator<Item = &Arc<Upstream>> {
        self.upstreams.values().filter(|u| u.config.enabled)
    }

    /// Rebuilds the catalog from the upstreams' tool caches, then publishes it.
    pub async fn rebuild_catalog(&self) {
        let _guard = self.rebuild_lock.lock().await;

        // 1. Gather the tools of each enabled upstream (last known list).
        let mut tools: BTreeMap<String, ToolEntry> = BTreeMap::new();
        let mut server_texts: BTreeMap<String, String> = BTreeMap::new();
        for upstream in self.enabled_upstreams() {
            let Some(upstream_tools) = upstream.cached_tools() else {
                continue;
            };
            server_texts.insert(upstream.name().to_owned(), server_text(upstream));
            for tool in upstream_tools.iter() {
                let name = exposed_name(upstream.name(), &tool.name);
                // Cannot happen: server names are unique and contain no '_' (see
                // `Config::validate`), so `{server}_{tool}` is unique. Kept as a safeguard.
                if tools.contains_key(&name) {
                    tracing::error!(tool = %name, server = upstream.name(), "duplicate exposed tool name, ignored");
                    continue;
                }
                tools.insert(name, tool_entry(upstream.name(), tool));
            }
        }

        // 2. Index.
        let index = Bm25ToolIndex::build(tools.iter().map(|(name, entry)| IndexEntry {
            exposed_name: name,
            tool: &entry.tool,
            server_text: &server_texts[&entry.server],
        }));

        // 3. Report, per server, the tools whose declaration is incomplete.
        for q in catalog_quality(tools.values()) {
            tracing::info!(
                server = %q.server,
                tools = q.tools,
                without_output_schema = q.without_output_schema.len(),
                invalid_output_schema = q.invalid_output_schema.len(),
                without_annotations = q.without_annotations.len(),
                without_output_schema_names = %sample_names(&q.without_output_schema),
                invalid_output_schema_names = %sample_names(&q.invalid_output_schema),
                without_annotations_names = %sample_names(&q.without_annotations),
                "catalog quality"
            );
        }

        // 4. Publish atomically.
        tracing::info!(tools = tools.len(), "tool catalog rebuilt");
        metrics::gauge!(metric::CATALOG_TOOLS).set(tools.len() as f64);
        self.catalog.store(Arc::new(ToolCatalog {
            tools,
            index: Box::new(index),
        }));
    }

    /// One refresh cycle for an upstream (see the module documentation).
    pub async fn refresh_upstream(&self, upstream: &Upstream) {
        let outcome = if upstream.cache_expired() {
            match upstream.fetch_tools().await {
                Ok(tools) => {
                    if upstream.store_tools(tools) {
                        self.rebuild_catalog().await;
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else {
            upstream.probe().await
        };
        if let Err(e) = &outcome {
            tracing::warn!(server = upstream.name(), error = %e, "upstream probe failed");
        }
        upstream.record_probe(&outcome);
        let up = matches!(upstream.availability(), Availability::Available);
        metrics::gauge!(metric::UPSTREAM_UP, "server" => upstream.name().to_owned()).set(if up {
            1.0
        } else {
            0.0
        });
    }

    /// Initial load: all upstreams **in parallel**, then report ready.
    ///
    /// This first pass is awaited before `/ready` = 200 so that a new pod (rolling update)
    /// does not receive traffic with an empty catalog. A failing upstream does not block:
    /// the wait is bounded by its `timeout`.
    pub async fn initial_load(&self) {
        let tasks = self.enabled_upstreams().map(|u| self.refresh_upstream(u));
        futures::future::join_all(tasks).await;
        self.ready.store(true, Ordering::Release);
        tracing::info!("gateway ready");
    }
}

/// Starts one refresh task per enabled upstream. They stop when `shutdown` is cancelled.
pub fn spawn_refresh_loops(state: Arc<AppState>, shutdown: CancellationToken) {
    let health = &state.config.gateway.health;
    for upstream in state.enabled_upstreams() {
        // Without health probes, wake up only to refresh the cache.
        let interval = if health.enabled {
            Duration::from_secs(health.probe_interval_seconds)
        } else {
            upstream.config.cache_ttl().max(Duration::from_secs(1))
        };
        let (state, upstream, shutdown) = (state.clone(), upstream.clone(), shutdown.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = shutdown.cancelled() => break,
                }
                state.refresh_upstream(&upstream).await;
            }
        });
    }
}

/// Text describing a server, added to the indexed document of each of its tools.
fn server_text(upstream: &Upstream) -> String {
    let c = &upstream.config;
    let mut parts = vec![c.name.clone()];
    parts.extend(c.domain.clone());
    parts.extend(c.tags.0.iter().cloned());
    parts.extend(c.description.clone());
    parts.join(" ")
}

/// Builds the catalog entry of an upstream tool: exposed name and compiled schemas.
fn tool_entry(server: &str, tool: &Tool) -> ToolEntry {
    let mut exposed = tool.clone();
    exposed.name = exposed_name(server, &tool.name).into();
    ToolEntry {
        server: server.to_owned(),
        upstream_name: tool.name.to_string(),
        validator: compile_schema(&tool.name, SchemaKind::Input, &tool.input_schema),
        output_validator: tool
            .output_schema
            .as_deref()
            .and_then(|schema| compile_schema(&tool.name, SchemaKind::Output, schema)),
        tool: exposed,
    }
}

/// Which schema of a tool is compiled (only changes the log message).
#[derive(Clone, Copy)]
enum SchemaKind {
    Input,
    Output,
}

/// Compiles one of a tool's JSON Schemas, whatever its root (object, array, primitive:
/// SEP-2106). A schema that cannot be compiled is not blocking: log it and keep the tool
/// (for `inputSchema`, the upstream is then left to validate by itself).
///
/// No external `$ref` is fetched: `jsonschema` is built without `resolve-http` and
/// `resolve-file`, so such a reference makes the compilation fail instead.
fn compile_schema(
    tool: &str,
    kind: SchemaKind,
    schema: &JsonObject,
) -> Option<Arc<jsonschema::Validator>> {
    let schema = serde_json::Value::Object(schema.clone());
    match jsonschema::validator_for(&schema) {
        Ok(v) => Some(Arc::new(v)),
        Err(e) => {
            match kind {
                SchemaKind::Input => tracing::warn!(
                    tool,
                    error = %e,
                    "inputSchema cannot be compiled, validation delegated to the upstream"
                ),
                SchemaKind::Output => {
                    tracing::warn!(tool, error = %e, "outputSchema cannot be compiled")
                }
            }
            None
        }
    }
}

/// Quality of a server's tool declarations, logged at each catalog rebuild ("catalog
/// quality"). Names are upstream tool names.
#[derive(Debug, Default, PartialEq, Eq)]
struct ServerQuality {
    server: String,
    /// Number of tools of the server in the catalog.
    tools: usize,
    /// Tools that declare no `outputSchema`.
    without_output_schema: Vec<String>,
    /// Tools whose `outputSchema` is declared but cannot be compiled.
    invalid_output_schema: Vec<String>,
    /// Tools that declare no `annotations`.
    without_annotations: Vec<String>,
}

/// Groups the catalog entries by server and lists, for each one, the tools whose
/// declaration is incomplete. Servers come out sorted by name.
fn catalog_quality<'a>(entries: impl IntoIterator<Item = &'a ToolEntry>) -> Vec<ServerQuality> {
    let mut by_server: BTreeMap<&str, ServerQuality> = BTreeMap::new();
    for entry in entries {
        let q = by_server
            .entry(&entry.server)
            .or_insert_with(|| ServerQuality {
                server: entry.server.clone(),
                ..ServerQuality::default()
            });
        q.tools += 1;
        let name = || entry.upstream_name.clone();
        match (&entry.tool.output_schema, &entry.output_validator) {
            (None, _) => q.without_output_schema.push(name()),
            (Some(_), None) => q.invalid_output_schema.push(name()),
            (Some(_), Some(_)) => {}
        }
        if entry.tool.annotations.is_none() {
            q.without_annotations.push(name());
        }
    }
    by_server.into_values().collect()
}

/// At most this many names per category in the "catalog quality" log line.
const MAX_LOGGED_NAMES: usize = 20;

/// Joins at most [`MAX_LOGGED_NAMES`] names, and says how many were left out.
fn sample_names(names: &[String]) -> String {
    let shown = names[..names.len().min(MAX_LOGGED_NAMES)].join(", ");
    match names.len().saturating_sub(MAX_LOGGED_NAMES) {
        0 => shown,
        more => format!("{shown}, … (+{more})"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// An upstream tool with this input schema, optional output schema and annotations.
    fn tool(name: &str, output_schema: Option<Value>, annotated: bool) -> Tool {
        let mut value = json!({
            "name": name,
            "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}},
        });
        if let Some(schema) = output_schema {
            value["outputSchema"] = schema;
        }
        if annotated {
            value["annotations"] = json!({"readOnlyHint": true});
        }
        serde_json::from_value(value).unwrap()
    }

    fn entry(server: &str, name: &str, output_schema: Option<Value>, annotated: bool) -> ToolEntry {
        tool_entry(server, &tool(name, output_schema, annotated))
    }

    #[test]
    fn valid_output_schema_is_compiled() {
        let schema = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
        let e = entry("srv", "t", Some(schema), false);
        let validator = e.output_validator.expect("compiled");
        assert!(validator.is_valid(&json!({"n": 1})));
        assert!(!validator.is_valid(&json!({"n": "one"})));
        assert_eq!(e.tool.name, "srv_t");
        assert!(e.validator.is_some());
    }

    #[test]
    fn missing_output_schema_gives_no_validator() {
        let e = entry("srv", "t", None, false);
        assert!(e.output_validator.is_none());
        assert!(e.tool.output_schema.is_none());
    }

    #[test]
    fn invalid_output_schema_keeps_the_tool_and_its_declaration() {
        let e = entry("srv", "t", Some(json!({"type": 12})), false);
        assert!(e.output_validator.is_none());
        // The declaration is kept as is (it is still returned by search_tools).
        assert_eq!(e.tool.output_schema.as_deref().unwrap()["type"], 12);
        assert!(e.validator.is_some());
    }

    #[test]
    fn pydantic_output_schema_with_local_refs_is_compiled() {
        let schema = json!({
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": {"id": {"type": "string"}},
                    "required": ["id"],
                },
            },
            "type": "object",
            "properties": {"results": {"type": "array", "items": {"$ref": "#/$defs/Item"}}},
            "required": ["results"],
        });
        let validator = entry("srv", "t", Some(schema), false)
            .output_validator
            .expect("compiled");
        assert!(validator.is_valid(&json!({"results": [{"id": "a"}]})));
        assert!(!validator.is_valid(&json!({"results": [{}]})));
    }

    #[test]
    fn non_object_root_output_schemas_are_compiled() {
        // SEP-2106: the root of an outputSchema can be an array, a primitive…
        let array = json!({"type": "array", "items": {"type": "string"}});
        let validator = entry("srv", "t", Some(array), false)
            .output_validator
            .expect("array root");
        assert!(validator.is_valid(&json!(["a", "b"])));
        assert!(!validator.is_valid(&json!({"a": 1})));

        let integer = json!({"type": "integer"});
        let validator = entry("srv", "t", Some(integer), false)
            .output_validator
            .expect("integer root");
        assert!(validator.is_valid(&json!(3)));
        assert!(!validator.is_valid(&json!("3")));
    }

    #[test]
    fn remote_refs_are_not_fetched() {
        // Built without `resolve-http` / `resolve-file`: compilation fails without any
        // network or file access.
        for (uri, feature) in [
            ("https://example.invalid/schema.json", "resolve-http"),
            ("file:///etc/passwd", "resolve-file"),
        ] {
            let schema = json!({"$ref": uri});
            let e = entry("srv", "t", Some(schema.clone()), false);
            assert!(e.output_validator.is_none(), "{uri}");
            // The failure comes from the disabled feature, not from a network/file error.
            let error = jsonschema::validator_for(&schema).unwrap_err().to_string();
            assert!(error.contains(feature), "{uri}: {error}");
        }
    }

    #[test]
    fn catalog_quality_counts_incomplete_declarations_per_server() {
        let ok = json!({"type": "object"});
        let entries = [
            entry("b", "full", Some(ok.clone()), true),
            entry("b", "bare", None, false),
            entry("a", "broken", Some(json!({"type": 12})), true),
            entry("a", "plain", Some(ok), false),
            entry("a", "none", None, true),
        ];
        let quality = catalog_quality(&entries);
        assert_eq!(
            quality,
            vec![
                ServerQuality {
                    server: "a".into(),
                    tools: 3,
                    without_output_schema: vec!["none".into()],
                    invalid_output_schema: vec!["broken".into()],
                    without_annotations: vec!["plain".into()],
                },
                ServerQuality {
                    server: "b".into(),
                    tools: 2,
                    without_output_schema: vec!["bare".into()],
                    invalid_output_schema: vec![],
                    without_annotations: vec!["bare".into()],
                },
            ]
        );
    }

    #[test]
    fn logged_names_are_truncated() {
        let names: Vec<String> = (0..23).map(|i| format!("t{i}")).collect();
        assert_eq!(sample_names(&names[..2]), "t0, t1");
        assert_eq!(sample_names(&[]), "");
        let sample = sample_names(&names);
        assert!(sample.starts_with("t0, t1,"), "{sample}");
        assert!(sample.contains("t19, … (+3)"), "{sample}");
        assert!(!sample.contains("t20"), "{sample}");
    }
}
