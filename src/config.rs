//! # Configuration: the YAML registry
//!
//! This module contains **only** data types (deserialized with `serde`) and their
//! validation. It makes no network call, so it can be fully unit-tested.
//!
//! Example registry:
//!
//! ```yaml
//! gateway:
//!   name: SSP Gateway
//!   tool_search: {enabled: true, max_results: 5}
//!   catalog: {enabled: true, include_tool_inventory: true, include_guide: true, guide_path: guide-ssp.md}
//!   health: {enabled: true, path: /health, probe_interval_seconds: 60, failure_threshold: 2}
//!   metrics: {log_level: INFO, json_logs: true, prometheus: {enabled: true, port: 9090}}
//!   concurrency: {global_limit: 16, acquire_timeout_seconds: 5}
//!
//! servers:
//!   - name: mcpdiffusion
//!     url: https://mcpdiffusion.lab.sspcloud.fr/mcp
//!     transport: http
//!     domain: diffusion
//!     description: Server exposing Melodi, Rmes and insee.fr
//!     tags: [dataset, publications]
//!     enabled: true
//!     timeout: 30.0
//!     max_concurrency: 4
//!     cache_ttl: 300
//! ```
//!
//! Every field has a sensible default value (`default_*` functions), which makes it possible
//! to write a minimal registry (`gateway: {name: X}` + a list of servers).

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

/// Possible errors when loading the registry.
///
/// `thiserror` gives readable messages in the startup logs: an invalid config must prevent
/// the pod from starting, with a clear message.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read the registry {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid YAML registry: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("invalid registry: {0}")]
    Invalid(String),
}

// ---------------------------------------------------------------------------------------------
// Registry root
// ---------------------------------------------------------------------------------------------

/// Root of the `registry.yaml` file.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Settings of the gateway itself.
    pub gateway: GatewayConfig,
    /// List of upstream MCP servers to aggregate.
    #[serde(default)]
    pub servers: Vec<UpstreamConfig>,
}

impl Config {
    /// Reads, parses and validates the registry.
    ///
    /// `guide_path` is relative **to the registry file** (not to the current directory): it
    /// is resolved here so that the rest of the code no longer has to think about it.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let mut config = Self::from_yaml(&text)?;

        // Resolve guide_path relative to the registry's directory.
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        if let Some(guide) = &config.gateway.catalog.guide_path
            && guide.is_relative()
        {
            config.gateway.catalog.guide_path = Some(base_dir.join(guide));
        }
        config.check_guide_file()?;
        Ok(config)
    }

    /// Parses and validates a registry from a YAML string (without touching the disk).
    /// Handy for unit tests.
    pub fn from_yaml(text: &str) -> Result<Self, ConfigError> {
        let config: Config = serde_yaml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Enabled servers (`enabled: true`), in registry order.
    pub fn enabled_servers(&self) -> impl Iterator<Item = &UpstreamConfig> {
        self.servers.iter().filter(|s| s.enabled)
    }

    /// Consistency rules that cannot be expressed with types alone.
    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |msg: String| Err(ConfigError::Invalid(msg));

        let g = &self.gateway;
        if g.concurrency.global_limit == 0 {
            return invalid("gateway.concurrency.global_limit must be >= 1".into());
        }
        if g.tool_search.max_results == 0 {
            return invalid("gateway.tool_search.max_results must be >= 1".into());
        }
        if g.health.probe_interval_seconds == 0 || g.health.failure_threshold == 0 {
            return invalid(
                "gateway.health.probe_interval_seconds and failure_threshold must be >= 1".into(),
            );
        }
        if !g.http.path.starts_with('/') {
            return invalid(format!(
                "gateway.http.path must start with '/': {}",
                g.http.path
            ));
        }

        let mut seen = HashSet::new();
        for s in &self.servers {
            // The name is used as the tool prefix ("{server}_{tool}"): restrict it to characters
            // that are safe in an MCP tool name and in a catalog://servers/{name} URI.
            let valid_name = !s.name.is_empty()
                && s.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !valid_name {
                return invalid(format!(
                    "invalid server name '{}': only [A-Za-z0-9_-]",
                    s.name
                ));
            }
            if !seen.insert(s.name.as_str()) {
                return invalid(format!("duplicate server name: '{}'", s.name));
            }
            if !(s.url.starts_with("http://") || s.url.starts_with("https://")) {
                return invalid(format!("server '{}': url must be http(s)", s.name));
            }
            if !(s.timeout > 0.0 && s.timeout.is_finite()) {
                return invalid(format!("server '{}': timeout must be > 0", s.name));
            }
            if s.max_concurrency == 0 {
                return invalid(format!("server '{}': max_concurrency must be >= 1", s.name));
            }
        }
        Ok(())
    }

    /// `guide_path` is checked at startup: better a pod that does not start than a broken
    /// `catalog://guide` resource discovered in production.
    fn check_guide_file(&self) -> Result<(), ConfigError> {
        let catalog = &self.gateway.catalog;
        if catalog.enabled
            && catalog.include_guide
            && let Some(path) = &catalog.guide_path
            && !path.is_file()
        {
            return Err(ConfigError::Invalid(format!(
                "gateway.catalog.guide_path not found: {}",
                path.display()
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Section `gateway`
// ---------------------------------------------------------------------------------------------

/// `gateway` section: global settings of the gateway.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Name shown to MCP clients (serverInfo.name).
    pub name: String,
    #[serde(default)]
    pub tool_search: ToolSearchConfig,
    #[serde(default)]
    pub catalog: CatalogConfig,
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub concurrency: ConcurrencyConfig,
    #[serde(default)]
    pub http: HttpConfig,
}

/// Tool search.
///
/// - `enabled: true`  → `tools/list` only exposes `search_tools`, `call_tool` and
///   `send_feedback`; the upstream tools are "hidden" behind search (saves context on the
///   client side).
/// - `enabled: false` → `tools/list` directly exposes every prefixed upstream tool.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSearchConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Maximum number of results returned by `search_tools`.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

impl Default for ToolSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_results: default_max_results(),
        }
    }
}

/// `catalog://…` resources.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Expose `catalog://servers/{name}/tools`.
    #[serde(default = "yes")]
    pub include_tool_inventory: bool,
    /// Expose `catalog://guide`.
    #[serde(default = "yes")]
    pub include_guide: bool,
    /// Deployment-specific guide, appended after the generic guide. Relative to the registry.
    #[serde(default)]
    pub guide_path: Option<PathBuf>,
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            include_tool_inventory: true,
            include_guide: true,
            guide_path: None,
        }
    }
}

