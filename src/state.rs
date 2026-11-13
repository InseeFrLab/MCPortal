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
use rmcp::model::Tool;
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
                // Possible collision: server "a_b" + tool "c" and server "a" + tool "b_c".
                if tools.contains_key(&name) {
                    tracing::warn!(tool = %name, server = upstream.name(), "duplicate exposed tool name, ignored");
                    continue;
                }
                let mut exposed = tool.clone();
                exposed.name = name.clone().into();
                tools.insert(
                    name,
                    ToolEntry {
                        server: upstream.name().to_owned(),
                        upstream_name: tool.name.to_string(),
                        validator: compile_validator(tool),
                        tool: exposed,
                    },
                );
            }
        }

        // 2. Index.
        let index = Bm25ToolIndex::build(tools.iter().map(|(name, entry)| IndexEntry {
            exposed_name: name,
            tool: &entry.tool,
            server_text: &server_texts[&entry.server],
        }));

        // 3. Publish atomically.
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

/// Compiles a tool's input JSON Schema. A schema that cannot be compiled is not blocking:
/// log it and let the upstream validate.
fn compile_validator(tool: &Tool) -> Option<Arc<jsonschema::Validator>> {
    let schema = serde_json::Value::Object((*tool.input_schema).clone());
    match jsonschema::validator_for(&schema) {
        Ok(v) => Some(Arc::new(v)),
        Err(e) => {
            tracing::warn!(tool = %tool.name, error = %e, "inputSchema cannot be compiled, validation delegated to the upstream");
            None
        }
    }
}
