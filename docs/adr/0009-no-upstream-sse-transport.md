# 0009. Upstream `sse` transport not supported

- Status: Accepted
- Date: 2026-11-19
- Note: Recorded retroactively: moved from the *Design decisions* section of `ARCHITECTURE.md`.

## Context

The old HTTP+SSE transport is deprecated by the MCP spec and no longer exists in the rmcp
3.x client.

## Decision

Only Streamable HTTP is supported upstream. The `sse` value is still accepted in the
registry so as not to break an existing config, but the server is reported `unavailable`
with an explicit message.

## Alternatives considered

- Implementing the deprecated transport ourselves: maintenance cost for a transport that
  is being phased out.

## Consequences

- Existing registries keep loading; affected servers are visible as unavailable.
