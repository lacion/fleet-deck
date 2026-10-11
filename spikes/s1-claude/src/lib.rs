//! Spike S1 (PLAN §14 Phase 0): drive Claude Code from Rust through the
//! stream-json control protocol (`claude -p --input-format stream-json
//! --output-format stream-json --permission-prompt-tool stdio`).
//!
//! Modules:
//! - [`frames`]: CLI → host frame parser (never panics; unknown frames are kept).
//! - [`control`]: host → CLI frame builders.
//! - [`adapter`]: sans-IO adapter core (commands + frames in, canonical events + writes out).

pub mod adapter;
pub mod control;
pub mod frames;
