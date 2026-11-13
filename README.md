<p align="center">
  <img src="docs/assets/MCPortal_logo.png" alt="MCPortal logo" width="200">
</p>

MCPortal aggregates several MCP servers declared in a YAML registry and exposes them to an
MCP client (Claude, Claude Code…) as **a single server**.

```text
MCP client ──HTTP──▶ MCPortal ──HTTP──▶ "insee" MCP server
                                 ├────▶ "melodi" MCP server
                                 └────▶ "geodata" MCP server
```

Rather than loading dozens of tools into the client's context, the gateway exposes three
tools:

| Tool | Purpose |
| --- | --- |
| `search_tools(query, limit?)` | full-text search (BM25) across the tools of every server; returns their name and `inputSchema` |
| `call_tool(name, arguments)` | calls an upstream tool by its full name `{server}_{tool}` |
| `send_feedback(server, author, feedback)` | records feedback in the logs (label `FEEDBACK`) |

It also exposes a **catalog as resources** (`catalog://servers`, `catalog://domains`,
`catalog://servers/{name}`, `catalog://servers/{name}/tools`, `catalog://guide`), readable
without consuming context.

## Features

- MCP protocol **2026-07-28** (sessionless), compatible with older clients.
- **Stateless** mode: several replicas without sticky sessions.
- Arguments validated against the target tool's JSON Schema before any upstream call.
- Transparent relaying of results, progress, cancellation and multi-turn exchanges
  (`input_required`).
- Global and per-server concurrency limits, with a bounded wait time.
- Upstream health probes; a failing upstream breaks neither the catalog nor the others.
- JSON logs on standard output (Loki / Grafana) and Prometheus metrics.
- `/health` (liveness), `/ready` (readiness) and graceful shutdown for Kubernetes.

## Quick start

Requirements: Rust 1.88 or newer (2024 edition). No system dependency: TLS goes through
rustls, without OpenSSL.

```bash
cargo run -- --config registry.yaml
```

| Port | Routes |
| --- | --- |
| 8000 | `/mcp` (MCP endpoint), `/health`, `/ready` |
| 9090 | `/metrics` (Prometheus) |

To plug the gateway into Claude Code:

```bash
claude mcp add --transport http mcportal http://localhost:8000/mcp
```

## Configuration

The whole configuration lives in the YAML registry (path given by `--config` or the
`MCPORTAL_CONFIG` variable). Example:

```yaml
gateway:
  name: SSP Gateway
  tool_search: {enabled: true, max_results: 5}
  catalog: {enabled: true, include_tool_inventory: true, include_guide: true, guide_path: guide-ssp.md}
  health: {enabled: true, path: /health, probe_interval_seconds: 60, failure_threshold: 2}
  metrics: {log_level: INFO, json_logs: true, prometheus: {enabled: true, port: 9090}}
  concurrency: {global_limit: 16, acquire_timeout_seconds: 5}
  http: {bind: "0.0.0.0:8000", path: /mcp, allowed_hosts: []}

servers:
  - name: mcpdiffusion            # unique name, also used as the tool prefix
    url: https://mcpdiffusion.lab.sspcloud.fr/mcp
    transport: http               # http (sse: deprecated, not supported)
    domain: diffusion
    description: Server exposing Melodi, Rmes and insee.fr
    tags: [dataset, publications] # list or single string
    enabled: true                 # false: visible in the catalog, not exposed
    timeout: 30.0                 # timeout per upstream call (s)
    max_concurrency: 4            # concurrent calls to this server
    cache_ttl: 300                # lifetime of the tool list cache (s)
```

Every key has a default value; they are all documented in
[`src/config.rs`](src/config.rs). An unknown key (typo) or an inconsistent value prevents
startup, with an explicit message.

`guide_path` points to the deployment-specific guide: it is **relative to the registry**,
checked at startup and re-read on every read of `catalog://guide`.

The `RUST_LOG` variable takes precedence over `log_level` (e.g. `RUST_LOG=mcportal=debug`).

## Deployment

```bash
docker build -t mcportal:0.1.0 .
```

A Kubernetes example (ConfigMap, Deployment, Service) is provided in
[`deploy/kubernetes.yaml`](deploy/kubernetes.yaml). Key points:

- the registry and the guide are mounted from a ConfigMap into `/config`;
- a config change is deployed through a **rolling update** (no hot reload);
- `terminationGracePeriodSeconds` must exceed the largest upstream `timeout`;
- port 9090 (`/metrics`) must not be exposed by the ingress.

## Tests and measurements

```bash
cargo test                                   # unit and integration tests (no network)
cargo test --test guide_drift -- --ignored   # guide consistency with the registry and real upstreams
cargo bench --bench overhead                 # gateway overhead
```

Integration tests start real mock MCP servers on random ports: they need no external
service.

Overhead measured locally: about **0.9 ms** per relayed call, mostly due to the extra HTTP
hop. In production, the `mcportal_gateway_overhead_seconds` metric gives the same
measurement.

## Documentation

- [Website](https://inseefrlab.github.io/MCPortal/): project landing page (source in [`website/`](website/)).
- [ARCHITECTURE.md](ARCHITECTURE.md): code layout, internals and design decisions.
- [CONTRIBUTING.md](CONTRIBUTING.md): how to contribute.
- [`src/guide.md`](src/guide.md): usage guide served to MCP clients.

## Known limitations (V1)

- No authentication and no per-user limit.
- No configuration hot reload.
- Upstream `sse` transport not supported (deprecated by the MCP spec).

The reasons for these choices are detailed in
[ARCHITECTURE.md](ARCHITECTURE.md#design-decisions).

## Contributing

Contributions are welcome: bug reports, feature ideas, documentation or code.

1. Open an issue to describe the bug or discuss the change before starting.
2. Create a branch from `main` (`feat/…`, `fix/…`, `docs/…`).
3. Check that everything passes:

   ```bash
   cargo fmt --all --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

4. Open a pull request against `main`.

The details (code conventions, commit messages, tests, checklist) are in
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

Distributed under the [MIT](LICENSE) license.
