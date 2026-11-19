# MCPortal architecture

This document describes the code layout and the internals of the gateway. For usage, see
the [README](README.md). Design choices and their rationale are recorded as
[Architecture Decision Records](docs/adr/README.md), summarized in
[Design decisions](#design-decisions).

Guiding principle: **simplicity first**. A single crate, an immutable shared state, no lock
on the request path, and the MCP protocol delegated to the official SDK
[`rmcp`](https://crates.io/crates/rmcp).

## Overview

```text
                       ┌──────────────────────── MCPortal ────────────────────────┐
                       │                                                          │
 MCP client ──HTTP──▶  │  server.rs ──▶ gateway.rs (rmcp ServerHandler)            │
 (downstream) :8000    │  /mcp /health    ├─ search_tools ──▶ search.rs (BM25)      │
                       │  /ready          ├─ call_tool ─────▶ limits.rs ──┐         │
                       │                  ├─ send_feedback ─▶ logs        │         │
                       │                  └─ resources ─────▶ catalog.rs  │         │
                       │                                      guide.rs    ▼         │
                       │  state.rs  AppState ─────────────────────▶ upstream.rs ───┼──HTTP──▶ upstreams
                       │   ├─ config (fixed)                        (rmcp client)  │
                       │   ├─ upstreams (fixed)                                    │
                       │   └─ catalog: ArcSwap<ToolCatalog>  ◀── refresh          │
                       │                                         loop              │
 Prometheus ──HTTP──▶  │  /metrics :9090  ◀── telemetry.rs                         │
                       └──────────────────────────────────────────────────────────┘
```

## Layout

```text
.
├── Cargo.toml
├── registry.yaml            demo registry
├── guide-ssp.md             deployment-specific guide (served after the generic guide)
├── src/
│   ├── main.rs              binary: reads the config, sets up logs and metrics, runs server::serve
│   ├── lib.rs               library root, suggested reading order
│   ├── config.rs            YAML registry types + validation (pure, no network)
│   ├── upstream.rs          one upstream server: connection, calls, health, tool cache
│   ├── state.rs             AppState, tool catalog, refresh loop
│   ├── search.rs            BM25 index (ToolIndex trait)
│   ├── gateway.rs           exposed MCP server: tools and resources
│   ├── catalog.rs           JSON content of the catalog://… resources
│   ├── guide.rs             guide assembly + guards against drift
│   ├── guide.md             generic guide, compiled into the binary
│   ├── limits.rs            concurrency semaphores
│   ├── server.rs            axum routers, /health, /ready, /metrics, graceful shutdown
│   └── telemetry.rs         tracing logs (JSON) + Prometheus export, metric names
├── tests/
│   ├── common/mod.rs        mock MCP upstream, gateway startup, test client
│   ├── gateway.rs           end-to-end tests (client → gateway → mock upstream)
│   └── guide_drift.rs       consistency of the deployment guide with the real registry
├── benches/overhead.rs      overhead measurement (criterion)
├── deploy/kubernetes.yaml   deployment example
└── Dockerfile
```

All the logic lives in the **library** (`src/lib.rs`); `main.rs` only wires things
together. This is what lets the tests and the benchmark start a real in-memory gateway.

## Modules

### `config`: the registry

`serde` types for the YAML file (`Config`, `GatewayConfig`, `UpstreamConfig`…) with
`deny_unknown_fields` and a default value for every key. `Config::load` resolves
`guide_path` relative to the registry file and checks that it exists. `Config::from_yaml`
allows testing without touching the disk.

### `upstream`: talking to an upstream server

One `Upstream` per registry server. It holds:

- **one rmcp connection shared** by all users. With Streamable HTTP, every call is an
  independent POST: a single client is enough for concurrent calls. The connection is
  opened when first needed and reopened if the transport closes. It is read lock-free
  (`ArcSwapOption`); a mutex only serializes connection attempts;
- **a semaphore** with `max_concurrency` permits;
- **the health state** (consecutive failures, last error, timestamps) and counters;
- **the tool list cache**, timestamped to honor `cache_ttl`.

The protocol version is negotiated in `Auto` mode: `server/discover` (2026-07-28) first,
then a fallback to `initialize` for older upstreams.

Every upstream error is translated into an `UpstreamError` (`Unavailable`, `Timeout`,
`Protocol`). A transport error invalidates the connection, which is reopened on the next
call. Only the connection that failed is invalidated: a late error on an old connection
does not close the one that replaced it.

The `timeout` of an operation covers **everything**, including waiting for a connection:
no call lasts longer than `timeout`. After a failed connection attempt, the failure is kept
for 5 seconds (`RECONNECT_COOLDOWN`): the calls waiting for this attempt, and those that
arrive during that window, fail immediately instead of each retrying in turn. Without it,
N calls to an upstream that accepts TCP connections but never answers would wait
N × `timeout`.

### `state`: the shared state

`AppState` is created once and shared through an `Arc`. It separates:

| Data | Lifetime | Access |
| --- | --- | --- |
| `config`, `upstreams`, `limits` | pod lifetime | direct read |
| `catalog: ArcSwap<ToolCatalog>` | replaced when an upstream changes its tools | lock-free `load()` |
| `ready: AtomicBool` | changes at startup and shutdown | atomic |

The `ToolCatalog` is **immutable**: the `exposed name → ToolEntry` table (a `BTreeMap`, for
a deterministic `tools/list` order) and the BM25 index. Each `ToolEntry` holds the exposed
definition (renamed to `{server}_{tool}`), the upstream name and the compiled JSON Schema
validator.

To change the catalog, `rebuild_catalog` builds a new one alongside, then publishes it in
one go with `store()`. In-flight requests keep the old version until they finish: none of
them sees a half-built catalog. A lock serializes rebuilds, so that a stale version is
never published after a newer one.

### `search`: BM25 search

`Bm25ToolIndex` relies on the `bm25` crate with a French tokenizer (accents removed, stop
words, stemming). Quality mostly comes from the document indexed for each tool:

- the exposed name, split on `_` and repeated (to give it more weight);
- the server's name, domain, tags and description;
- the tool's description;
- the names and descriptions of its parameters.

The module depends only on `rmcp::model::Tool`, behind the `ToolIndex` trait, so it can be
extracted (separate crate or contribution to rmcp).

### `gateway`: the exposed MCP server

`Gateway` implements `ServerHandler` **by hand** rather than with the `#[tool]` macros,
because the tool list depends on the upstreams. The arguments of the gateway's tools are
`serde` + `schemars` structs (`SearchToolsArgs`, `CallToolArgs`, `SendFeedbackArgs`); their
`///` comments become the JSON Schema descriptions seen by the LLM.

`CallToolArgs` mirrors the protocol shape exactly (`name` + `arguments` as a JSON object):
the target tool's arguments are only known at runtime, so they are not typed.

### `catalog` and `guide`: the resources

`catalog.rs` produces the resources' JSON from the current state, on every read: there is
no second source of truth. `guide.rs` assembles `catalog://guide`: the generic guide
(`src/guide.md`, compiled into the binary), then the deployment guide re-read from disk. It
can also extract the servers and tools cited by a guide, which the drift tests use.

### `limits`: concurrency

One semaphore **per server** (`max_concurrency`), acquired within `acquire_timeout_seconds`.
There is no global limit: it would couple the servers to one another (see
[ADR-0004](docs/adr/0004-per-server-concurrency-limits.md)). Permits are released automatically when the `CallPermit` is dropped, including on error or
cancellation. Past the timeout, the call is rejected with a readable message.

### `server` and `main`: HTTP and lifecycle

`router()` mounts rmcp's MCP service on `gateway.http.path`, with:

- `legacy_session_mode(false)`: every request is handled without a session;
- `json_response(true)`: plain JSON response when the tool emits no notification (rmcp
  switches back to SSE otherwise);
- a `Host` header check, disabled by default (suited to running behind an ingress).

`serve()` starts the background tasks (initial load, refresh, metrics port) and handles
shutdown.

### `telemetry`: logs and metrics

`tracing` logs on standard output, as flattened JSON. Metrics through the `metrics` facade,
aggregated by the Prometheus recorder. All metric names are grouped in
`telemetry::metric`.

## Lifecycle of a `call_tool` call

```text
client ──tools/call {name: "call_tool", arguments: {name, arguments}}──▶ rmcp ──▶ Gateway::call_tool
  1. parse CallToolArgs                     failure → JSON-RPC invalid_params error
  2. catalog.load()                         lock-free read of the current version
  3. resolve the exposed name               unknown → isError + close names (BM25)
  4. JSON Schema validation                 invalid → isError + list of errors
  5. limits.acquire (server permit)         timeout exceeded → isError "saturated"
  6. upstream.call_tool                     timeout, cancellation, progress relay
  7. upstream result returned as is         upstream error → readable isError
  8. metrics + "tool call" log
```

The upstream result (`content`, `structuredContent`, `isError`, `_meta`) is never
re-wrapped: images, embedded resources and structured content go through untouched.

### Two levels of error

| Situation | Response |
| --- | --- |
| Malformed request (arguments of our tools cannot be deserialized) | JSON-RPC error |
| Anything the LLM can fix or work around: unknown tool, invalid arguments, saturation, unreachable upstream, timeout, tool error | `CallToolResult` with `isError: true` and an actionable message |

This is what the MCP spec recommends: clients often display JSON-RPC errors opaquely,
whereas an `isError` is read by the LLM.

### Relaying to the upstream: tricky points

- **Cancellation**: if the downstream client cancels (`context.ct`) or the timeout expires,
  the call's future is dropped and `notifications/cancelled` is sent upstream.
- **Progress**: the rmcp client imposes its own `progressToken` on every request (a counter
  per connection). So we send the request, read the chosen token
  (`RequestHandle::progress_token`), then register the "upstream token → downstream client"
  route in the `ProgressRouter` **of that connection**. A notification without a route is
  dropped: either the client asked for no progress (rmcp still sends a token upstream), or,
  in theory, it arrived in the few microseconds between the send and the registration,
  which is negligible next to an HTTP round trip. The downstream client receives its own
  token.
- **Multi-round-trip requests (MRTR)**: an upstream `InputRequiredResult` is returned as is
  to the downstream client. When the client calls `call_tool` again with `inputResponses` /
  `requestState`, these fields are forwarded upstream. The gateway keeps no state between
  rounds.

## Upstream refresh and health

```text
startup ──▶ initial_load: all upstreams in parallel ──▶ ready = true
                                                                │
              one task per upstream, every probe_interval_seconds
                                                                ▼
              cache older than cache_ttl? ──yes──▶ full tools/list
                     │                              └─ list changed? ──▶ rebuild_catalog
                     no
                     └──▶ lightweight probe (first page of tools/list)
              then: record_probe → health, mcportal_upstream_up metric
```

An upstream becomes `unavailable` after `failure_threshold` consecutive failures. Its last
known tools stay in the catalog; a call is still attempted, which allows the upstream to be
recovered without waiting for the next probe.

## Concurrency and threads

Multi-threaded `tokio` runtime; each HTTP request is a task. Rules followed:

- no blocking call on the request path (the guide is read with `tokio::fs`);
- `std::sync::Mutex` only for very short critical sections, without `await`;
- `ArcSwapOption` for the upstream connection (lock-free read on every call), and a
  `tokio::sync::Mutex` held only while connecting, so that two simultaneous requests open
  only one connection;
- `ArcSwap` for data read everywhere and written rarely (the catalog).

## Startup and shutdown

```text
startup : Config::load ─▶ logs ─▶ Prometheus ─▶ listen on ports ─▶ /health = 200
          ─▶ initial_load (in the background) ─▶ /ready = 200

SIGTERM : /ready = 503 ─▶ axum stops accepting connections and waits for in-flight requests
          ─▶ background tasks cancelled ─▶ exit
```

`/health` does not depend on the upstreams: a failing upstream must not cause the gateway
to restart. `/ready` waits for the first tool load, so that a new pod does not receive
traffic with an empty catalog during a rolling update.

## Observability

| Metric | Type | Labels |
| --- | --- | --- |
| `mcportal_tool_calls_total` | counter | server, tool, status |
| `mcportal_tool_call_duration_seconds` | histogram | server |
| `mcportal_gateway_overhead_seconds` | histogram | — |
| `mcportal_upstream_up` | gauge | server |
| `mcportal_inflight` | gauge | server |
| `mcportal_ratelimit_rejections_total` | counter | scope (always `server`) |
| `mcportal_search_queries_total` | counter | — |
| `mcportal_catalog_tools` | gauge | — |
| `mcportal_feedback_total` | counter | server |

`status` values: `ok`, `tool_error`, `upstream_error`, `timeout`, `rejected`, `invalid`,
`cancelled`. No metric carries a user identifier or a value chosen by the client, to avoid
a cardinality explosion: a call to a name missing from the catalog is counted with
`server="unknown"` and `tool="unknown"`, and the requested name (truncated to 128
characters) only appears in the log (`requested_tool`).

Each relayed call produces a `"message": "tool call"` log with `server`, `tool`, `status`,
`duration_ms` and `upstream_ms`. Feedback from `send_feedback` carries
`"label": "FEEDBACK"`.

The endpoint is public and unauthenticated, so free text is capped: `search_tools.query`
at 500 characters, `send_feedback.author` at 200 and `send_feedback.feedback` at 4000. A
longer value is rejected with a tool error that tells the LLM to shorten it.

## Testing strategy

| Level | Where | What is checked |
| --- | --- | --- |
| Unit | `#[cfg(test)]` in each module | config, BM25, semaphores, extraction of guide citations |
| Integration | `tests/gateway.rs` | end to end with real mock MCP servers: search, calls, errors, timeout, saturation, catalog, guide, progress, older clients, graceful shutdown |
| Guide drift | `tests/guide_drift.rs` (ignored by default) | cited servers enabled in the registry; cited tools present on the real upstreams |
| Performance | `benches/overhead.rs` | direct call vs through the gateway |

## Design decisions

The **why** behind the structural choices is recorded as Architecture Decision Records in
[`docs/adr/`](docs/adr/README.md), so that each decision can be challenged, and superseded,
with full knowledge of the trade-offs. Summary:

| ADR | Decision | Main rejected option |
| --- | --- | --- |
| [0002](docs/adr/0002-mcp-2026-07-28-stateless.md) | MCP 2026-07-28, stateless: any pod serves any request | MCP sessions (would require sticky sessions) |
| [0003](docs/adr/0003-no-configuration-hot-reload.md) | Config change = rolling update | Hot reload (pointless without sessions) |
| [0004](docs/adr/0004-per-server-concurrency-limits.md) | Concurrency limit per server | Global limit (couples the servers without protecting the gateway) |
| [0005](docs/adr/0005-no-authentication-in-v1.md) | No authentication, no per-user limit in V1 | Per-IP limit (ineffective with Claude.ai connectors) |
| [0006](docs/adr/0006-bm25-search.md) | BM25 search with the `bm25` crate | tantivy / DuckDB (oversized) |
| [0007](docs/adr/0007-catalog-as-resources.md) | Catalog exposed as resources | Catalog tools, hand-written inventory |
| [0008](docs/adr/0008-prometheus-metrics.md) | Prometheus metrics on a private port | Metrics computed from logs |
| [0009](docs/adr/0009-no-upstream-sse-transport.md) | No upstream `sse` transport | Implementing a deprecated transport |

## Overhead measurement

Measured locally with `cargo bench --bench overhead`:

| Path | Median time |
| --- | --- |
| client → upstream | ~0.95 ms |
| client → gateway → upstream | ~1.8 ms |
| `search_tools` (no upstream) | ~0.87 ms |

The overhead (~0.9 ms) mostly comes from the extra HTTP hop; the gateway's own processing
(resolution, validation, semaphores) is negligible. Plain JSON responses rather than an SSE
stream (`json_response`) reduced it by about 20%.
