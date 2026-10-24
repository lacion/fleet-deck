//! The default modes of a running station (B27 §3.5): the mode of a start
//! that gives none, and what the station reports about it.

use fleetdeck_core::config::{
    CLAUDE_PROVIDER_DEFAULT, CODEX_PROVIDER_DEFAULT, DefaultModes, StationConfig,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::{claude, codex};

/// The default modes of a running station (B27 §3.5).
#[derive(Debug, Clone, Default)]
pub struct DefaultModeState {
    /// `[default_mode]` of the file (applied live).
    pub configured: DefaultModes,
    /// `allow_bypass_permissions` as the adapters were built at the start
    /// of the process (`adapters.rs`). `Policy::reconfigured` keeps it: the
    /// adapters change only at a restart.
    pub bypass_offered: bool,
}

/// True for a mode that needs the adapters to offer bypass: Claude
/// `bypassPermissions`, or a Codex mode with the `danger-full-access`
/// sandbox.
fn needs_bypass(kind: ProviderKind, mode: &str) -> bool {
    match kind {
        ProviderKind::Claude => mode == claude::BYPASS_MODE,
        ProviderKind::Codex => {
            codex::Mode::parse(mode).is_some_and(|m| m.sandbox == codex::FULL_ACCESS_SANDBOX)
        }
        ProviderKind::Unknown => false,
    }
}

impl DefaultModeState {
    /// The state of a process that starts with `config`.
    #[must_use]
    pub fn from_config(config: &StationConfig) -> Self {
        Self {
            configured: config.default_mode.clone(),
            bypass_offered: config.allow_bypass_permissions,
        }
    }

    /// The default that a start without a mode really gets on `kind`: the
    /// configured mode, or `None` (the provider default) when no mode is
    /// configured, or when the mode is a bypass or full-access mode and
    /// `bypass_offered` is false (the adapters do not offer it until the
    /// restart). `None` for `Unknown`.
    #[must_use]
    pub fn effective(&self, kind: ProviderKind) -> Option<&str> {
        let mode = match kind {
            ProviderKind::Claude => self.configured.claude.as_deref(),
            ProviderKind::Codex => self.configured.codex.as_deref(),
            ProviderKind::Unknown => None,
        }?;
        (self.bypass_offered || !needs_bypass(kind, mode)).then_some(mode)
    }

    /// The mode that `station.info` reports for `kind`: the effective
    /// default, or the provider default. `None` for `Unknown`.
    #[must_use]
    pub fn reported(&self, kind: ProviderKind) -> Option<&str> {
        self.effective(kind).or(match kind {
            ProviderKind::Claude => Some(CLAUDE_PROVIDER_DEFAULT),
            ProviderKind::Codex => Some(CODEX_PROVIDER_DEFAULT),
            ProviderKind::Unknown => None,
        })
    }
}
