# Contributing to MCPortal

Thank you for your interest in the project. This document explains how to set up the
environment, propose a change and get it accepted.

Before changing the code, read [ARCHITECTURE.md](ARCHITECTURE.md): it describes the modules
and the principles to follow.

## Reporting a problem or suggesting an idea

Open an issue stating:

- **for a bug**: the version (or commit), the registry used (without secrets), what you did,
  what you expected, what happened, and the relevant logs;
- **for a feature**: the need and the use case, before the envisioned solution.

For a significant change, discuss it in an issue **before** writing code: this avoids
working on a direction that would not be accepted.

Do not disclose a security vulnerability in a public issue: contact the maintainers
directly.

## Setting up the environment

- Rust 1.88 or newer (`rustup update stable`), with `rustfmt` and `clippy`.
- No system dependency is required (TLS via rustls).

```bash
git clone <repository-url>
cd MCPortal
cargo build
cargo test
```

To run the gateway locally: `cargo run -- --config registry.yaml`.

## Proposing a change

1. Create a branch from `main`, named after its purpose:
   `feat/per-user-limit`, `fix/reconnect-timeout`, `docs/geodata-guide`.
2. Make small, coherent commits.
3. Check locally that everything passes (see below).
4. Open a pull request against `main` and fill in the checklist.

### Commit messages

The project follows [Conventional Commits](https://www.conventionalcommits.org/):

```text
<type>(<optional scope>): <summary in the imperative mood>

<optional body: the why, not the how>
```

Common types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `chore`, `ci`.
Examples:

```text
feat(search): index parameter descriptions
fix(upstream): reconnect after an expired session
docs: document the gateway.http key
```

### Checks before the pull request

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If you touch the call path (`gateway.rs`, `upstream.rs`, `limits.rs`, `state.rs`), also run
the benchmark and report any regression in the pull request:

```bash
cargo bench --bench overhead
```

If you change `registry.yaml` or `guide-ssp.md`, run the guide drift tests (they query the
real upstream servers):

```bash
cargo test --test guide_drift -- --ignored
```

### Pull request checklist

- [ ] The checks above pass.
- [ ] The new or fixed behavior is covered by a test.
- [ ] Comments and documentation are up to date (README, ARCHITECTURE, `src/config.rs`).
- [ ] A new configuration key has a default value, validation if needed, and appears in the
      README.
- [ ] A new metric is declared in `telemetry::metric` and listed in ARCHITECTURE.

## Code conventions

### Style

- Formatting by `rustfmt` (default configuration), no `clippy` warnings.
- Comments and messages in **English**.
- Every module starts with a `//!` comment explaining its role and its choices; public items
  have a `///` comment. Comment the **why** when it is not obvious, not what the code
  already says.
- The `///` comments of tool argument structs (`SearchToolsArgs`…) become the descriptions
  seen by the LLM: write them for it.

### Principles

- **Simplicity**: prefer the simplest solution that meets the need; an abstraction must be
  justified by a real use.
- **Nothing blocking** on the request path; no `await` while holding a `std::sync::Mutex`.
- **No second source of truth**: the catalog and the instructions are generated from the
  registry and the upstreams, never written by hand.
- **Errors**: what the LLM can fix becomes a `CallToolResult` with `isError: true` and an
  actionable message; JSON-RPC errors are reserved for malformed requests.
- **Upstream results relayed as is**: do not re-wrap `content` or `structuredContent`.
- **Metrics**: never a user identifier or an unbounded value as a label.
- **Dependencies**: any new dependency must be justified in the pull request.

### Tests

- Unit tests in the relevant module (`#[cfg(test)] mod tests`).
- End-to-end tests in `tests/`, using the helpers in `tests/common/mod.rs`:
  `spawn_upstream` (mock MCP server), `spawn_gateway`, `connect`, `call_via_gateway`.
  For a new upstream behavior, add a tool to `MockUpstream`.
- Tests do not use the external network; those that need it are marked
  `#[ignore = "reason"]`.
- Tests use random ports (`127.0.0.1:0`): they can run in parallel.

## Common tasks

### Adding a configuration key

1. Add the field to the relevant struct in `src/config.rs`, with
   `#[serde(default = …)]` and a `///` comment.
2. Update the struct's `Default` implementation.
3. Add validation in `Config::validate` if the value can be inconsistent.
4. Add a unit test in `config.rs`.
5. Document the key in the README.

### Changing the guide

- `src/guide.md` (mechanics) must cite **no** server or tool of a deployment; the
  `mechanics_guide_cites_no_deployment_server` test checks it.
- `guide-ssp.md` (deployment) contains narrative and pointers, never an inventory: the
  inventory is generated by the catalog.

## License

The project is distributed under the [MIT](LICENSE) license. By submitting a contribution,
you agree that it will be distributed under the same license.
