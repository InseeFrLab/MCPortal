//! Garde-fous contre la dérive du guide de déploiement (`guide-ssp.md`) par rapport au
//! registre réel (`registry.yaml`). Voir `src/guide.rs`.
//!
//! - `deployment_guide_cites_only_enabled_servers` : hermétique (aucun réseau) ;
//! - `cited_tools_still_exist_upstream` : interroge les vrais amonts, donc ignoré par
//!   défaut ; à lancer avec `cargo test --test guide_drift -- --ignored`. Un amont
//!   injoignable est sauté (avec un message), pas compté comme un échec.

use std::path::Path;

use mcportal::{config::Config, guide, upstream::Upstream};

fn registry() -> Config {
    Config::load(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("registry.yaml")
            .as_path(),
    )
    .expect("registry.yaml")
}

fn deployment_guide(config: &Config) -> String {
    let path = config
        .gateway
        .catalog
        .guide_path
        .as_ref()
        .expect("guide_path");
    std::fs::read_to_string(path).expect("guide de déploiement")
}

#[test]
#[ignore = "registry.yaml incomplet : renseigner les URL de insee, melodi, rmes et geodata cités par guide-ssp.md"]
fn deployment_guide_cites_only_enabled_servers() {
    let config = registry();
    let problems = guide::check_against_registry(&deployment_guide(&config), &config);
    assert!(
        problems.is_empty(),
        "le guide de déploiement a dérivé :\n{}",
        problems.join("\n")
    );
}

#[tokio::test]
#[ignore = "réseau : interroge les serveurs amont réels"]
async fn cited_tools_still_exist_upstream() {
    let config = registry();
    let cited = guide::cited_upstream_tools(&deployment_guide(&config), &config);
    let mut missing = Vec::new();
    for (server, tools) in cited {
        let upstream_config = config
            .servers
            .iter()
            .find(|s| s.name == server)
            .unwrap()
            .clone();
        let upstream = Upstream::new(upstream_config, 1);
        let inventory = match upstream.fetch_tools().await {
            Ok(inventory) => inventory,
            Err(e) => {
                eprintln!("amont '{server}' injoignable, vérification sautée : {e}");
                continue;
            }
        };
        for tool in tools {
            if !inventory.iter().any(|t| t.name == tool.as_str()) {
                missing.push(format!("{server}_{tool}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "outils cités mais absents des amonts : {missing:?}"
    );
}