/// Upstream health probes and `/health` endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// HTTP path of the liveness endpoint.
    #[serde(default = "default_health_path")]
    pub path: String,
    /// Interval between two probes of the same upstream.
    #[serde(default = "default_probe_interval")]
    pub probe_interval_seconds: u64,
    /// Number of consecutive failures before an upstream is declared "unavailable".
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: default_health_path(),
            probe_interval_seconds: default_probe_interval(),
            failure_threshold: default_failure_threshold(),
        }
    }
}

/// Logs and metrics.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    /// Log level (TRACE, DEBUG, INFO, WARN, ERROR). The `RUST_LOG` variable takes precedence.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// JSON logs (one line = one object) for Loki/Grafana; otherwise human-readable text.
    #[serde(default = "yes")]
    pub json_logs: bool,
    #[serde(default)]
    pub prometheus: PrometheusConfig,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
            json_logs: true,
            prometheus: PrometheusConfig::default(),
        }
    }
}

/// Prometheus export: `/metrics` on a port separate from the MCP port (not publicly exposed).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrometheusConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_prometheus_port")]
    pub port: u16,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: default_prometheus_port(),
        }
    }
}

/// Global concurrency limit (V1: no per-user limit).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyConfig {
    /// Maximum number of concurrent upstream calls, across all servers.
    #[serde(default = "default_global_limit")]
    pub global_limit: usize,
    /// Maximum wait for a permit before answering "saturated".
    #[serde(default = "default_acquire_timeout")]
    pub acquire_timeout_seconds: f64,
}

impl ConcurrencyConfig {
    pub fn acquire_timeout(&self) -> Duration {
        Duration::from_secs_f64(self.acquire_timeout_seconds.max(0.0))
    }
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            global_limit: default_global_limit(),
            acquire_timeout_seconds: default_acquire_timeout(),
        }
    }
}

/// HTTP exposure of the MCP server.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// Listening address of the MCP port (and of /health, /ready).
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Path of the MCP endpoint (Streamable HTTP).
    #[serde(default = "default_mcp_path")]
    pub path: String,
    /// Allowed values for the `Host` header (DNS rebinding protection).
    /// Empty = accept everything, which suits running behind a Kubernetes ingress.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            path: default_mcp_path(),
            allowed_hosts: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Section `servers`
// ---------------------------------------------------------------------------------------------

/// An upstream MCP server.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamConfig {
    /// Unique name; also used as the **prefix** of the exposed tools (`{name}_{tool}`).
    pub name: String,
    /// URL of the upstream MCP endpoint.
    pub url: String,
    #[serde(default)]
    pub transport: Transport,
    /// Functional grouping (used by `catalog://domains`).
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// List of tags, or a single string (`tags: dataset`).
    #[serde(default)]
    pub tags: Tags,
    /// `false`: kept in the registry (visible in the catalog) but neither exposed nor contacted.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Maximum duration of an upstream call, in seconds.
    #[serde(default = "default_timeout")]
    pub timeout: f64,
    /// Maximum number of concurrent calls to this server.
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    /// Validity of the tool list cache, in seconds.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl: u64,
}

impl UpstreamConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs_f64(self.timeout)
    }
    pub fn cache_ttl(&self) -> Duration {
        Duration::from_secs(self.cache_ttl)
    }
}

/// Transport to the upstream.
///
/// `sse` (the old HTTP+SSE transport) is **deprecated** in the MCP spec and no longer
/// implemented by the rmcp 3.x client: it is accepted in the registry so as not to break an
/// existing config, but the server is then reported "unavailable" with a clear message.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Http,
    Sse,
}

