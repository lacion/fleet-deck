//! The station-info refresh of a live link (DL-193), moved out of `mod.rs`
//! for the module size rule. Behavior is unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::{Outcome, StationInfo};
use serde_json::json;

use super::{DeckLink, fleet_git_sync};
use crate::store::{DeckError, DeckStore};

/// How often the deck asks a live link for `station.info` again (DL-193).
pub(super) const INFO_REFRESH: Duration = Duration::from_secs(60);

/// Asks the station for `station.info` when the link comes up and then every
/// `INFO_REFRESH` while it is live, and keeps the answer for the board
/// (version, capacity, providers, resources). The board name follows the
/// station's own `name` setting: the deck adopts the name of the first
/// answer only.
pub(super) fn fetch_info(
    ctx: &Arc<DeckLink>,
    control: &Control,
    id: &StationId,
    name: &str,
    token: u64,
) {
    let (ctx, control, id, stored) = (
        Arc::clone(ctx),
        control.clone(),
        id.clone(),
        name.to_owned(),
    );
    tokio::spawn(async move {
        // The first answer starts the Fleet Git snapshot (design §3.3).
        let announced = Arc::new(AtomicBool::new(false));
        let ask = || {
            let (ctx, control, id, announced) = (
                Arc::clone(&ctx),
                control.clone(),
                id.clone(),
                Arc::clone(&announced),
            );
            async move {
                let answer = ask_info(&control).await;
                // G6 §3.2: the stream of this link, before the board gets
                // the answer; recorded only while the link lives.
                if let Ok(Some(info)) = &answer
                    && let Err(e) = ctx
                        .store
                        .record_link_stream(
                            id.clone(),
                            token,
                            (info.incarnation.clone(), info.head),
                        )
                        .await
                {
                    tracing::warn!(error = %e, "link stream");
                }
                // B26 section 3.3: the return check; its commit opens the
                // return gate of this link.
                if let Ok(Some(info)) = &answer {
                    match ctx
                        .store
                        .resolve_grant_holds(
                            id.clone(),
                            token,
                            info.incarnation.clone(),
                            info.version.clone(),
                        )
                        .await
                    {
                        // The grant work that the router held goes out now.
                        Ok(r) if r.settled => {
                            let (router, station) = (Arc::clone(&ctx), id.clone());
                            tokio::spawn(async move { router.dispatch_pending(station).await });
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(
                            station = %id,
                            error = %e,
                            "grant return check failed: no grant work on this link until the next answer"
                        ),
                    }
                }
                if matches!(answer, Ok(Some(_))) && !announced.swap(true, Ordering::Relaxed) {
                    tokio::spawn(async move { fleet_git_sync::sync_station(&ctx, id).await });
                }
                answer
            }
        };
        info_loop(
            &ctx.store,
            &id,
            &stored,
            INFO_REFRESH,
            ask,
            control.closed(),
        )
        .await;
    });
}

/// The loop of [`fetch_info`]. It ends when `closed` resolves or `ask`
/// fails. An answer that is `Ok(None)` is skipped and tried again later.
pub(super) async fn info_loop<F, Fut>(
    store: &DeckStore,
    id: &StationId,
    stored: &str,
    every: Duration,
    mut ask: F,
    closed: impl Future,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<StationInfo>, ()>>,
{
    tokio::pin!(closed);
    match ask().await {
        Ok(Some(info)) => {
            let reported = info.name.clone();
            store.record_station_info(id, info).await;
            adopt_name(store, id.clone(), stored, &reported).await;
        }
        Ok(None) => {}
        Err(()) => return,
    }
    loop {
        tokio::select! {
            _ = &mut closed => return,
            () = tokio::time::sleep(every) => {}
        }
        match ask().await {
            Ok(Some(info)) => store.record_station_info(id, info).await,
            Ok(None) => {}
            Err(()) => return,
        }
    }
}

/// One `station.info` request. `Err` when the link is gone; `Ok(None)` when
/// the answer is an error or does not parse.
async fn ask_info(control: &Control) -> Result<Option<StationInfo>, ()> {
    match control.request("station.info", None, json!({})).await {
        Ok(Outcome::Ok(v)) => match serde_json::from_value::<StationInfo>(v) {
            Ok(info) => Ok(Some(info)),
            Err(e) => {
                tracing::debug!(error = %e, "station.info does not parse");
                Ok(None)
            }
        },
        Ok(other) => {
            tracing::debug!(outcome = ?other, "station.info refused");
            Ok(None)
        }
        Err(e) => {
            tracing::debug!(error = %e, "station.info on a link that ended");
            Err(())
        }
    }
}

/// Gives the station the name that it reports, when the name is new and
/// free. A name that another station has leaves the old name.
pub(super) async fn adopt_name(store: &DeckStore, id: StationId, stored: &str, reported: &str) {
    let reported = reported.trim();
    if reported.is_empty() || reported == stored {
        return;
    }
    match store.rename_station(id.clone(), reported.to_owned()).await {
        Ok(()) => tracing::info!(station = %id, name = %reported, "station named"),
        Err(DeckError::NameTaken(_)) => tracing::warn!(
            station = %id,
            name = %reported,
            "another station has this name; the board keeps the old name"
        ),
        Err(e) => tracing::warn!(error = %e, station = %id, "station keeps its name"),
    }
}
