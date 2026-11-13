# MCPortal architecture

This document describes the code layout and the internals of the gateway. For usage, see
the [README](README.md). Design choices and their rationale are grouped at the end, in
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
[Design decisions](#concurrency-and-limits-per-server-in-v1)). Permits are released automatically when the `CallPermit` is dropped, including on error or
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
`cancelled`. No metric carries a user identifier, to avoid a cardinality explosion.

Each relayed call produces a `"message": "tool call"` log with `server`, `tool`, `status`,
`duration_ms` and `upstream_ms`. Feedback from `send_feedback` carries
`"label": "FEEDBACK"`.

## Testing strategy

| Level | Where | What is checked |
| --- | --- | --- |
| Unit | `#[cfg(test)]` in each module | config, BM25, semaphores, extraction of guide citations |
| Integration | `tests/gateway.rs` | end to end with real mock MCP servers: search, calls, errors, timeout, saturation, catalog, guide, progress, older clients, graceful shutdown |
| Guide drift | `tests/guide_drift.rs` (ignored by default) | cited servers enabled in the registry; cited tools present on the real upstreams |
| Performance | `benches/overhead.rs` | direct call vs through the gateway |

## Design decisions

This section records the **why** behind the structural choices, so that they can be
challenged with full knowledge of the trade-offs.

### MCP protocol 2026-07-28 and stateless mode

The 2026-07-28 revision of the MCP spec removes sessions (`Mcp-Session-Id`) and the
`initialize` handshake: each request carries its protocol version, the client's
capabilities and identity in `_meta`.

- **Downstream**, the gateway serves every request without a session, including for older
  clients (`legacy_session_mode(false)`). Consequence: any pod can serve any request.
  Several replicas work without sticky sessions, and a pod restart disconnects no client.
- **Upstream**, one rmcp client shared per server. Upstreams still on a 2025-xx version keep
  their sessions, managed by rmcp (an expired session is reopened automatically).
- `tools/list` returns `ttlMs` and `cacheScope: public`: the list is the same for every
  client.

### No configuration hot reload

The goal was to avoid outages when the config changes on Kubernetes. With a sessionless
protocol, a **rolling update** is enough and disconnects no one:

1. the ConfigMap changes; a `checksum/config` annotation (Helm) or the Reloader tool
   triggers the rolling update;
2. the new pod starts and only reports ready after the first tool load;
3. the old pod receives SIGTERM, reports not ready and finishes its in-flight calls.

What makes this possible is the graceful shutdown described above, together with a
`preStop` of a few seconds on the Kubernetes side and a `terminationGracePeriodSeconds`
greater than the largest upstream `timeout`.

The cost of a hot reload would have been moderate (about 200 lines), but with pitfalls:
semaphores resized while permits are held, non-reloadable settings (listening port),
reliable detection of ConfigMap updates (atomic replacement of a symbolic link). If the
need comes back, the catalog's `ArcSwap` already provides the swap mechanism: what would
remain is watching the file and diffing the upstreams.

### Concurrency and limits: per server in V1

One limit per server (`max_concurrency`) protects each upstream without needing to know who
is calling. There is no authentication and no per-user limit in V1.

A global limit across all servers existed at first and was removed. It did not protect the
gateway (an in-flight call costs a tokio task and an HTTP request, and the total is already
bounded by the sum of the servers' limits), but it **coupled the servers**: calls waiting
for a permit on a slow or failing server held global permits, and calls to healthy servers
were rejected as "saturated". Below the sum of the servers' limits it causes this coupling;
above it, it never applies. The `global_limit` key is still accepted in the registry
(ignored, with a warning) so that existing configs keep loading. If the gateway itself ever
needs protecting, it belongs on incoming requests (a concurrency layer on `/mcp`, or
autoscaling), not on upstream calls.

Points to keep in mind for later:

- **The MCP client does not identify the user**: `clientInfo` describes the software (and
  is self-declared), `Mcp-Session-Id` no longer exists. The only mechanism provided by the
  spec is `Authorization: Bearer` (OAuth 2.1), with the gateway acting as the *resource
  server*.
- **A per-IP-address limit does not work with Claude.ai**: connectors call from
  Anthropic's servers, so all their users would share a few addresses.
- Options considered, from simplest to most complete:

  | Mode | Identity | Note |
  | --- | --- | --- |
  | `api_key` | personal key in `Authorization: Bearer` | simple, no identity provider; keys in a Kubernetes Secret |
  | `oauth` | token from an identity provider (Keycloak, possibly federated with GitHub or Google) | the cleanest; the user signs in through their browser. Limitation: an account on that provider is required |

- **Never forward a user's token to the upstreams** (forbidden by the spec, "confused
  deputy" risk): if an upstream requires authentication, the gateway uses its own
  credentials.

### Search with the `bm25` crate

There are at most a few hundred tools: the index is rebuilt in a few milliseconds and fits
in memory. A full engine (tantivy) or an embedded database (DuckDB and its full-text
extension) would be oversized. The module is isolated behind the `ToolIndex` trait with a
view to contributing it to the rmcp SDK (optional module or separate crate, to be
discussed with its maintainers first).

### Catalog as resources rather than tools

A resource costs nothing in the client's context until it reads it, and it stays readable
when tool search hides `tools/list`. The catalog is always generated from the registry and
the upstreams' state; the deployment guide contains narrative and pointers, never an
inventory.

### Prometheus metrics on top of logs

JSON logs go to Loki, but computing latencies and error rates from logs is costly and
imprecise. Prometheus metrics, served on a separate port that is not publicly exposed, feed
the Grafana dashboards cheaply.

### Upstream `sse` transport not supported

The old HTTP+SSE transport is deprecated by the spec and no longer exists in the rmcp 3.x
client. The value is still accepted in the registry so as not to break an existing config,
but the server is reported `unavailable` with an explicit message.

### Overhead measurement

Measured locally with `cargo bench --bench overhead`:

| Path | Median time |
| --- | --- |
| client → upstream | ~0.95 ms |
| client → gateway → upstream | ~1.8 ms |
| `search_tools` (no upstream) | ~0.87 ms |

The overhead (~0.9 ms) mostly comes from the extra HTTP hop; the gateway's own processing
(resolution, validation, semaphores) is negligible. Plain JSON responses rather than an SSE
stream (`json_response`) reduced it by about 20%.

### Summary of rejected options

| Option | Why not (for now) |
| --- | --- |
| Configuration hot reload | pointless without sessions: a rolling update disconnects no one |
| MCP sessions | removed by the 2026-07-28 spec; they would require sticky sessions |
| tantivy / DuckDB for search | oversized for a few hundred tools |
| Authentication, per-user limits | postponed until after V1 (see above) |
| Per-IP-address limit | ineffective with Claude.ai connectors |
| Global concurrency limit | couples the servers to one another without protecting the gateway |
| Upstream `sse` transport | deprecated by the spec and absent from the rmcp 3.x client |
