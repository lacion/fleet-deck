//! Fleet Deck v2 provider adapters (PLAN §6.2): the `ProviderAdapter` and
//! `LiveSession` traits and one adapter per provider (Claude per ADR-001,
//! Codex per ADR-002), and the [`host::SessionHost`] boundary that starts
//! their processes (PLAN §6.1, A9).

pub mod host;
