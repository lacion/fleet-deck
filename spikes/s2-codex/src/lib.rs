//! Phase 0 spike S2: drive `codex app-server` (JSON-RPC over stdio) from Rust.
//!
//! - [`rpc`]: JSON-RPC framing (parse, never panics; host frame builders).
//! - [`adapter`]: sans-IO adapter core (host commands + frames in, canonical
//!   events + writes out).
//! - [`driver`]: live `codex app-server` process around the core (recorder only).
//! - [`record`]: `record-codex` scenarios and fixture scrubbing.
//! - [`replay`]: offline replay of a fixture through the core (`replay-codex`).
//!
//! See `docs/v2/adr/ADR-002-native-codex-adapter.md`.

pub mod adapter;
pub mod driver;
pub mod record;
pub mod replay;
pub mod rpc;
