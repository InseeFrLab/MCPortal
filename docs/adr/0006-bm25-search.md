# 0006. Tool search with the `bm25` crate

- Status: Accepted
- Date: 2026-11-19
- Note: Recorded retroactively: moved from the *Design decisions* section of `ARCHITECTURE.md`.

## Context

`search_tools` needs full-text search over the tools of every upstream. There are at most a
few hundred tools.

## Decision

Use the `bm25` crate with a French tokenizer, behind the `ToolIndex` trait. The index is
rebuilt with the catalog, in memory.

## Alternatives considered

- tantivy: a full engine, oversized for a few hundred tools.
- DuckDB and its full-text extension: an embedded database, oversized as well.

## Consequences

- The index is rebuilt in a few milliseconds and fits in memory.
- The module depends only on `rmcp::model::Tool`, so it can be extracted (separate crate or
  contribution to the rmcp SDK, to be discussed with its maintainers first).
