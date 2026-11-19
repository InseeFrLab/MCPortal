# 0007. Catalog as resources rather than tools

- Status: Accepted
- Date: 2026-11-19
- Note: Recorded retroactively: moved from the *Design decisions* section of `ARCHITECTURE.md`.

## Context

Clients need to discover the servers, domains and tools behind the gateway without loading
every tool definition into their context.

## Decision

Expose the catalog as MCP resources (`catalog://servers`, `catalog://domains`,
`catalog://servers/{name}`, `catalog://servers/{name}/tools`, `catalog://guide`), always
generated from the registry and the upstreams' state.

## Alternatives considered

- Catalog tools: they would cost context on every request through their definitions.
- A hand-written inventory in the guide: a second source of truth that drifts.

## Consequences

- A resource costs nothing in the client's context until it reads it, and it stays
  readable when tool search hides `tools/list`.
- The deployment guide contains narrative and pointers, never an inventory; drift tests
  check it.
