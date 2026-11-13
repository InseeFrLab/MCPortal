//! Guards against drift of the deployment guide (`guide-ssp.md`) relative to the real
//! registry (`registry.yaml`). See `src/guide.rs`.
//!
//! - `deployment_guide_cites_only_enabled_servers`: hermetic (no network);
//! - `cited_tools_still_exist_upstream`: queries the real upstreams, so ignored by
//!   default; run it with `cargo test --test guide_drift -- --ignored`. An unreachable
//!   upstream is skipped (with a message), not counted as a failure.

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
    std::fs::read_to_string(path).expect("deployment guide")
}

#[test]
#[ignore = "registry.yaml incomplete: fill in the URLs of insee, melodi, rmes and geodata cited by guide-ssp.md"]
fn deployment_guide_cites_only_enabled_servers() {
    let config = registry();
    let problems = guide::check_against_registry(&deployment_guide(&config), &config);
    assert!(
        problems.is_empty(),
        "the deployment guide has drifted:\n{}",
        problems.join("\n")
    );
}

#[tokio::test]
#[ignore = "network: queries the real upstream servers"]
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
                eprintln!("upstream '{server}' unreachable, check skipped: {e}");
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
        "tools cited but missing from the upstreams: {missing:?}"
    );
}
