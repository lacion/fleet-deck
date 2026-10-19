//! The memory of the stop of a lost run (BG-4, design
//! `docs/v2/design/board-gaps.md` §5.3 rules 1 and 9):
//!
//! - [`SessionLocks`] serializes the stop plan, the `StopExecution`
//!   decision and dispatch, the `Resume` and `ReconcileExecution` decision,
//!   and the `Resume` (import) dispatch of one session.
//! - [`PlanStore`] holds the stop plans with their pidfds: 30 seconds, at
//!   most [`MAX_PLANS`] in the station, one for each session. A new plan of
//!   a session replaces its old one. A restart of the station loses them.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleetdeck_proto::SessionId;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// How long a plan lives.
pub const PLAN_TTL: Duration = Duration::from_secs(30);
/// The most plans in the whole station.
pub const MAX_PLANS: usize = 8;
/// The most targets of one plan.
pub const MAX_TARGETS: usize = 128;

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One async mutex for each session.
#[derive(Default)]
pub struct SessionLocks {
    map: Mutex<HashMap<SessionId, Arc<AsyncMutex<()>>>>,
}

impl SessionLocks {
    /// Waits for the lock of `session` and holds it until the guard drops.
    pub async fn hold(&self, session: &SessionId) -> OwnedMutexGuard<()> {
        let lock = {
            let mut map = locked(&self.map);
            // A lock that nobody holds or waits for is dropped.
            map.retain(|_, l| Arc::strong_count(l) > 1);
            Arc::clone(map.entry(session.clone()).or_default())
        };
        lock.lock_owned().await
    }
}

/// One target of a plan: a process that the station can prove belongs to
/// the run, and the pidfd that signals exactly that process.
#[derive(Debug)]
pub struct PlanTarget {
    pub pid: u32,
    pub start_time: u64,
    pub fd: OwnedFd,
}

/// A stored stop plan.
#[derive(Debug)]
pub struct StopPlan {
    pub plan_id: String,
    pub session_id: SessionId,
    pub shim_id: String,
    /// The domain number: the PID of the leader of the run.
    pub leader: u32,
    pub targets: Vec<PlanTarget>,
    /// Every live member of the domain when the plan was made, as
    /// `(pid, start_time)`. A member that is not in it at the dispatch is a
    /// new member.
    pub domain: Vec<(u32, u64)>,
    /// Unix ms, for the answer.
    pub expires_at: i64,
    expires: Instant,
}

impl StopPlan {
    /// A plan that expires [`PLAN_TTL`] after `now`.
    #[must_use]
    pub fn new(
        plan_id: String,
        (session_id, shim_id, leader): (SessionId, String, u32),
        targets: Vec<PlanTarget>,
        domain: Vec<(u32, u64)>,
        (now, now_ms): (Instant, i64),
    ) -> Self {
        Self {
            plan_id,
            session_id,
            shim_id,
            leader,
            targets,
            domain,
            expires_at: now_ms.saturating_add(i64::try_from(PLAN_TTL.as_millis()).unwrap_or(0)),
            expires: now + PLAN_TTL,
        }
    }

    fn live(&self, now: Instant) -> bool {
        now < self.expires
    }
}

/// The store is full: eight plans of other sessions are alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreFull;

/// The stop plans of the station.
#[derive(Default)]
pub struct PlanStore {
    plans: Mutex<HashMap<SessionId, StopPlan>>,
}

impl PlanStore {
    /// Stores `plan`; the old plan of the same session goes. Expired plans
    /// go first.
    ///
    /// # Errors
    /// [`StoreFull`] when [`MAX_PLANS`] plans of other sessions are alive.
    pub fn put(&self, plan: StopPlan, now: Instant) -> Result<(), StoreFull> {
        let mut plans = locked(&self.plans);
        plans.retain(|_, p| p.live(now));
        plans.remove(&plan.session_id);
        if plans.len() >= MAX_PLANS {
            return Err(StoreFull);
        }
        plans.insert(plan.session_id.clone(), plan);
        Ok(())
    }

    /// True when `session` has a live plan `plan_id` for `shim_id`.
    #[must_use]
    pub fn valid(&self, session: &SessionId, shim_id: &str, plan_id: &str, now: Instant) -> bool {
        locked(&self.plans)
            .get(session)
            .is_some_and(|p| p.live(now) && p.plan_id == plan_id && p.shim_id == shim_id)
    }

