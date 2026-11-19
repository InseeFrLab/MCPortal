# 0008. Prometheus metrics on top of logs

- Status: Accepted
- Date: 2026-11-19
- Note: Recorded retroactively: moved from the *Design decisions* section of `ARCHITECTURE.md`.

## Context

JSON logs go to Loki, but computing latencies and error rates from logs is costly and
imprecise.

## Decision

Expose Prometheus metrics (through the `metrics` facade) on a separate port (9090) that is
not publicly exposed. All metric names are declared in `telemetry::metric`.

## Alternatives considered

- Logs only: costly and imprecise for latencies and error rates.

## Consequences

- Grafana dashboards are fed cheaply.
- No metric carries a user identifier or a value chosen by the client, to avoid a
  cardinality explosion.
