//! The plans of `station.desktop_tools_plan`: a static store of at most four
//! plans, bound to the deck link that asked. The maintenance channel reads a
//! plan with [`plan`] and drops it with [`remove`].

use std::sync::{Mutex, PoisonError};

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::desktop_tools::ResolvedPackage;

/// How long a plan lasts.
pub const PLAN_LIFETIME_MS: i64 = 120_000;
/// The most plans that the station keeps.
pub const MAX_PLANS: usize = 4;

/// A plan that the station keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // The fields are read by the maintenance channel (BG1-3).
pub struct StoredPlan {
    pub plan_id: String,
    /// The deck link that asked for the plan.
    pub peer: Fingerprint,
    /// The tool IDs to install.
    pub tools: Vec<String>,
    /// The `<repo>/<name>` targets, in the order of the command.
    pub targets: Vec<String>,
    /// The expected transaction of the dry run.
    pub transaction: Vec<ResolvedPackage>,
    /// The SHA-256 of `/etc/pacman.conf` at the time of the plan.
    pub conf_sha256: String,
    /// Unix milliseconds.
    pub expires_at_ms: i64,
}

/// The plans of the station: at most [`MAX_PLANS`], the oldest first.
#[derive(Debug, Default)]
pub struct PlanStore {
    plans: Mutex<Vec<StoredPlan>>,
}

impl PlanStore {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            plans: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<StoredPlan>> {
        self.plans.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores `plan`. The expired plans go first; a fifth plan removes the
    /// oldest.
    pub fn insert(&self, now_ms: i64, plan: StoredPlan) {
        let mut plans = self.lock();
        plans.retain(|p| p.expires_at_ms > now_ms);
        plans.push(plan);
        while plans.len() > MAX_PLANS {
            plans.remove(0);
        }
    }

    /// The live plan `plan_id` of the deck `peer`. The plan stays in the
    /// store; an expired plan is dropped.
    pub fn get(&self, plan_id: &str, peer: &Fingerprint, now_ms: i64) -> Option<StoredPlan> {
        let mut plans = self.lock();
        let at = plans.iter().position(|p| p.plan_id == plan_id)?;
        if plans[at].expires_at_ms <= now_ms {
            plans.remove(at);
            return None;
        }
        plans[at].peer.matches(peer).then(|| plans[at].clone())
    }

    /// Removes the plan `plan_id`.
    pub fn remove(&self, plan_id: &str) {
        self.lock().retain(|p| p.plan_id != plan_id);
    }
}

/// The plans of this process.
pub static STORE: PlanStore = PlanStore::new();

/// A live plan of the deck `peer`; the plan stays stored.
#[allow(dead_code)] // Used by the maintenance channel (BG1-3).
pub(crate) fn plan(plan_id: &str, peer: &Fingerprint, now_ms: i64) -> Option<StoredPlan> {
    STORE.get(plan_id, peer, now_ms)
}

/// Removes the plan `plan_id`.
#[allow(dead_code)] // Used by the maintenance channel (BG1-3).
pub(crate) fn remove(plan_id: &str) {
    STORE.remove(plan_id);
}
