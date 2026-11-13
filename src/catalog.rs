//! # The catalog (`catalog://…` resources)
//!
//! The catalog is exposed as MCP **resources**, not tools: it costs nothing in the client's
//! context until the client reads it, and it stays readable when tool search hides
//! `tools/list`.
//!
//! | URI | Content |
//! | --- | --- |
//! | `catalog://servers` | all servers: domain, description, tags, `enabled`, prefix, limits, status |
//! | `catalog://domains` | servers grouped by domain |
//! | `catalog://servers/{name}` | details of a server + metrics (tools, probes) |
//! | `catalog://servers/{name}/tools` | inventory: upstream name, name to call, description, parameters |
//! | `catalog://guide` | usage guide (see [`crate::guide`]) |
//!
//! Every response is generated **on read** from the current state: there is no second
//! source of truth. An unreachable upstream does not break the resource: it responds with
//! `"status": "unavailable"` and the error.

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

/// Domain of servers that do not declare one.
const NO_DOMAIN: &str = "(no domain)";

/// Resources listed by `resources/list`.
///
/// The concrete resources of each server are listed too (on top of the templates): some
/// clients cannot use `resources/templates/list`.
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
            "All servers: domain, description, tags, reachability status, limits".into(),
        ),
        json_resource(
            DOMAINS.into(),
            "domains".into(),
            "Servers grouped by functional domain".into(),
        ),
    ];
    if catalog.include_guide {
        resources.push(
            Resource::new(GUIDE, "guide")
                .with_description("Gateway usage guide and deployment workflows")
                .with_mime_type("text/markdown"),
        );
    }
    for name in state.upstreams.keys() {
        resources.push(json_resource(
            format!("{SERVER_PREFIX}{name}"),
            format!("server-{name}"),
            format!("Details and metrics of server {name}"),
        ));
        if catalog.include_tool_inventory {
            resources.push(json_resource(
                format!("{SERVER_PREFIX}{name}/tools"),
                format!("server-{name}-tools"),
                format!("Tool inventory of server {name}"),
            ));
        }
    }
    resources
}

/// Resource templates (`resources/templates/list`).
pub fn list_templates(state: &AppState) -> Vec<ResourceTemplate> {
    let catalog = &state.config.gateway.catalog;
    if !catalog.enabled {
        return Vec::new();
    }
    let mut templates = vec![
        ResourceTemplate::new("catalog://servers/{name}", "server")
            .with_description("Details of a server and its metrics")
            .with_mime_type("application/json"),
    ];
    if catalog.include_tool_inventory {
        templates.push(
            ResourceTemplate::new("catalog://servers/{name}/tools", "server-tools")
                .with_description("Tool inventory of a server (name to call, parameters)")
                .with_mime_type("application/json"),
        );
    }
    templates
}

/// Reads a catalog resource.
pub async fn read(state: &AppState, uri: &str) -> Result<ResourceContents, McpError> {
    let catalog = &state.config.gateway.catalog;
    let not_found = || McpError::resource_not_found(format!("unknown resource: {uri}"), None);
    if !catalog.enabled {
        return Err(not_found());
    }

    let value = match uri {
        SERVERS => servers(state),
        DOMAINS => domains(state),
        GUIDE if catalog.include_guide => {
            let text = guide::render(&state.config)
                .await
                .map_err(|e| McpError::internal_error(format!("unreadable guide: {e}"), None))?;
            return Ok(ResourceContents::text(text, uri).with_mime_type("text/markdown"));
        }
        _ => {
            // catalog://servers/{name} or catalog://servers/{name}/tools
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
    let text = serde_json::to_string_pretty(&value).expect("serializable JSON");
    Ok(ResourceContents::text(text, uri).with_mime_type("application/json"))
}

// ---------------------------------------------------------------------------------------------
// JSON rendering
// ---------------------------------------------------------------------------------------------

/// Summary of a server, shared by `catalog://servers` and `catalog://servers/{name}`.
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
    value["metrics"] = serde_json::to_value(u.health_snapshot()).expect("serializable JSON");
    value
}

fn tool_inventory(u: &Upstream) -> Value {
    let health = u.health_snapshot();
    let Some(tools) = u.cached_tools() else {
        // Never managed to list the tools: say so instead of failing.
        return json!({
            "server": u.config.name,
            "status": health.status,
            "error": health.last_error.or_else(|| (!u.config.enabled).then(|| "server disabled".into())),
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
