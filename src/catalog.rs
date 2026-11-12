//! # Le catalogue (ressources `catalog://…`)
//!
//! Le catalogue est exposé en **ressources** MCP et non en outils : il ne coûte rien au
//! contexte du client tant que celui-ci ne le lit pas, et il reste lisible quand la recherche
//! d'outils masque `tools/list`.
//!
//! | URI | Contenu |
//! | --- | --- |
//! | `catalog://servers` | tous les serveurs : domaine, description, tags, `enabled`, préfixe, limites, état |
//! | `catalog://domains` | serveurs regroupés par domaine |
//! | `catalog://servers/{name}` | détail d'un serveur + métriques (outils, sondes) |
//! | `catalog://servers/{name}/tools` | inventaire : nom amont, nom à appeler, description, paramètres |
//! | `catalog://guide` | guide d'utilisation (voir [`crate::guide`]) |
//!
//! Toutes les réponses sont générées **à la lecture** à partir de l'état courant : il n'y a
//! pas de seconde source de vérité. Un amont injoignable ne casse pas la ressource : elle
//! répond `"status": "unavailable"` avec l'erreur.

use std::collections::BTreeMap;

use rmcp::{
    ErrorData as McpError,
    model::{Resource, ResourceContents, ResourceTemplate},
};
use serde_json::{Value, json};

use crate::{
    guide,
    state::{AppState, exposed_name},
    upstream::Upstream,
};

const SERVERS: &str = "catalog://servers";
const DOMAINS: &str = "catalog://domains";
const GUIDE: &str = "catalog://guide";
const SERVER_PREFIX: &str = "catalog://servers/";

/// Domaine des serveurs qui n'en déclarent pas.
const NO_DOMAIN: &str = "(sans domaine)";

/// Ressources listées par `resources/list`.
///
/// On liste aussi les ressources concrètes de chaque serveur (en plus des modèles) : certains
/// clients ne savent pas exploiter les `resources/templates/list`.
pub fn list_resources(state: &AppState) -> Vec<Resource> {
    let catalog = &state.config.gateway.catalog;
    if !catalog.enabled {
        return Vec::new();
    }
    let json_resource = |uri: String, name: String, description: String| {
        Resource::new(uri, name)
            .with_description(description)
            .with_mime_type("application/json")
    };

    let mut resources = vec![
        json_resource(
            SERVERS.into(),
            "servers".into(),
            "Tous les serveurs : domaine, description, tags, état de joignabilité, limites".into(),
        ),
        json_resource(
            DOMAINS.into(),
            "domains".into(),
            "Serveurs regroupés par domaine fonctionnel".into(),
        ),
    ];
    if catalog.include_guide {
        resources.push(
            Resource::new(GUIDE, "guide")
                .with_description(
                    "Guide d'utilisation de la passerelle et démarches du déploiement",
                )
                .with_mime_type("text/markdown"),
        );
    }
    for name in state.upstreams.keys() {
        resources.push(json_resource(
            format!("{SERVER_PREFIX}{name}"),
            format!("server-{name}"),
            format!("Détail et métriques du serveur {name}"),
        ));
        if catalog.include_tool_inventory {
            resources.push(json_resource(
                format!("{SERVER_PREFIX}{name}/tools"),
                format!("server-{name}-tools"),
                format!("Inventaire des outils du serveur {name}"),
            ));
        }
    }
    resources
}

/// Modèles de ressources (`resources/templates/list`).
pub fn list_templates(state: &AppState) -> Vec<ResourceTemplate> {
    let catalog = &state.config.gateway.catalog;
    if !catalog.enabled {
        return Vec::new();
    }
    let mut templates = vec![
        ResourceTemplate::new("catalog://servers/{name}", "server")
            .with_description("Détail d'un serveur et ses métriques")
            .with_mime_type("application/json"),
    ];
    if catalog.include_tool_inventory {
        templates.push(
            ResourceTemplate::new("catalog://servers/{name}/tools", "server-tools")
                .with_description("Inventaire des outils d'un serveur (nom à appeler, paramètres)")
                .with_mime_type("application/json"),
        );
    }
    templates
}

