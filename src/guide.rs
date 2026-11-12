//! # Le guide (`catalog://guide`)
//!
//! Le guide est composé de deux couches, servies dans une seule ressource :
//!
//! 1. **la mécanique** ([`MECHANICS`], fichier `src/guide.md`) : embarquée dans le binaire à
//!    la compilation (`include_str!`). Elle ne cite aucun serveur ni outil du déploiement ;
//!    une nouvelle version du binaire la met à jour ;
//! 2. **les démarches du déploiement** (`gateway.catalog.guide_path`) : relues sur le disque
//!    **à chaque lecture** de la ressource, donc modifiables sans redémarrage (ConfigMap).
//!
//! ## Garde-fous contre la dérive du guide de déploiement
//!
//! Le guide de déploiement cite des serveurs et des outils réels. Pour éviter qu'il ne
//! dérive du registre, ce module sait extraire ces citations ([`citations`]) :
//!
//! - [`check_against_registry`] (test hermétique) : chaque serveur cité doit être activé ;
//! - [`cited_upstream_tools`] (test d'intégration réseau) : chaque outil cité doit exister
//!   dans l'inventaire réel de son amont.
//!
//! Une citation est un passage entre accents graves (`` `…` ``) :
//! - `serveur_outil(…)` ou `serveur_outil` → un outil ;
//! - `catalog://servers/{nom}…` → un serveur.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Config;

/// Guide générique, compilé dans le binaire.
pub const MECHANICS: &str = include_str!("guide.md");

/// Outils propres à la passerelle (pas de serveur amont derrière).
pub const GATEWAY_TOOLS: &[&str] = &["search_tools", "call_tool", "send_feedback"];

/// Assemble le guide complet : mécanique, séparateur, puis guide de déploiement (s'il existe).
pub async fn render(config: &Config) -> std::io::Result<String> {
    let mut text = MECHANICS.to_owned();
    if let Some(path) = &config.gateway.catalog.guide_path {
        let deployment = tokio::fs::read_to_string(path).await?;
        text.push_str("\n\n---\n\n");
        text.push_str(&deployment);
    }
    Ok(text)
}

/// Une citation trouvée dans le guide.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Citation {
    /// Identifiant entre accents graves (ex. `insee_get_insee_document`).
    /// `is_call` = suivi d'une parenthèse (`geodata_geocode(text=…)`).
    Identifier { token: String, is_call: bool },
    /// `catalog://servers/{nom}…`
    CatalogServer(String),
}

/// Extrait les citations des passages de code (entre accents graves).
pub fn citations(text: &str) -> Vec<Citation> {
    let mut found = Vec::new();
    // Découper sur '`' : les segments d'indice impair sont à l'intérieur d'un passage de code.
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

/// Trouve le serveur du registre dont `token` porte le préfixe (`{serveur}_…`).
/// En cas de préfixes imbriqués (`a` et `a_b`), le plus long gagne.
fn server_prefix<'a>(token: &str, config: &'a Config) -> Option<&'a str> {
    config
        .servers
        .iter()
        .map(|s| s.name.as_str())
        .filter(|name| token.len() > name.len() + 1 && token.starts_with(&format!("{name}_")))
        .max_by_key(|name| name.len())
}

/// Vérifie que chaque serveur cité par le guide est déclaré **et activé** dans le registre.
/// Renvoie la liste des problèmes (vide = tout va bien).
pub fn check_against_registry(text: &str, config: &Config) -> Vec<String> {
    let enabled = |name: &str| config.servers.iter().any(|s| s.name == name && s.enabled);
    let declared = |name: &str| config.servers.iter().any(|s| s.name == name);
    let mut problems = BTreeSet::new();

    for citation in citations(text) {
        match citation {
            Citation::CatalogServer(name) => {
                if !enabled(&name) {
                    problems.insert(format!(
                        "serveur '{name}' cité (catalog://) mais absent ou désactivé"
                    ));
                }
            }
            Citation::Identifier { token, is_call } => {
                if GATEWAY_TOOLS.contains(&token.as_str()) {
                    continue;
                }
                if let Some(server) = server_prefix(&token, config) {
                    if !enabled(server) {
                        problems.insert(format!("outil '{token}' : serveur '{server}' désactivé"));
                    }
                } else if is_call {
                    // Un appel `x_y(…)` sans préfixe connu : serveur inconnu du registre.
                    problems.insert(format!(
                        "outil '{token}' : aucun serveur du registre ne porte ce préfixe"
                    ));
                } else if token.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    && !declared(&token)
                    && looks_like_server_row(text, &token)
                {
                    problems.insert(format!("serveur '{token}' cité mais absent du registre"));
                }
            }
        }
    }
    problems.into_iter().collect()
}

/// Heuristique : un mot seul est un nom de serveur s'il ouvre une ligne de tableau
/// (`| `insee` | …`), forme utilisée par les guides pour présenter les serveurs.
fn looks_like_server_row(text: &str, token: &str) -> bool {
    let cell = format!("| `{token}` |");
    text.lines()
        .any(|line| line.trim_start().starts_with(&cell))
}

/// Outils amont cités par le guide, par serveur (nom amont, sans préfixe).
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
        // La mécanique doit rester vraie quel que soit le registre.
        let empty = Config::from_yaml("gateway: {name: T}").unwrap();
        assert!(check_against_registry(MECHANICS, &empty).is_empty());
        assert!(cited_upstream_tools(MECHANICS, &registry()).is_empty());
    }

    #[test]
    fn extracts_calls_identifiers_and_catalog_uris() {
        let text = "Appelez `geodata_geocode(text=\"Paris\")` puis `insee_search`. Voir `catalog://servers/insee/tools` et `catalog://servers/{nom}`.";
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
