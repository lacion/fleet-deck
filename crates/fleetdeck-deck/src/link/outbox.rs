//! Outbox delivery on the deck link (M2 P3-7b; A5). The store rules are in
//! [`crate::store::outbox`]: [`DeckLink::enqueue`] stores the intent; the
//! router tick reconciles the items and, for each reachable station,
//! starts the due attempts ([`DeckLink::start_outbox`]). An attempt is one
//! routed command, inserted with the item's move to `sending` in one
//! transaction and then sent like every other routed command. Its
//! `retry_until` is capped at the item's `expires_at`, so the router never
//! sends or resends expired intent.

use std::time::Duration;

use fleetdeck_proto::StationId;
use fleetdeck_proto::command::CommandOrigin;

use super::DeckLink;
use super::router::{CommandKind, origin_name};
use crate::store::{DeckError, NewOutboxItem, OutboxItem};

impl DeckLink {
    /// Queues `kind` for `station` as deck-owned intent that stays valid
    /// for `ttl`. It is delivered when the station is reachable.
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], or a database error.
    pub async fn enqueue(
        &self,
        station: StationId,
        origin: CommandOrigin,
        kind: CommandKind,
        ttl: Duration,
    ) -> Result<OutboxItem, DeckError> {
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        let item = NewOutboxItem {
            station_id: station,
            origin: origin_name(origin).to_owned(),
            payload: serde_json::to_value(&kind)?,
            expires_at: self.store.now_ms().saturating_add(ttl_ms),
        };
        self.store.enqueue(item).await
    }

    /// Starts one attempt of each due item of `station`. The router sends
    /// the attempt's command right after (`dispatch_pending`).
    pub async fn start_outbox(&self, station: StationId) {
        let due = match self.store.due_outbox(station.clone()).await {
            Ok(due) => due,
            Err(e) => {
                tracing::warn!(error = %e, "outbox due items");
                return;
            }
        };
        for item in due {
            if let Err(e) = self.attempt(&item).await {
                tracing::warn!(outbox = %item.outbox_id, error = %e, "outbox attempt");
            }
        }
    }

    async fn attempt(&self, item: &OutboxItem) -> Result<(), DeckError> {
        let kind: CommandKind = serde_json::from_value(item.payload.clone())?;
        let origin = match item.origin.as_str() {
            "mail" => CommandOrigin::Mail,
            "coordinator" => CommandOrigin::Coordinator,
            _ => CommandOrigin::Board,
        };
        // The attempt ends no later than the item: nothing is sent, or
        // resent, after `expires_at`.
        let cmd = self
            .build_until(
                item.station_id.clone(),
                origin,
                kind,
                item.expires_at,
                (None, None),
                item.authority.clone(),
            )
            .await?;
        self.store
            .start_attempt(item.outbox_id.clone(), cmd)
            .await?;
        Ok(())
    }
}
