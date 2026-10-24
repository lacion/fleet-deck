//! The live fleet-control policy of a caller station
//! (`coordinator.policy_now`, ADR-008 section 6.2), bound to the settle epoch
//! of that station (B26 `grants-update.md` §3.3a).
//!
//! A returning link of the caller station makes every answer that the deck
//! got before it stale: the return check of that link can drop the grant.
//! The final decision of an authority check therefore checks
//! [`DeckStore::settled_since`](crate::store::DeckStore::settled_since) with
//! the epoch of the answer, after its last local pass and without an await
//! between that check and the answer.

use std::time::Duration;

use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::{PolicyNow, PolicyNowAnswer};
use fleetdeck_proto::{SessionId, StationId};

use super::DeckLink;
use super::router::POLICY_NOW_DEADLINE;

/// A `coordinator.policy_now` answer and the settle epoch of the caller
/// station when the deck asked. The answer holds only while
/// `settled_since(station, epoch)` is true.
#[derive(Debug, Clone)]
pub(crate) struct BoundPolicy {
    pub answer: PolicyNowAnswer,
    pub epoch: u64,
}

impl DeckLink {
    /// The live fleet-control policy of the caller station (ADR-008 section
    /// 6.2): `coordinator.policy_now` with a deadline of 3 s. `None` for any
    /// error, a timeout, no live link or a link without `coordinator.v2`;
    /// the caller treats it as `fleet_control: false` (fail closed).
    pub(crate) async fn policy_now(
        &self,
        station: &StationId,
        caller_session: &SessionId,
    ) -> Option<BoundPolicy> {
        let query = PolicyNow {
            caller_session: caller_session.clone(),
            binding: None,
        };
        self.policy_now_with(station, query, POLICY_NOW_DEADLINE)
            .await
    }

    /// [`DeckLink::policy_now`] with the request `query` (R13 P6 §5.6 sends
    /// the expected binding) and the wait `timeout`. B26 §3.3a: `None` when a
    /// link of the station waits for its return check when the deck asks, or
    /// when a link of the station returned while the deck waited.
    pub(crate) async fn policy_now_with(
        &self,
        station: &StationId,
        query: PolicyNow,
        timeout: Duration,
    ) -> Option<BoundPolicy> {
        let epoch = self.store.settle_epoch(station)?;
        let control = self.control_for(station)?;
        let params = serde_json::to_value(query).ok()?;
        let outcome = tokio::time::timeout(
            timeout,
            control.request("coordinator.policy_now", None, params),
        )
        .await;
        let answer: PolicyNowAnswer = match outcome {
            Ok(Ok(Outcome::Ok(value))) => serde_json::from_value(value).ok()?,
            Ok(Ok(Outcome::Err(e))) => {
                tracing::debug!(station = %station, code = %e.code, "policy_now refused");
                return None;
            }
            Ok(Err(e)) => {
                tracing::debug!(station = %station, error = %e, "policy_now failed");
                return None;
            }
            Err(_) => {
                tracing::debug!(station = %station, "policy_now timed out");
                return None;
            }
        };
        if !self.store.settled_since(station, epoch) {
            tracing::debug!(station = %station, "policy_now answer of an earlier link");
            return None;
        }
        Some(BoundPolicy { answer, epoch })
    }
}
