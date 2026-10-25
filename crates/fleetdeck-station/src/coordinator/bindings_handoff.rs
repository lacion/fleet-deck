//! The credential handoff check and the probed provider versions of the
//! bindings, moved out of `bindings.rs`; no behaviour change.

use fleetdeck_proto::event::ProviderKind;

#[cfg(test)]
use super::HandoffCheck;

impl super::Bindings {
    /// Whether the credential handoff is verified for a provider version:
    /// `Err` with the capability reason (`handoff_unverified`).
    ///
    /// # Errors
    /// The capability reason.
    pub fn handoff(
        &self,
        provider: ProviderKind,
        version: Option<&str>,
    ) -> Result<(), &'static str> {
        let check = self
            .handoff
            .unwrap_or(fleetdeck_providers::coordinator::handoff);
        if let Some(version) = version {
            return check(provider, Some(version));
        }
        let probed = self
            .versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&provider)
            .cloned()
            .unwrap_or_default();
        if probed.is_empty() {
            return check(provider, None);
        }
        probed.iter().try_for_each(|v| check(provider, Some(v)))
    }

    /// Records the probed version of one provider instance (DL-219).
    pub fn record_version(&self, provider: ProviderKind, version: String) {
        let mut map = self
            .versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let list = map.entry(provider).or_default();
        if !list.contains(&version) {
            list.push(version);
        }
    }

    /// Replaces the probed versions of one provider kind (B28: the report
    /// follows an install).
    pub fn replace_versions(&self, provider: ProviderKind, versions: Vec<String>) {
        self.versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider, versions);
    }

    /// The probed versions of one provider kind, in the order they were
    /// recorded (empty before the first probe).
    #[must_use]
    pub fn versions(&self, provider: ProviderKind) -> Vec<String> {
        self.versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&provider)
            .cloned()
            .unwrap_or_default()
    }

    /// The same bindings with another handoff check (station tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_handoff(mut self, check: HandoffCheck) -> Self {
        self.handoff = Some(check);
        self
    }
}
