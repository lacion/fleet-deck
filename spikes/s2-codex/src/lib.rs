//! Phase 0 spike S2: drive `codex app-server` (JSON-RPC over stdio) from Rust.
//!
//! - [`rpc`]: JSON-RPC framing (parse, never panics; host frame builders).
//! - [`adapter`]: sans-IO adapter core (host commands + frames in, canonical
//!   events + writes out).
//!
//! See `docs/v2/adr/ADR-002-native-codex-adapter.md`.

pub mod adapter;
pub mod rpc;
