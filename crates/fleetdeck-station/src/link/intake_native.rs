//! `station.native_sessions` and `station.native_preview` (B7): the native
//! sessions of the station's provider homes and the preview of one. Error
//! codes `invalid_params`, `busy` and `internal`. Both are served to the
//! owner login by the deck; the station checks the params again.

use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::native::{NativePreviewQuery, NativeSessionsQuery};

use super::StationLink;
use super::{bad_params, ok};
use crate::history::browse::{self, Budget};
use crate::history::browse_preview;

fn failed(e: crate::history::HistoryError) -> Outcome {
    Outcome::Err(WireError::new(e.code, e.message))
}

/// `station.native_sessions`.
pub(super) async fn sessions(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    let query: NativeSessionsQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let policy = ctx.manager.policy();
    let private = ctx.manager.private_paths();
    let store = ctx.manager.store();
    match browse::native_sessions(store, &policy, &private, query, Budget::default()).await {
        Ok(list) => ok(list),
        Err(e) => failed(e),
    }
}

/// `station.native_preview`.
pub(super) async fn preview(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    let query: NativePreviewQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let policy = ctx.manager.policy();
    let private = ctx.manager.private_paths();
    let store = ctx.manager.store();
    match browse_preview::native_preview(store, &policy, &private, query, Budget::default()).await {
        Ok(preview) => ok(preview),
        Err(e) => failed(e),
    }
}
