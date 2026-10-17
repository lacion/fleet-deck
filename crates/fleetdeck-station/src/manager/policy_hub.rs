//! The live policy of the station (DL-172 §1.4): one versioned snapshot of
//! the configuration that every consumer reads when it handles a request,
//! a decision or a dispatch, never once at startup.
//!
//! - [`PolicyHub::current`] gives the snapshot ([`Policy`]); a reader keeps
//!   its `Arc`, so a publish never changes a snapshot under a running
//!   decision.
//! - Applying a new `StationConfig` takes two steps, both on the
//!   [`super::SessionManager`]. `prepare` validates it and builds what must
//!   be rebuilt (the integration service, when the trackers changed); it
//!   publishes nothing. `publish` swaps the snapshot and the rebuilt
//!   service and cannot fail.
//! - The fields that apply only at a restart (`listen`, `host`,
//!   `shim_buffer_mib`, `provider_instances`, and the Fleet Git keys
//!   `clone_root`, `traits`, `allow_check_cmd`, `allow_local_remotes`,
//!   `taildrive_mounts`) are ignored: the running values stay.
//! - The caller serializes `prepare` and `publish` (the settings holder
//!   runs one save at a time).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use fleetdeck_core::config::{StationConfig, TrackerConfig};
use fleetdeck_proto::integrations::{TrackerKind, valid_tracker_id};

use super::policy::Policy;
use crate::coordinator::Bindings;
use crate::integrations::Integrations;

/// The most `[[trackers]]` entries (the limit of the config parser).
const MAX_TRACKERS: usize = 16;

/// The key and the fixed message of a config that cannot apply. The
/// message holds no value.
pub type PrepareError = (String, String);

/// The runtime state of a station that is built once and is not part of
/// the snapshot: the coordinator bindings and the integration service.
#[derive(Default)]
pub struct Holders {
    pub coordinator: Bindings,
    /// `None`: a service for the trackers of the first snapshot.
    pub integrations: Option<Integrations>,
}

/// What [`super::SessionManager::prepare`] built for
/// [`super::SessionManager::publish`].
#[derive(Debug)]
pub struct Prepared {
    pub(super) policy: Policy,
    /// The new service, when the trackers changed.
    pub(super) integrations: Option<Integrations>,
}

/// The current policy snapshot of one station, with its version.
#[derive(Debug)]
pub struct PolicyHub {
    inner: RwLock<Arc<Policy>>,
    version: AtomicU64,
    /// The coordinator bindings of the station: they hold the live
    /// `allow_fleet_control` for the intake of the coordinator tools
    /// (ADR-008 §3). Each swap of the snapshot updates it.
    fleet: Option<Bindings>,
}

impl PolicyHub {
    /// A hub whose first snapshot (version 1) is `policy`.
    #[must_use]
    pub fn new(policy: Policy) -> Self {
        Self {
            inner: RwLock::new(Arc::new(policy)),
            version: AtomicU64::new(1),
            fleet: None,
        }
    }

    /// This hub with the bindings that hold the live `allow_fleet_control`
    /// (ADR-008 §3): they get the value of the current snapshot now and of
    /// every later snapshot.
    #[must_use]
    pub fn with_fleet_control(mut self, bindings: Bindings) -> Self {
        bindings.set_fleet_control(self.current().allow_fleet_control());
        self.fleet = Some(bindings);
        self
    }

    /// The coordinator bindings of the station, when the hub has them (FG-8:
    /// the land engine checks the authority of a coordinator land).
    #[must_use]
    pub fn fleet_bindings(&self) -> Option<&Bindings> {
        self.fleet.as_ref()
    }

    /// The current snapshot.
    #[must_use]
    pub fn current(&self) -> Arc<Policy> {
        Arc::clone(&self.inner.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// The version of the current snapshot: 1 at the start, plus 1 for each
    /// publish.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Swaps in `policy` and counts the publish. The manager calls it from
    /// `publish`; the tests of other modules apply a config to a bare hub
    /// with it.
    pub(crate) fn swap(&self, policy: Policy) {
        let mut slot = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(bindings) = &self.fleet {
            bindings.set_fleet_control(policy.allow_fleet_control());
        }
        *slot = Arc::new(policy);
        self.version.fetch_add(1, Ordering::SeqCst);
    }
}

/// Validates `new`: the rules of the config parser that a config built in
/// memory can break.
///
/// # Errors
/// The key of the first value that cannot apply, and a fixed message
/// without the value.
pub(super) fn validate(new: &StationConfig) -> Result<(), PrepareError> {
    check_trackers(&new.trackers)
}

fn bad(key: String, message: &str) -> PrepareError {
    (key, message.to_owned())
}

/// The rules of the config parser for the `[[trackers]]` entries, for a
/// config that was not parsed from a file.
fn check_trackers(list: &[TrackerConfig]) -> Result<(), PrepareError> {
    if list.len() > MAX_TRACKERS {
        return Err(bad("trackers".to_owned(), "use at most 16 entries"));
    }
    for (i, t) in list.iter().enumerate() {
        let field = |k: &str| format!("trackers[{i}].{k}");
        if !valid_tracker_id(&t.id) {
            return Err(bad(
                field("id"),
                "use 1-32 of a-z, 0-9, `-`, `_`, starting with a letter or digit",
            ));
        }
        if list[..i].iter().any(|o| o.id == t.id) {
            return Err(bad(field("id"), "the id is used twice"));
        }
        match t.kind {
            TrackerKind::Jira => {
                if t.site.is_none() {
                    return Err(bad(field("site"), "a Jira tracker needs its site"));
                }
                if t.email.is_none() {
                    return Err(bad(
                        field("email"),
                        "a Jira tracker needs the account e-mail",
                    ));
                }
            }
            TrackerKind::Linear => {
                if t.site.is_some() || t.email.is_some() {
                    return Err(bad(
                        field("site"),
                        "a Linear tracker takes neither a site nor an e-mail",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A `StationConfig` for tests: the allowed root `root` and the TOML in
/// `extra` (its top-level keys come before its tables).
///
/// # Panics
/// When the text does not parse.
#[cfg(test)]
pub(crate) fn test_config(root: &std::path::Path, extra: &str) -> StationConfig {
    let text = format!("allowed_roots = [\"{}\"]\n{extra}", root.display());
    StationConfig::parse(&text, &fleetdeck_core::paths::Env::default()).unwrap()
}
