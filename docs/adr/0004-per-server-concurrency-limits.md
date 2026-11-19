# 0004. Concurrency limits per server, no global limit

- Status: Accepted
- Date: 2026-11-19
- Note: Recorded retroactively: moved from the *Design decisions* section of `ARCHITECTURE.md`.

## Context

Each upstream must be protected from overload. The gateway does not know who is calling
(see ADR-0005).

## Decision

One limit per server (`max_concurrency`), acquired within `acquire_timeout_seconds`. There
is no global limit across servers.

## Alternatives considered

- **Global limit** (existed at first, removed): it did not protect the gateway (an
  in-flight call costs a tokio task and an HTTP request, and the total is already bounded
  by the sum of the servers' limits), but it **coupled the servers**: calls waiting for a
  permit on a slow or failing server held global permits, and calls to healthy servers were
  rejected as "saturated". Below the sum of the servers' limits it causes this coupling;
  above it, it never applies.

## Consequences

- A slow or failing upstream cannot starve the others.
- The `global_limit` key is still accepted in the registry (ignored, with a warning) so that
  existing configs keep loading.
- If the gateway itself ever needs protecting, it belongs on incoming requests (a
  concurrency layer on `/mcp`, or autoscaling), not on upstream calls.
