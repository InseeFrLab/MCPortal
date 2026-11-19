# 0001. Record architecture decisions

- Status: Accepted
- Date: 2026-11-19

## Context

Design choices used to live in a *Design decisions* section of `ARCHITECTURE.md`. As the
project grows (authentication, per-user limits…), decisions get revisited, and rewriting a
section in place loses the history of what was decided, when and why it changed.

## Decision

Record each structural decision as an Architecture Decision Record in `docs/adr/`, one
file per decision, numbered `NNNN-short-title.md` and listed in `docs/adr/README.md`.

- `ARCHITECTURE.md` describes how the code works **now**; ADRs record **why**.
- An ADR is written for a structural decision: new dependency, protocol behavior,
  state/concurrency model, security posture, public contract seen by clients or the LLM, or
  reversal of an earlier ADR. Not for a routine change following existing patterns.
- Lifecycle: `Proposed` (in the PR) → `Accepted` (merged). An accepted ADR is not rewritten:
  a new ADR supersedes it and the old one is marked `Superseded by ADR-NNNN`.

## Alternatives considered

- Keep the section in `ARCHITECTURE.md`: simple, but no status or history, and every
  decision makes the file longer.
- A wiki or issues: decisions would drift away from the code they explain.

## Consequences

- Decisions are reviewed in the same pull request as the code.
- `ARCHITECTURE.md` keeps a one-line summary per decision with a link to its ADR.
