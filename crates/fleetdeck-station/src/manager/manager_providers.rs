//! The provider list of `station.info` (M2 P4-2).

use fleetdeck_proto::link::{StationProvider, VersionStatus};
use fleetdeck_providers::AuthState;

use super::{PROBE_TIMEOUT, SessionManager};

impl SessionManager {
    /// The configured provider instances with the version and sign-in state
    /// of one probe each (`station.info`, M2 P4-2). The first call probes,
    /// each probe bounded by [`PROBE_TIMEOUT`]; later calls reuse the
    /// result for the life of the station process (DL-059). An instance
    /// without an adapter, or a failed probe, reports no version and
    /// `unknown` sign-in.
    pub async fn providers(&self) -> Vec<StationProvider> {
        self.providers
            .get_or_init(|| async {
                let mut out = Vec::new();
                let policy = self.policy.current();
                for (instance, kind) in policy.instances() {
                    let probe = match self.target.adapter(instance) {
                        Some(adapter) => tokio::time::timeout(PROBE_TIMEOUT, adapter.probe())
                            .await
                            .ok()
                            .and_then(std::result::Result::ok),
                        None => None,
                    };
                    let auth = match probe.as_ref().map(|p| &p.auth) {
                        Some(AuthState::SignedIn { .. }) => "signed_in",
                        Some(AuthState::SignedOut) => "signed_out",
                        Some(AuthState::Unknown) | None => "unknown",
                    };
                    let version = probe.and_then(|p| p.version);
                    let version_status =
                        fleetdeck_providers::version::version_status(*kind, version.as_deref());
                    if version_status == VersionStatus::Broken {
                        // Starts of this instance are refused (R8.2, DL-162).
                        self.target.mark_broken(instance, *kind);
                    }
                    if let Some(v) = &version {
                        // The coordinator handoff check (DL-219).
                        self.coordinator.record_version(*kind, v.clone());
                    }
                    out.push(StationProvider {
                        instance: instance.clone(),
                        provider: *kind,
                        version,
                        auth: auth.to_owned(),
                        capabilities: self.target.adapter(instance).map(|a| a.capabilities()),
                        version_status,
                    });
                }
                out
            })
            .await
            .clone()
    }
}
