//! # MCPortal — MCP gateway
//!
//! MCPortal aggregates several "upstream" MCP servers (declared in a YAML registry) and
//! exposes them to a "downstream" MCP client (Claude, Claude Code…) as a single server.
//!
//! ```text
//!   MCP client ──HTTP──▶ ┌──────────── MCPortal ─────────────┐ ──HTTP──▶ "insee" upstream
//!   (downstream)         │ gateway.rs   (MCP handler)        │ ──HTTP──▶ "melodi" upstream
//!                        │   ├─ search.rs  (BM25)            │ ──HTTP──▶ "geodata" upstream
//!                        │   ├─ catalog.rs (resources)       │
//!                        │   └─ limits.rs  (semaphores)      │
//!                        │ upstream.rs  (upstream clients)   │
//!                        │ state.rs     (shared catalog)     │
//!                        └───────────────────────────────────┘
//! ```
//!
//! The binary (`main.rs`) only wires these modules together; all the logic lives in the
//! library so that it can be tested (`tests/`) and measured (`benches/`).
//!
//! Suggested reading order:
//! 1. [`config`]    — what the YAML registry contains;
//! 2. [`upstream`]  — how we talk to an upstream server;
//! 3. [`state`]     — the shared state and its refresh;
//! 4. [`search`]    — the BM25 index;
//! 5. [`gateway`]   — the exposed MCP server (tools + resources);
//! 6. [`catalog`]   — the content of the `catalog://…` resources;
//! 7. [`server`]    — HTTP wiring (axum), `/health`, `/ready`, graceful shutdown;
//! 8. [`telemetry`] — JSON logs and Prometheus metrics.

pub mod catalog;
pub mod config;
pub mod gateway;
pub mod guide;
pub mod limits;
pub mod search;
pub mod server;
pub mod state;
pub mod telemetry;
pub mod upstream;
