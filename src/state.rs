//! # État partagé de la passerelle
//!
//! [`AppState`] est créé une fois au démarrage et partagé (via `Arc`) par toutes les requêtes.
//! Il contient deux sortes de données :
//!
//! 1. des données **fixes** pour la vie du pod : la config, les [`Upstream`], les limites ;
//! 2. un **catalogue d'outils** ([`ToolCatalog`]) qui change quand un amont publie de
//!    nouveaux outils. Il est rangé derrière un [`ArcSwap`].
//!
//! ## Pourquoi `ArcSwap` ?
//!
//! Le catalogue est lu à **chaque** requête et réécrit **rarement** (rafraîchissement toutes
//! les `cache_ttl` secondes). `ArcSwap` est fait pour ça :
//!
//! - lecture : `catalog.load()` renvoie un pointeur vers la version courante, sans verrou
//!   (quelques nanosecondes) ;
//! - écriture : on construit un **nouveau** catalogue complet à côté, puis `store()` le
//!   publie atomiquement. Les requêtes en cours gardent l'ancienne version jusqu'à leur fin.
//!
//! Aucune requête ne voit donc jamais un catalogue « à moitié reconstruit ».
//!
//! ## Boucle de rafraîchissement
//!
//! Une tâche par amont activé ([`spawn_refresh_loops`]) se réveille toutes les
//! `probe_interval_seconds` :
//!
//! - si le cache d'outils a dépassé `cache_ttl` → `tools/list` complet, et reconstruction du
//!   catalogue si la liste a changé ;
//! - sinon → sonde légère (première page de `tools/list`).
//!
//! Dans les deux cas le résultat met à jour la santé de l'amont.

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

/// Un outil tel qu'exposé par la passerelle.
pub struct ToolEntry {
    /// Nom du serveur amont (= préfixe).
    pub server: String,
    /// Nom de l'outil **chez l'amont** (sans préfixe), utilisé pour l'appel amont.
    pub upstream_name: String,
    /// Définition exposée : identique à l'amont, mais `name` = `{serveur}_{outil}`.
    pub tool: Tool,
    /// Validateur JSON Schema compilé à partir de `inputSchema` (None si le schéma est
    /// invalide : on laisse alors l'amont valider lui-même).
    pub validator: Option<Arc<jsonschema::Validator>>,
}