/// Lit une ressource du catalogue.
pub async fn read(state: &AppState, uri: &str) -> Result<ResourceContents, McpError> {
    let catalog = &state.config.gateway.catalog;
    let not_found = || McpError::resource_not_found(format!("ressource inconnue: {uri}"), None);
    if !catalog.enabled {
        return Err(not_found());
    }

    let value = match uri {
        SERVERS => servers(state),
        DOMAINS => domains(state),
        GUIDE if catalog.include_guide => {
            let text = guide::render(&state.config)
                .await
                .map_err(|e| McpError::internal_error(format!("guide illisible: {e}"), None))?;
            return Ok(ResourceContents::text(text, uri).with_mime_type("text/markdown"));
        }
        _ => {
            // catalog://servers/{name} ou catalog://servers/{name}/tools
            let rest = uri.strip_prefix(SERVER_PREFIX).ok_or_else(not_found)?;
            let (name, tools) = match rest.strip_suffix("/tools") {
                Some(name) => (name, true),
                None => (rest, false),
            };
            let upstream = state.upstreams.get(name).ok_or_else(not_found)?;
            match tools {
                false => server_detail(upstream),
                true if catalog.include_tool_inventory => tool_inventory(upstream),
                true => return Err(not_found()),
            }
        }
    };
    let text = serde_json::to_string_pretty(&value).expect("JSON sérialisable");
    Ok(ResourceContents::text(text, uri).with_mime_type("application/json"))
}

// ---------------------------------------------------------------------------------------------
// Rendu JSON
// ---------------------------------------------------------------------------------------------

/// Résumé d'un serveur, commun à `catalog://servers` et `catalog://servers/{name}`.
fn server_summary(u: &Upstream) -> Value {
    let c = &u.config;
    let health = u.health_snapshot();
    json!({
        "name": c.name,
        "prefix": format!("{}_", c.name),
        "domain": c.domain,
        "description": c.description,
        "tags": c.tags.0,
        "enabled": c.enabled,
        "transport": c.transport,
        "limits": {
            "timeout_seconds": c.timeout,
            "max_concurrency": c.max_concurrency,
            "cache_ttl_seconds": c.cache_ttl,
        },
        "status": health.status,
        "error": health.last_error,
        "tool_count": health.tool_count,
    })
}

fn servers(state: &AppState) -> Value {
    let list: Vec<Value> = state
        .upstreams
        .values()
        .map(|u| server_summary(u))
        .collect();
    json!({ "servers": list })
}

fn domains(state: &AppState) -> Value {
    let mut by_domain: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for u in state.upstreams.values() {
        let domain = u.config.domain.as_deref().unwrap_or(NO_DOMAIN);
        by_domain.entry(domain).or_default().push(json!({
            "name": u.config.name,
            "description": u.config.description,
            "enabled": u.config.enabled,
            "status": u.availability(),
        }));
    }
    json!({ "domains": by_domain })
}

fn server_detail(u: &Upstream) -> Value {
    let mut value = server_summary(u);
    value["metrics"] = serde_json::to_value(u.health_snapshot()).expect("JSON sérialisable");
    value
}

fn tool_inventory(u: &Upstream) -> Value {
    let health = u.health_snapshot();
    let Some(tools) = u.cached_tools() else {
        // Jamais réussi à lister les outils : on le dit au lieu d'échouer.
        return json!({
            "server": u.config.name,
            "status": health.status,
            "error": health.last_error.or_else(|| (!u.config.enabled).then(|| "serveur désactivé".into())),
            "tools": [],
        });
    };
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "upstream_name": t.name,
                "name": exposed_name(&u.config.name, &t.name),
                "title": t.title,
                "description": t.description,
                "input_schema": t.input_schema,
                "annotations": t.annotations,
            })
        })
        .collect();
    json!({
        "server": u.config.name,
        "status": health.status,
        "error": health.last_error,
        "tools": tools,
    })
}
