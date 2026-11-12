//! # Configuration : le registre YAML
//!
//! Ce module contient **uniquement** des types de données (désérialisés avec `serde`) et leur
//! validation. Il ne fait aucun appel réseau : on peut donc le tester entièrement en unitaire.
//!
//! Exemple de registre :
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
//!     description: Serveur pour exposer Melodi, Rmes et insee.fr
//!     tags: [dataset, publications]
//!     enabled: true
//!     timeout: 30.0
//!     max_concurrency: 4
//!     cache_ttl: 300
//! ```
//!
//! Chaque champ a une valeur par défaut raisonnable (fonctions `default_*`), ce qui permet
//! d'écrire un registre minimal (`gateway: {name: X}` + une liste de serveurs).

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

/// Erreurs possibles au chargement du registre.
///
/// On utilise `thiserror` pour avoir des messages lisibles dans les logs de démarrage :
/// une config invalide doit empêcher le pod de démarrer avec un message clair.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("impossible de lire le registre {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("registre YAML invalide: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("registre invalide: {0}")]
    Invalid(String),
}

// ---------------------------------------------------------------------------------------------
// Racine du registre
// ---------------------------------------------------------------------------------------------

/// Racine du fichier `registry.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Réglages de la passerelle elle-même.
    pub gateway: GatewayConfig,
    /// Liste des serveurs MCP amont à agréger.
    #[serde(default)]
    pub servers: Vec<UpstreamConfig>,
}

impl Config {
    /// Lit, parse et valide le registre.
    ///
    /// `guide_path` est relatif **au fichier de registre** (et non au répertoire courant) :
    /// on le résout ici en chemin absolu pour que le reste du code n'ait plus à y penser.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let mut config = Self::from_yaml(&text)?;