/// Catalogue immuable : tous les outils exposés + l'index de recherche.
pub struct ToolCatalog {
    /// Clé = nom exposé. `BTreeMap` → ordre déterministe pour `tools/list` (recommandé par la
    /// spec 2026-07-28 pour le cache des prompts côté client).
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

/// Construit le nom exposé d'un outil amont.
pub fn exposed_name(server: &str, tool: &str) -> String {
    format!("{server}_{tool}")
}

/// État global. Voir la documentation du module.
pub struct AppState {
    pub config: Arc<Config>,
    /// **Tous** les serveurs du registre, y compris désactivés (visibles dans le catalogue).
    pub upstreams: BTreeMap<String, Arc<Upstream>>,
    pub limits: Limits,
    pub catalog: ArcSwap<ToolCatalog>,
    /// Passe à `true` après la première tentative de chargement des outils, et repasse à
    /// `false` au début de l'arrêt du pod (voir `/ready`).
    pub ready: AtomicBool,
    /// Sérialise les reconstructions du catalogue (deux amonts qui se rafraîchissent en même
    /// temps ne doivent pas publier une version périmée l'un après l'autre).
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
        Arc::new(Self {
            limits: Limits::new(c.global_limit, c.acquire_timeout()),
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

    /// Serveurs activés uniquement.
    pub fn enabled_upstreams(&self) -> impl Iterator<Item = &Arc<Upstream>> {
        self.upstreams.values().filter(|u| u.config.enabled)
    }

    /// Reconstruit le catalogue à partir des caches d'outils des amonts, puis le publie.
    pub async fn rebuild_catalog(&self) {
        let _guard = self.rebuild_lock.lock().await;

        // 1. Rassembler les outils de chaque amont activé (dernière liste connue).
        let mut tools: BTreeMap<String, ToolEntry> = BTreeMap::new();
        let mut server_texts: BTreeMap<String, String> = BTreeMap::new();
        for upstream in self.enabled_upstreams() {
            let Some(upstream_tools) = upstream.cached_tools() else {
                continue;
            };
            server_texts.insert(upstream.name().to_owned(), server_text(upstream));
            for tool in upstream_tools.iter() {
                let name = exposed_name(upstream.name(), &tool.name);
                // Collision possible : serveur "a_b" + outil "c" et serveur "a" + outil "b_c".
                if tools.contains_key(&name) {
                    tracing::warn!(tool = %name, server = upstream.name(), "nom d'outil exposé en double, ignoré");
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

        // 2. Indexer.
        let index = Bm25ToolIndex::build(tools.iter().map(|(name, entry)| IndexEntry {
            exposed_name: name,
            tool: &entry.tool,
            server_text: &server_texts[&entry.server],
        }));

        // 3. Publier atomiquement.
        tracing::info!(tools = tools.len(), "catalogue d'outils reconstruit");
        metrics::gauge!(metric::CATALOG_TOOLS).set(tools.len() as f64);
        self.catalog.store(Arc::new(ToolCatalog {
            tools,
            index: Box::new(index),
        }));
    }

    /// Un cycle de rafraîchissement pour un amont (voir la documentation du module).
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
            tracing::warn!(server = upstream.name(), error = %e, "sonde amont en échec");
        }
        upstream.record_probe(&outcome);
        let up = matches!(upstream.availability(), Availability::Available);
        metrics::gauge!(metric::UPSTREAM_UP, "server" => upstream.name().to_owned()).set(if up {
            1.0
        } else {
            0.0
        });
    }

    /// Premier chargement : tous les amonts **en parallèle**, puis on se déclare prêt.
    ///
    /// On attend ce premier passage avant `/ready` = 200 pour qu'un nouveau pod (rolling
    /// update) ne reçoive pas de trafic avec un catalogue vide. Un amont en échec ne bloque pas :
    /// le délai est borné par son `timeout`.
    pub async fn initial_load(&self) {
        let tasks = self.enabled_upstreams().map(|u| self.refresh_upstream(u));
        futures::future::join_all(tasks).await;
        self.ready.store(true, Ordering::Release);
        tracing::info!("passerelle prête");
    }
}

/// Lance une tâche de rafraîchissement par amont activé. Elles s'arrêtent quand `shutdown`
/// est annulé.
pub fn spawn_refresh_loops(state: Arc<AppState>, shutdown: CancellationToken) {
    let health = &state.config.gateway.health;
    for upstream in state.enabled_upstreams() {
        // Sans sondes de santé, on se réveille seulement pour rafraîchir le cache.
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

/// Texte décrivant un serveur, ajouté au document indexé de chacun de ses outils.
fn server_text(upstream: &Upstream) -> String {
    let c = &upstream.config;
    let mut parts = vec![c.name.clone()];
    parts.extend(c.domain.clone());
    parts.extend(c.tags.0.iter().cloned());
    parts.extend(c.description.clone());
    parts.join(" ")
}

/// Compile le JSON Schema d'entrée d'un outil. Un schéma que l'on ne sait pas compiler n'est
/// pas bloquant : on log et on laisse l'amont valider.
fn compile_validator(tool: &Tool) -> Option<Arc<jsonschema::Validator>> {
    let schema = serde_json::Value::Object((*tool.input_schema).clone());
    match jsonschema::validator_for(&schema) {
        Ok(v) => Some(Arc::new(v)),
        Err(e) => {
            tracing::warn!(tool = %tool.name, error = %e, "inputSchema non compilable, validation déléguée à l'amont");
            None
        }
    }
}
