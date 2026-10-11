//! Fleet Deck v2 provider adapters (PLAN §6.2): the [`ProviderAdapter`] and
//! [`LiveSession`] traits, one adapter per provider (Claude per ADR-001,
//! Codex per ADR-002), and the [`host::SessionHost`] boundary that starts
//! their processes (PLAN §6.1, A9).

pub mod adapter;
pub mod claude;
pub mod codex;
pub mod host;
pub mod rawlog;
mod stdio;

pub use adapter::{
    Answer, AuthState, ControlId, Emitted, EventSink, HostInput, LiveSession, ModelId,
    NativeSessionId, PermissionMode, ProviderAdapter, ProviderError, ProviderProbe,
};
