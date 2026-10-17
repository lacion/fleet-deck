//! The display side of the board WebSocket (R13 §5, §5D items 7, 16 and 17,
//! R9 Q1 and Q5 to Q7): the view of a display, its subscription
//! authorization and its timeline sends.
//!
//! - The view (snapshot and delta) is the reduced cards and stations inside
//!   the effective scope. A revoked display gets an empty view; the next
//!   recheck closes the socket.
//! - A subscription needs a session that the effective scope allows now.
//! - Every timeline send goes through the one projection of
//!   `DeckStore::display_project` / `display_timeline_after`. The deck reads
//!   the scope again right before each send and drops the subscription when
//!   it does not allow the session. A scope that changes between this check
//!   and the socket write is accepted.
//! - A frame holds the highest `station_seq` that was examined, so a dropped
//!   event is never examined again by a client that follows the cursor. A
//!   live batch whose events are all dropped sends no frame.

use std::collections::BTreeMap;

use fleetdeck_proto::EventEnvelope;
use fleetdeck_proto::board::{BoardServerMessage, DeckSession, SessionKey, StationView};

use super::{Session, WsError};
use crate::http::display_view::{display_cards, display_stations, in_scope};
use crate::local::station_view;
use crate::store::activity::display_timeline::DisplayBatch;
use crate::store::display_directives::EffectiveScope;
use crate::store::displays::DisplayLogin;
use crate::store::{DeckStore, LiveEvents, Principal};

/// What a display view holds: the stations, the cards and the scope.
type DisplayView = (Vec<StationView>, Vec<DeckSession>, Option<EffectiveScope>);

/// The view of a display: the reduced stations and cards inside its
/// effective scope, and that scope. The data is read first and the scope
/// last, right before the filter, so a scope that narrowed during the reads
/// is applied. A display that no longer exists gets an empty view.
pub(super) async fn view(store: &DeckStore, d: &DisplayLogin) -> Result<DisplayView, WsError> {
    let stations: Vec<StationView> = store
        .stations()
        .await?
        .iter()
        .map(|st| station_view(st, store))
        .collect();
    let sessions = store.sessions().await?;
    let Some(scope) = store.display_scope(d.display_id.clone()).await? else {
        return Ok((Vec::new(), Vec::new(), None));
    };
    let cards = display_cards(&sessions, &scope);
    let stations = display_stations(&stations, &scope, &cards);
    Ok((stations, cards, Some(scope)))
}

/// True when the effective scope of the display allows the session now: the
/// session exists, its station is in the scope and its repository matches a
/// repository filter.
pub(super) async fn authorized(
    store: &DeckStore,
    display_id: &str,
    k: &SessionKey,
) -> Result<bool, WsError> {
    let Some(scope) = store.display_scope(display_id.to_owned()).await? else {
        return Ok(false);
    };
    if !scope.stations.contains(&k.station_id) {
        return Ok(false);
    }
    let Some(summary) = store
        .session_summary(k.station_id.clone(), k.session_id.clone())
        .await?
    else {
        return Ok(false);
    };
    Ok(in_scope(
        &scope,
        &k.station_id,
        summary.repo.as_ref().map(|r| r.root.as_str()),
    ))
}

impl Session {
    fn display_id(&self) -> Option<String> {
        match &self.principal {
            Principal::Display(d) => Some(d.display_id.clone()),
            _ => None,
        }
    }

    /// The display looks at its effective scope: a change sends a delta,
    /// which also ends the subscriptions that left the scope.
    pub(super) async fn display_rescope(&mut self, version: u64) -> Result<(), WsError> {
        let Some(id) = self.display_id() else {
            return Ok(());
        };
        if self.board.store.display_scope(id).await? != self.scope {
            self.delta(version).await?;
        }
        Ok(())
    }

    /// Sends one projected batch of a session when the scope allows it now;
    /// else the subscription ends.
    async fn display_send(&mut self, k: &SessionKey, batch: DisplayBatch) -> Result<(), WsError> {
        let Some(id) = self.display_id() else {
            return Ok(());
        };
        let store = self.board.store.clone();
        if !authorized(&store, &id, k).await? {
            self.subs.remove(k);
            return Ok(());
        }
        let Some(high_water) = batch.high_water else {
            return Ok(());
        };
        self.send(&BoardServerMessage::DisplayTimelineEvents {
            station_id: k.station_id.clone(),
            session_id: k.session_id.clone(),
            events: batch.events,
            high_water,
        })
        .await
    }

    /// Live events for a display: the events after each cursor are projected
    /// together; a batch that keeps nothing sends no frame.
    pub(super) async fn display_live(&mut self, batch: &LiveEvents) -> Result<(), WsError> {
        let mut out: BTreeMap<SessionKey, Vec<EventEnvelope>> = BTreeMap::new();
        for e in &batch.events {
            let k = SessionKey {
                station_id: batch.station_id.clone(),
                session_id: e.session_id.clone(),
            };
            if let Some(sub) = self.subs.get_mut(&k)
                && sub.incarnation.as_ref() == Some(&batch.incarnation)
                && e.station_seq > sub.cursor
            {
                sub.cursor = e.station_seq;
                out.entry(k).or_default().push(e.clone());
            }
        }
        let store = self.board.store.clone();
        for (k, events) in out {
            let projected = store
                .display_project(k.station_id.clone(), k.session_id.clone(), events)
                .await?;
            if projected.events.is_empty() {
                continue;
            }
            self.display_send(&k, projected).await?;
        }
        Ok(())
    }

    /// The stored timeline after the cursor of a subscription, for replay
    /// and for the catch-up after a lag. A frame goes out when at least one
    /// event was examined, also when every one was dropped.
    pub(super) async fn display_stored_after(&mut self, k: &SessionKey) -> Result<(), WsError> {
        let Some(cursor) = self.subs.get(k).map(|s| s.cursor) else {
            return Ok(());
        };
        let store = self.board.store.clone();
        let batch = store
            .display_timeline_after(k.station_id.clone(), k.session_id.clone(), cursor)
            .await?;
        let Some(high) = batch.high_water else {
            return Ok(());
        };
        if let Some(sub) = self.subs.get_mut(k) {
            sub.cursor = high;
        }
        self.display_send(k, batch).await
    }
}
