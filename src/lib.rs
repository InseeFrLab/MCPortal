//! # MCPortal — passerelle MCP
//!
//! MCPortal agrège plusieurs serveurs MCP « amont » (déclarés dans un registre YAML) et les
//! expose à un client MCP « aval » (Claude, Claude Code…) sous la forme d'un seul serveur.
//!
//! ```text
//!   client MCP ──HTTP──▶ ┌──────────── MCPortal ─────────────┐ ──HTTP──▶ amont « insee »
//!   (aval)               │ gateway.rs   (handler MCP)        │ ──HTTP──▶ amont « melodi »
//!                        │   ├─ search.rs  (BM25)            │ ──HTTP──▶ amont « geodata »
//!                        │   ├─ catalog.rs (ressources)      │
//!                        │   └─ limits.rs  (sémaphores)      │
//!                        │ upstream.rs  (clients amont)      │
//!                        │ state.rs     (catalogue partagé)  │
//!                        └───────────────────────────────────┘
//! ```
//!
//! Le binaire (`main.rs`) ne fait qu'assembler ces modules ; toute la logique vit dans la
//! bibliothèque pour être testable (`tests/`) et mesurable (`benches/`).
//!
//! Ordre de lecture conseillé :
//! 1. [`config`]    — ce que contient le registre YAML ;
//! 2. [`upstream`]  — comment on parle à un serveur amont ;
//! 3. [`state`]     — l'état partagé et son rafraîchissement ;
//! 4. [`search`]    — l'index BM25 ;
//! 5. [`gateway`]   — le serveur MCP exposé (outils + ressources) ;
//! 6. [`catalog`]   — le contenu des ressources `catalog://…` ;
//! 7. [`server`]    — l'assemblage HTTP (axum), `/health`, `/ready`, arrêt propre ;
//! 8. [`telemetry`] — logs JSON et métriques Prometheus.

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