        // Résolution de guide_path relativement au dossier du registre.
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        if let Some(guide) = &config.gateway.catalog.guide_path
            && guide.is_relative()
        {
            config.gateway.catalog.guide_path = Some(base_dir.join(guide));
        }
        config.check_guide_file()?;
        Ok(config)
    }

    /// Parse et valide un registre depuis une chaîne YAML (sans toucher au disque).
    /// Pratique pour les tests unitaires.
    pub fn from_yaml(text: &str) -> Result<Self, ConfigError> {
        let config: Config = serde_yaml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Serveurs activés (`enabled: true`), dans l'ordre du registre.
    pub fn enabled_servers(&self) -> impl Iterator<Item = &UpstreamConfig> {
        self.servers.iter().filter(|s| s.enabled)
    }

    /// Règles de cohérence qui ne s'expriment pas avec les types seuls.
    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |msg: String| Err(ConfigError::Invalid(msg));

        let g = &self.gateway;
        if g.concurrency.global_limit == 0 {
            return invalid("gateway.concurrency.global_limit doit être >= 1".into());
        }
        if g.tool_search.max_results == 0 {
            return invalid("gateway.tool_search.max_results doit être >= 1".into());
        }
        if g.health.probe_interval_seconds == 0 || g.health.failure_threshold == 0 {
            return invalid(
                "gateway.health.probe_interval_seconds et failure_threshold doivent être >= 1"
                    .into(),
            );
        }
        if !g.http.path.starts_with('/') {
            return invalid(format!(
                "gateway.http.path doit commencer par '/': {}",
                g.http.path
            ));
        }

        let mut seen = HashSet::new();
        for s in &self.servers {
            // Le nom sert de préfixe d'outil ("{serveur}_{outil}") : on le restreint à des
            // caractères sûrs pour un nom d'outil MCP et pour une URI catalog://servers/{nom}.
            let valid_name = !s.name.is_empty()
                && s.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !valid_name {
                return invalid(format!(
                    "nom de serveur invalide '{}': uniquement [A-Za-z0-9_-]",
                    s.name
                ));
            }
            if !seen.insert(s.name.as_str()) {
                return invalid(format!("nom de serveur en double: '{}'", s.name));
            }
            if !(s.url.starts_with("http://") || s.url.starts_with("https://")) {
                return invalid(format!("serveur '{}': url doit être http(s)", s.name));
            }
            if !(s.timeout > 0.0 && s.timeout.is_finite()) {
                return invalid(format!("serveur '{}': timeout doit être > 0", s.name));
            }
            if s.max_concurrency == 0 {
                return invalid(format!(
                    "serveur '{}': max_concurrency doit être >= 1",
                    s.name
                ));
            }
        }
        Ok(())
    }

    /// `guide_path` est vérifié au démarrage : mieux vaut un pod qui ne démarre pas qu'une
    /// ressource `catalog://guide` cassée découverte en production.
    fn check_guide_file(&self) -> Result<(), ConfigError> {
        let catalog = &self.gateway.catalog;
        if catalog.enabled
            && catalog.include_guide
            && let Some(path) = &catalog.guide_path
            && !path.is_file()
        {
            return Err(ConfigError::Invalid(format!(
                "gateway.catalog.guide_path introuvable: {}",
                path.display()
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Section `gateway`
// ---------------------------------------------------------------------------------------------

/// Section `gateway` : réglages globaux de la passerelle.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Nom affiché aux clients MCP (serverInfo.name).
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

/// Recherche d'outils.
///
/// - `enabled: true`  → `tools/list` n'expose que `search_tools`, `call_tool` et
///   `send_feedback` ; les outils amont sont « masqués » derrière la recherche
///   (économie de contexte côté client).
/// - `enabled: false` → `tools/list` expose directement tous les outils amont préfixés.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSearchConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Nombre maximal de résultats renvoyés par `search_tools`.
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

/// Ressources `catalog://…`.
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
    /// Guide propre au déploiement, ajouté après le guide générique. Relatif au registre.
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

/// Sondes de santé des amonts et endpoint `/health`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Chemin HTTP de l'endpoint de liveness.
    #[serde(default = "default_health_path")]
    pub path: String,
    /// Intervalle entre deux sondes d'un même amont.
    #[serde(default = "default_probe_interval")]
    pub probe_interval_seconds: u64,
    /// Nombre d'échecs consécutifs avant de déclarer un amont « unavailable ».
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

/// Logs et métriques.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    /// Niveau de log (TRACE, DEBUG, INFO, WARN, ERROR). La variable `RUST_LOG` est prioritaire.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Logs en JSON (une ligne = un objet) pour Loki/Grafana ; sinon format texte lisible.
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

/// Export Prometheus : `/metrics` sur un port séparé du port MCP (non exposé publiquement).
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

/// Limite de concurrence globale (V1 : pas de limite par utilisateur).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyConfig {
    /// Nombre maximal d'appels amont simultanés, tous serveurs confondus.
    #[serde(default = "default_global_limit")]
    pub global_limit: usize,
    /// Temps d'attente maximal pour obtenir une place avant de répondre « saturé ».
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

/// Exposition HTTP du serveur MCP.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// Adresse d'écoute du port MCP (et de /health, /ready).
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Chemin de l'endpoint MCP (Streamable HTTP).
    #[serde(default = "default_mcp_path")]
    pub path: String,
    /// Valeurs autorisées pour l'en-tête `Host` (protection DNS rebinding).
    /// Vide = tout accepter, ce qui convient derrière un ingress Kubernetes.
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

/// Un serveur MCP amont.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamConfig {
    /// Nom unique ; sert aussi de **préfixe** des outils exposés (`{name}_{outil}`).
    pub name: String,
    /// URL de l'endpoint MCP amont.
    pub url: String,
    #[serde(default)]
    pub transport: Transport,
    /// Regroupement fonctionnel (utilisé par `catalog://domains`).
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Liste de tags, ou une chaîne unique (`tags: dataset`).
    #[serde(default)]
    pub tags: Tags,
    /// `false` : gardé au registre (visible dans le catalogue) mais non exposé ni contacté.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Délai maximal d'un appel amont, en secondes.
    #[serde(default = "default_timeout")]
    pub timeout: f64,
    /// Nombre maximal d'appels simultanés vers ce serveur.
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    /// Durée de validité du cache de la liste des outils, en secondes.
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

/// Transport vers l'amont.
///
/// `sse` (l'ancien transport HTTP+SSE) est **déprécié** dans la spec MCP et n'est plus
/// implémenté par rmcp 3.x côté client : on l'accepte dans le registre pour ne pas casser une
/// config existante, mais le serveur est alors signalé « unavailable » avec un message clair.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Http,
    Sse,
}

/// Tags d'un serveur : le YAML accepte une liste **ou** une chaîne unique.
///
/// `#[serde(untagged)]` essaie chaque variante dans l'ordre ; on normalise ensuite en `Vec`.
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
// Valeurs par défaut (serde a besoin de fonctions, pas de constantes)
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
    description: Serveur pour exposer Melodi, Rmes et insee.fr
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
        assert!(err.contains("double"), "{err}");
    }

    #[test]
    fn rejects_invalid_prefix_characters() {
        let yaml = "gateway: {name: T}\nservers:\n  - {name: 'a b', url: http://x/mcp}\n";
        assert!(Config::from_yaml(yaml).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        // deny_unknown_fields : une faute de frappe dans le registre doit être visible.
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

        // Fichier absent → erreur au chargement.
        assert!(Config::load(&registry).is_err());

        // Fichier présent → chemin résolu à côté du registre.
        std::fs::write(dir.join("guide.md"), "# guide").unwrap();
        let c = Config::load(&registry).unwrap();
        assert_eq!(c.gateway.catalog.guide_path, Some(dir.join("guide.md")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
