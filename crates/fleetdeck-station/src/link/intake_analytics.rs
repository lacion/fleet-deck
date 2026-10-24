//! `station.analytics` (B29): one page of the usage hour buckets and the Fleet
//! Deck counters. Error codes `invalid_params`, `busy` and `internal`. The
//! deck sets `audience` and `sessions` (it is the authority on members); the
//! station checks the params again.

use fleetdeck_proto::analytics::AnalyticsQuery;
use fleetdeck_proto::history::code;
use fleetdeck_proto::link::{Outcome, WireError};

use super::StationLink;
use super::ok;
use crate::analytics::ScanStatus;
use crate::analytics::query::{Limits, answer_with};
use crate::store::Store;

/// `station.analytics`.
pub(super) async fn analytics(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    serve(ctx.manager.store(), &ScanStatus::shared(), params).await
}

/// The method on a store and a scan status.
pub(super) async fn serve(
    store: &Store,
    status: &ScanStatus,
    params: serde_json::Value,
) -> Outcome {
    let query: AnalyticsQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => {
            return Outcome::Err(WireError::new(code::INVALID_PARAMS, format!("params: {e}")));
        }
    };
    match answer_with(store, status, query, Limits::default()).await {
        Ok(answer) => ok(answer),
        Err(e) => Outcome::Err(WireError::new(e.code, e.message)),
    }
}

#[cfg(test)]
#[path = "intake_analytics_tests.rs"]
mod tests;
