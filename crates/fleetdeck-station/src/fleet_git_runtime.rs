//! The runtime state of Fleet Git on this station that is built once (design
//! §3.7, §9.1, §14, FG-5, FG-6, FG-11): the traits, the config digest, the
//! control of the current deck link and the confined Git options of a clone.
//! The settings stay in [`Policy`](crate::manager::Policy).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_link::control::Control;
use fleetdeck_proto::fleet_git::{ConfigDigest, StationTraits};

use crate::manager::Policy;
use crate::workspace::GitOptions;
use crate::workspace::fleet_git::OptionsProvider;

/// How long the traits stay (design §9.1: the battery is read at most every
/// 60 s).
pub const TRAITS_EVERY: Duration = Duration::from_secs(60);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The shared Fleet Git runtime of one station.
pub struct FleetGitRuntime {
    /// The traits and when they were read.
    traits: Mutex<Option<(Instant, StationTraits)>>,
    /// The newest config digest; the drift task writes it.
    config: Mutex<Option<ConfigDigest>>,
    /// The current deck link with `fleet_git.homes.v1`, with its token.
    deck: Mutex<Option<(u64, Control)>>,
    next_token: AtomicU64,
    /// The confined Git options of a clone (DL-200).
    provision: OptionsProvider,
}

impl std::fmt::Debug for FleetGitRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetGitRuntime").finish_non_exhaustive()
    }
}

impl FleetGitRuntime {
    /// A runtime whose clones use the options of `provision`.
    #[must_use]
    pub fn new(provision: OptionsProvider) -> Self {
        Self {
            traits: Mutex::new(None),
            config: Mutex::new(None),
            deck: Mutex::new(None),
            next_token: AtomicU64::new(1),
            provision,
        }
    }

    /// The traits of this station, read at most every 60 s. It may run
    /// `pmset` (macOS): call it from a blocking context.
    #[must_use]
    pub fn traits(&self, policy: &Policy) -> StationTraits {
        self.traits_at(policy, Instant::now(), crate::traits::read)
    }

    /// [`Self::traits`] with the time and the reader (tests).
    pub(crate) fn traits_at(
        &self,
        policy: &Policy,
        now: Instant,
        read: impl FnOnce(&[String]) -> StationTraits,
    ) -> StationTraits {
        let mut slot = lock(&self.traits);
        if let Some((at, traits)) = slot.as_ref()
            && now.saturating_duration_since(*at) < TRAITS_EVERY
        {
            return traits.clone();
        }
        let traits = read(policy.traits());
        *slot = Some((now, traits.clone()));
        traits
    }

    /// The newest config digest, or `None` before the first one.
    #[must_use]
    pub fn config(&self) -> Option<ConfigDigest> {
        lock(&self.config).clone()
    }

    /// Stores a config digest (the drift task).
    pub fn set_config(&self, digest: ConfigDigest) {
        *lock(&self.config) = Some(digest);
    }

    /// Stores the control of a deck link with `fleet_git.homes.v1` and returns
    /// its token. A later link replaces it.
    pub fn set_deck(&self, control: Control) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        *lock(&self.deck) = Some((token, control));
        token
    }

    /// Clears the deck control, but only when the slot still holds `token`:
    /// the end of an older link does not clear a newer one.
    pub fn clear_deck(&self, token: u64) {
        let mut slot = lock(&self.deck);
        if slot.as_ref().is_some_and(|(t, _)| *t == token) {
            *slot = None;
        }
    }

    /// The control of the current deck link, if one has the feature.
    #[must_use]
    pub fn deck(&self) -> Option<Control> {
        lock(&self.deck).as_ref().map(|(_, c)| c.clone())
    }

    /// The confined Git options of a clone, read at each use so that the
    /// allowed roots of the moment apply.
    #[must_use]
    pub fn provision_options(&self) -> GitOptions {
        (self.provision)()
    }
}
