# 0003. No configuration hot reload

- Status: Accepted
- Date: 2026-11-19
- Note: Recorded retroactively: moved from the *Design decisions* section of `ARCHITECTURE.md`.

## Context

The goal was to avoid outages when the config changes on Kubernetes.

## Decision

A configuration change is deployed through a **rolling update**. With a sessionless
protocol (ADR-0002), it disconnects no one:

1. the ConfigMap changes; a `checksum/config` annotation (Helm) or the Reloader tool
   triggers the rolling update;
2. the new pod starts and only reports ready after the first tool load;
3. the old pod receives SIGTERM, reports not ready and finishes its in-flight calls.

What makes this possible is the graceful shutdown, together with a `preStop` of a few
seconds on the Kubernetes side and a `terminationGracePeriodSeconds` greater than the
largest upstream `timeout`.

## Alternatives considered

- Hot reload: moderate cost (about 200 lines), but with pitfalls: semaphores resized while
  permits are held, non-reloadable settings (listening port), reliable detection of
  ConfigMap updates (atomic replacement of a symbolic link).

## Consequences

- Simpler code, no partially applied configuration.
- If the need comes back, the catalog's `ArcSwap` already provides the swap mechanism: what
  would remain is watching the file and diffing the upstreams.
