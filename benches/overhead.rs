//! # Gateway overhead measurement
//!
//! Compares the same tool call (`echo`) on a mock MCP server:
//!
//! - `direct`      : client ──▶ upstream;
//! - `via_gateway` : client ──▶ gateway ──▶ upstream (through `call_tool`).
//!
//! The difference between the two times is the cost added by the gateway: one extra local
//! HTTP hop + name resolution + JSON Schema validation + semaphores.
//! Everything runs locally (127.0.0.1): the figure measures the gateway, not the network.
//!
//! ```text
//! cargo bench --bench overhead
//! ```
//!
//! In production, the `mcportal_gateway_overhead_seconds` metric gives the same measurement
//! (total time − upstream time) on real traffic.

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
        // Sanity check: both paths return the same thing.
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
            .iter(|| call(&direct, "echo", json!({"text": "hello"})))
    });
    group.bench_function("via_gateway", |b| {
        b.to_async(&rt)
            .iter(|| call_via_gateway(&via_gateway, "mock_echo", json!({"text": "hello"})))
    });
    group.finish();

    // Search only (no upstream call): BM25 cost on a small catalog.
    c.bench_function("search_tools", |b| {
        b.to_async(&rt)
            .iter(|| call(&via_gateway, "search_tools", json!({"query": "echo text"})))
    });

    ct.cancel();
}

criterion_group!(benches, overhead);
criterion_main!(benches);
