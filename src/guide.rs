//! # The guide (`catalog://guide`)
//!
//! The guide is made of two layers, served in a single resource:
//!
//! 1. **the mechanics** ([`MECHANICS`], file `src/guide.md`): embedded in the binary at
//!    compile time (`include_str!`). It cites no deployment server or tool; a new version
//!    of the binary updates it;
//! 2. **the deployment workflows** (`gateway.catalog.guide_path`): re-read from disk **on
//!    every read** of the resource, so they can be changed without a restart (ConfigMap).
//!
//! ## Guards against deployment guide drift
//!
//! The deployment guide cites real servers and tools. To keep it from drifting away from
//! the registry, this module can extract these citations ([`citations`]):
//!
//! - [`check_against_registry`] (hermetic test): every cited server must be enabled;
//! - [`cited_upstream_tools`] (network integration test): every cited tool must exist in
//!   the real inventory of its upstream.
//!
//! A citation is a backtick-quoted span (`` `…` ``):
//! - `server_tool(…)` or `server_tool` → a tool;
//! - `catalog://servers/{name}…` → a server.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Config;

/// Generic guide, compiled into the binary.
pub const MECHANICS: &str = include_str!("guide.md");

/// The gateway's own tools (no upstream server behind them).
pub const GATEWAY_TOOLS: &[&str] = &["search_tools", "call_tool", "send_feedback"];

/// Assembles the full guide: mechanics, separator, then the deployment guide (if any).
pub async fn render(config: &Config) -> std::io::Result<String> {
    let mut text = MECHANICS.to_owned();
    if let Some(path) = &config.gateway.catalog.guide_path {
        let deployment = tokio::fs::read_to_string(path).await?;
        text.push_str("\n\n---\n\n");
        text.push_str(&deployment);
    }
    Ok(text)
}

/// A citation found in the guide.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Citation {
    /// Backtick-quoted identifier (e.g. `insee_get_insee_document`).
    /// `is_call` = followed by a parenthesis (`geodata_geocode(text=…)`).
    Identifier { token: String, is_call: bool },
    /// `catalog://servers/{name}…`
    CatalogServer(String),
}

/// Extracts citations from code spans (between backticks).
pub fn citations(text: &str) -> Vec<Citation> {
    let mut found = Vec::new();
    // Split on '`': odd-indexed segments are inside a code span.
    for span in text.split('`').skip(1).step_by(2) {
        if let Some(rest) = span.strip_prefix("catalog://servers/") {
            let name: String = rest.chars().take_while(|c| *c != '/').collect();
            if !name.is_empty() && !name.starts_with('{') {
                found.push(Citation::CatalogServer(name));
            }
            continue;
        }
        let token: String = span
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if token.is_empty() {
            continue;
        }
        let is_call = span[token.len()..].starts_with('(');
        found.push(Citation::Identifier { token, is_call });
    }
    found
}

/// Finds the registry server whose name is the prefix of `token` (`{server}_…`).
/// With nested prefixes (`a` and `a_b`), the longest wins.
fn server_prefix<'a>(token: &str, config: &'a Config) -> Option<&'a str> {
    config
        .servers
        .iter()
        .map(|s| s.name.as_str())
        .filter(|name| token.len() > name.len() + 1 && token.starts_with(&format!("{name}_")))
        .max_by_key(|name| name.len())
}

/// Checks that every server cited by the guide is declared **and enabled** in the registry.
/// Returns the list of problems (empty = all good).
pub fn check_against_registry(text: &str, config: &Config) -> Vec<String> {
    let enabled = |name: &str| config.servers.iter().any(|s| s.name == name && s.enabled);
    let declared = |name: &str| config.servers.iter().any(|s| s.name == name);
    let mut problems = BTreeSet::new();

    for citation in citations(text) {
        match citation {
            Citation::CatalogServer(name) => {
                if !enabled(&name) {
                    problems.insert(format!(
                        "server '{name}' cited (catalog://) but missing or disabled"
                    ));
                }
            }
            Citation::Identifier { token, is_call } => {
                if GATEWAY_TOOLS.contains(&token.as_str()) {
                    continue;
                }
                if let Some(server) = server_prefix(&token, config) {
                    if !enabled(server) {
                        problems.insert(format!("tool '{token}': server '{server}' disabled"));
                    }
                } else if is_call {
                    // A call `x_y(…)` without a known prefix: server unknown to the registry.
                    problems.insert(format!(
                        "tool '{token}': no registry server has this prefix"
                    ));
                } else if token.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    && !declared(&token)
                    && looks_like_server_row(text, &token)
                {
                    problems.insert(format!(
                        "server '{token}' cited but missing from the registry"
                    ));
                }
            }
        }
    }
    problems.into_iter().collect()
}

/// Heuristic: a single word is a server name if it opens a table row
/// (`| `insee` | …`), the form guides use to present servers.
fn looks_like_server_row(text: &str, token: &str) -> bool {
    let cell = format!("| `{token}` |");
    text.lines()
        .any(|line| line.trim_start().starts_with(&cell))
}

/// Upstream tools cited by the guide, per server (upstream name, without prefix).
pub fn cited_upstream_tools(text: &str, config: &Config) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_server: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for citation in citations(text) {
        if let Citation::Identifier { token, .. } = citation
            && let Some(server) = server_prefix(&token, config)
        {
            let tool = token[server.len() + 1..].to_owned();
            by_server.entry(server.to_owned()).or_default().insert(tool);
        }
    }
    by_server
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Config {
        Config::from_yaml(
            r#"
gateway: {name: T}
servers:
  - {name: insee, url: http://x/mcp}
  - {name: geodata, url: http://x/mcp}
  - {name: old, url: http://x/mcp, enabled: false}
"#,
        )
        .unwrap()
    }

    #[test]
    fn mechanics_guide_cites_no_deployment_server() {
        // The mechanics must hold whatever the registry.
        let empty = Config::from_yaml("gateway: {name: T}").unwrap();
        assert!(check_against_registry(MECHANICS, &empty).is_empty());
        assert!(cited_upstream_tools(MECHANICS, &registry()).is_empty());
    }

    #[test]
    fn extracts_calls_identifiers_and_catalog_uris() {
        let text = "Call `geodata_geocode(text=\"Paris\")` then `insee_search`. See `catalog://servers/insee/tools` and `catalog://servers/{name}`.";
        assert_eq!(
            citations(text),
            vec![
                Citation::Identifier {
                    token: "geodata_geocode".into(),
                    is_call: true
                },
                Citation::Identifier {
                    token: "insee_search".into(),
                    is_call: false
                },
                Citation::CatalogServer("insee".into()),
            ]
        );
    }

    #[test]
    fn valid_guide_has_no_problem() {
        let text = "| `insee` | … |\n`insee_search(query=…)`, `geo_level`, `send_feedback(server=…)`, `catalog://servers/geodata`";
        assert!(check_against_registry(text, &registry()).is_empty());
    }

    #[test]
    fn detects_unknown_and_disabled_servers() {
        let text = "| `melodi` | … |\n`rmes_run_sparql(q)` `old_tool` `catalog://servers/ghost`";
        let problems = check_against_registry(text, &registry());
        assert_eq!(problems.len(), 4, "{problems:#?}");
    }

    #[test]
    fn groups_cited_tools_by_server() {
        let text = "`insee_a()` `insee_b` `geodata_c(x)` `geo_level`";
        let cited = cited_upstream_tools(text, &registry());
        assert_eq!(
            cited["insee"],
            BTreeSet::from(["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(cited["geodata"], BTreeSet::from(["c".to_owned()]));
        assert_eq!(cited.len(), 2);
    }
}
