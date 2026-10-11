//! Spike S1 (PLAN §14 Phase 0): drive Claude Code from Rust through the
//! stream-json control protocol (`claude -p --input-format stream-json
//! --output-format stream-json --permission-prompt-tool stdio`).
//!
//! Modules:
//! - [`frames`]: CLI → host frame parser (never panics; unknown frames are kept).
//! - [`control`]: host → CLI frame builders.
//! - [`adapter`]: sans-IO adapter core (commands + frames in, canonical events + writes out).
//! - [`driver`]: live `claude` process around the core (recorder and smoke tests only).
//! - [`record`]: scripted live scenarios that write fixtures (`record-claude`).
//! - [`replay`]: offline replay of our own fixtures (exact host writes).
//! - [`reference`]: offline replay of TypeScript SDK fixtures (semantic host writes).

pub mod adapter;
pub mod control;
pub mod driver;
pub mod frames;
pub mod record;
pub mod reference;
pub mod replay;