    /// Takes the plan out of the store. `None` when it is gone, expired or
    /// another plan replaced it. An expired plan is dropped.
    #[must_use]
    pub fn take(&self, session: &SessionId, plan_id: &str, now: Instant) -> Option<StopPlan> {
        let mut plans = locked(&self.plans);
        if plans.get(session).is_some_and(|p| p.plan_id == plan_id) {
            return plans.remove(session).filter(|p| p.live(now));
        }
        None
    }

    /// The number of stored plans (tests).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        locked(&self.plans).len()
    }

    /// Changes the recorded start time of a target: the PID then looks
    /// reused (tests only).
    #[cfg(all(test, target_os = "linux"))]
    pub fn tamper_start_time(&self, session: &SessionId, pid: u32, start_time: u64) {
        let mut plans = locked(&self.plans);
        let plan = plans.get_mut(session).expect("a stored plan");
        let target = plan
            .targets
            .iter_mut()
            .find(|t| t.pid == pid)
            .expect("a target");
        target.start_time = start_time;
    }
}

/// The locks and the plans of a station.
#[derive(Default)]
pub struct StopState {
    pub locks: SessionLocks,
    pub plans: PlanStore,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn plan(session: &str, id: &str, now: Instant) -> StopPlan {
        StopPlan::new(
            id.to_owned(),
            (SessionId::from(session.to_owned()), "shim".to_owned(), 7),
            Vec::new(),
            Vec::new(),
            (now, 1_000),
        )
    }

    #[test]
    fn a_plan_expires_after_thirty_seconds() {
        let store = PlanStore::default();
        let t0 = Instant::now();
        store.put(plan("s1", "p1", t0), t0).unwrap();
        let s1 = SessionId::from("s1".to_owned());
        assert!(store.valid(&s1, "shim", "p1", t0 + Duration::from_secs(29)));
        assert!(!store.valid(&s1, "shim", "p1", t0 + Duration::from_secs(30)));
        assert!(!store.valid(&s1, "other", "p1", t0));
        assert!(!store.valid(&s1, "shim", "p2", t0));
        assert!(
            store
                .take(&s1, "p1", t0 + Duration::from_secs(31))
                .is_none()
        );
        assert_eq!(store.len(), 0, "an expired plan is dropped by the take");
    }

    #[test]
    fn the_store_keeps_eight_plans_and_one_for_each_session() {
        let store = PlanStore::default();
        let t0 = Instant::now();
        for n in 0..MAX_PLANS {
            store.put(plan(&format!("s{n}"), "p", t0), t0).unwrap();
        }
        assert_eq!(store.put(plan("extra", "p", t0), t0), Err(StoreFull));
        // A new plan of a session replaces its old one, also when full.
        store.put(plan("s0", "p-new", t0), t0).unwrap();
        assert_eq!(store.len(), MAX_PLANS);
        let s0 = SessionId::from("s0".to_owned());
        assert!(!store.valid(&s0, "shim", "p", t0));
        assert!(store.valid(&s0, "shim", "p-new", t0));
        // Expired plans make room.
        let later = t0 + PLAN_TTL;
        store.put(plan("extra", "p", later), later).unwrap();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_take_removes_the_plan_once() {
        let store = PlanStore::default();
        let t0 = Instant::now();
        store.put(plan("s1", "p1", t0), t0).unwrap();
        let s1 = SessionId::from("s1".to_owned());
        assert!(store.take(&s1, "p2", t0).is_none());
        assert!(store.take(&s1, "p1", t0).is_some());
        assert!(store.take(&s1, "p1", t0).is_none());
    }

    #[tokio::test]
    async fn the_lock_of_a_session_is_exclusive_and_the_others_are_free() {
        let locks = SessionLocks::default();
        let a = SessionId::from("a".to_owned());
        let b = SessionId::from("b".to_owned());
        let held = locks.hold(&a).await;
        let _other = locks.hold(&b).await;
        let waiting = tokio::time::timeout(Duration::from_millis(50), locks.hold(&a)).await;
        assert!(waiting.is_err());
        drop(held);
        let _again = locks.hold(&a).await;
    }
}