/// Tags of a server: the YAML accepts a list **or** a single string.
///
/// `#[serde(untagged)]` tries each variant in order; the result is then normalized to a `Vec`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Tags(pub Vec<String>);

impl<'de> Deserialize<'de> for Tags {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        Ok(match OneOrMany::deserialize(d)? {
            OneOrMany::One(s) => Tags(vec![s]),
            OneOrMany::Many(v) => Tags(v),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Default values (serde needs functions, not constants)
// ---------------------------------------------------------------------------------------------

fn yes() -> bool {
    true
}
fn default_max_results() -> usize {
    5
}
fn default_health_path() -> String {
    "/health".into()
}
fn default_probe_interval() -> u64 {
    60
}
fn default_failure_threshold() -> u32 {
    2
}
fn default_log_level() -> String {
    "INFO".into()
}
fn default_prometheus_port() -> u16 {
    9090
}
fn default_global_limit() -> usize {
    16
}
fn default_acquire_timeout() -> f64 {
    5.0
}
fn default_bind() -> String {
    "0.0.0.0:8000".into()
}
fn default_mcp_path() -> String {
    "/mcp".into()
}
fn default_timeout() -> f64 {
    30.0
}
fn default_max_concurrency() -> usize {
    4
}
fn default_cache_ttl() -> u64 {
    300
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
gateway: {name: Test}
servers:
  - name: a
    url: http://localhost:1/mcp
"#;

    #[test]
    fn minimal_registry_uses_defaults() {
        let c = Config::from_yaml(MINIMAL).unwrap();
        assert_eq!(c.gateway.tool_search.max_results, 5);
        assert!(c.gateway.metrics.prometheus.enabled);
        assert_eq!(c.gateway.metrics.prometheus.port, 9090);
        let s = &c.servers[0];
        assert!(s.enabled);
        assert_eq!(s.transport, Transport::Http);
        assert_eq!(s.max_concurrency, 4);
        assert_eq!(s.timeout(), Duration::from_secs(30));
    }

    #[test]
    fn full_example_from_plan_parses() {
        let yaml = r#"
gateway:
  name: SSP Gateway
  tool_search: {enabled: true, max_results: 5}
  catalog: {enabled: true, include_tool_inventory: true, include_guide: true, guide_path: guide-ssp.md}
  health: {enabled: true, path: /health, probe_interval_seconds: 60, failure_threshold: 2}
  metrics: {log_level: INFO, json_logs: true, prometheus: {enabled: true, port: 9090}}
  concurrency: {global_limit: 16, acquire_timeout_seconds: 5}
servers:
  - name: mcpdiffusion
    url: https://mcpdiffusion.lab.sspcloud.fr/mcp
    transport: http
    domain: diffusion
    description: Server exposing Melodi, Rmes and insee.fr
    tags: [dataset, publications]
    enabled: true
    timeout: 30.0
    max_concurrency: 4
    cache_ttl: 300
"#;
        let c = Config::from_yaml(yaml).unwrap();
        assert_eq!(c.servers[0].tags.0, vec!["dataset", "publications"]);
        assert_eq!(c.servers[0].domain.as_deref(), Some("diffusion"));
    }

    #[test]
    fn tags_accept_single_string() {
        let yaml = MINIMAL.replace("url:", "tags: dataset\n    url:");
        let c = Config::from_yaml(&yaml).unwrap();
        assert_eq!(c.servers[0].tags.0, vec!["dataset"]);
    }

    #[test]
    fn rejects_duplicate_names() {
        let yaml = r#"
gateway: {name: T}
servers:
  - {name: a, url: http://x/mcp}
  - {name: a, url: http://y/mcp}
"#;
        let err = Config::from_yaml(yaml).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn rejects_invalid_prefix_characters() {
        let yaml = "gateway: {name: T}\nservers:\n  - {name: 'a b', url: http://x/mcp}\n";
        assert!(Config::from_yaml(yaml).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        // deny_unknown_fields: a typo in the registry must be visible.
        let yaml = "gateway: {name: T, tool_serch: {enabled: true}}\n";
        assert!(Config::from_yaml(yaml).is_err());
    }

    #[test]
    fn rejects_zero_concurrency() {
        let yaml =
            "gateway: {name: T}\nservers:\n  - {name: a, url: http://x/mcp, max_concurrency: 0}\n";
        assert!(Config::from_yaml(yaml).is_err());
    }

    #[test]
    fn guide_path_is_relative_to_registry_and_checked() {
        let dir = std::env::temp_dir().join(format!("mcportal-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = dir.join("registry.yaml");
        std::fs::write(
            &registry,
            "gateway: {name: T, catalog: {guide_path: guide.md}}\n",
        )
        .unwrap();

        // Missing file → error on load.
        assert!(Config::load(&registry).is_err());

        // File present → path resolved next to the registry.
        std::fs::write(dir.join("guide.md"), "# guide").unwrap();
        let c = Config::load(&registry).unwrap();
        assert_eq!(c.gateway.catalog.guide_path, Some(dir.join("guide.md")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
