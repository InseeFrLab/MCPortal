//! # Mesure de l'overhead de la passerelle
//!
//! On compare le même appel d'outil (`echo`) sur un serveur MCP simulé :
//!
//! - `direct`      : client ──▶ amont ;
//! - `via_gateway` : client ──▶ passerelle ──▶ amont (via `call_tool`).
//!
//! La différence des deux temps est le coût ajouté par la passerelle : un saut HTTP local
//! supplémentaire + résolution du nom + validation JSON Schema + sémaphores.
//! Tout tourne en local (127.0.0.1) : le chiffre mesure la passerelle, pas le réseau.
//!
//! ```text
//! cargo bench --bench overhead
//! ```
//!
//! En production, la métrique `mcportal_gateway_overhead_seconds` donne la même mesure
//! (temps total − temps amont) sur le trafic réel.

#[path = "../tests/common/mod.rs"]
mod common;

use common::*;
use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn overhead(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let ct = CancellationToken::new();

    let (direct, via_gateway, _gateway) = rt.block_on(async {
        let upstream = spawn_upstream(&ct).await;
        let gateway = spawn_gateway(config(&[("mock", &upstream)], "", "")).await;
        let direct = connect(&upstream).await;
        let via_gateway = connect(&gateway.url).await;
        // Vérification préalable : les deux chemins répondent la même chose.
        assert_eq!(
            text(&call(&direct, "echo", json!({"text": "x"})).await),
            "x"
        );
        assert_eq!(
            text(&call_via_gateway(&via_gateway, "mock_echo", json!({"text": "x"})).await),
            "x"
        );
        (direct, via_gateway, gateway)
    });

    let mut group = c.benchmark_group("call_tool_echo");
    group.bench_function("direct", |b| {
        b.to_async(&rt)
            .iter(|| call(&direct, "echo", json!({"text": "bonjour"})))
    });
    group.bench_function("via_gateway", |b| {
        b.to_async(&rt)
            .iter(|| call_via_gateway(&via_gateway, "mock_echo", json!({"text": "bonjour"})))
    });
    group.finish();

    // Recherche seule (pas d'appel amont) : coût de BM25 sur un petit catalogue.
    c.bench_function("search_tools", |b| {
        b.to_async(&rt)
            .iter(|| call(&via_gateway, "search_tools", json!({"query": "écho texte"})))
    });

    ct.cancel();
}

criterion_group!(benches, overhead);
criterion_main!(benches);
