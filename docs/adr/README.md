# Architecture Decision Records

Why MCPortal is built the way it is. How it works now is described in
[ARCHITECTURE.md](../../ARCHITECTURE.md). Process: see [ADR-0001](0001-record-architecture-decisions.md).

New ADR: copy [template.md](template.md) to the next free number, status `Proposed` in the pull
request, `Accepted` when merged. An accepted ADR is not rewritten: supersede it.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-mcp-2026-07-28-stateless.md) | MCP protocol 2026-07-28 and stateless mode | Accepted |
| [0003](0003-no-configuration-hot-reload.md) | No configuration hot reload | Accepted |
| [0004](0004-per-server-concurrency-limits.md) | Concurrency limits per server, no global limit | Accepted |
| [0005](0005-no-authentication-in-v1.md) | No authentication and no per-user limit in V1 | Accepted |
| [0006](0006-bm25-search.md) | Tool search with the `bm25` crate | Accepted |
| [0007](0007-catalog-as-resources.md) | Catalog as resources rather than tools | Accepted |
| [0008](0008-prometheus-metrics.md) | Prometheus metrics on top of logs | Accepted |
| [0009](0009-no-upstream-sse-transport.md) | Upstream `sse` transport not supported | Accepted |
