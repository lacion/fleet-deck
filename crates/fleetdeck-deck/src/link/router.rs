//! The deck command router (M2 P3-7a; G3, PLAN §8.4, ADR-004 §9.3).
//!
//! [`DeckLink::submit`] stores the envelope as `pending` **before** the
//! first send, with the station incarnation that the read model holds as
//! `target_incarnation` and `retry_until` = now + [`RETRY_WINDOW_MS`]. Then
//! the router sends the stored bytes on the station's link. When the link
//! is down, or the request ends without a response (the link dropped, the
//! request timed out, the link had too many open requests), the command
//! stays `pending`, and the router resends the same envelope with the same
//! `command_id` when a link comes up and on each tick, until `retry_until`.
//! The station's receipt store makes a resend return the first decision,
//! so a command never runs twice. The state rules are in
//! [`crate::store::commands`].
//!
//! One send per command is in flight at a time (`inflight`).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_link::methods::code;
use fleetdeck_proto::command::{CommandOrigin, DeckCommandState, RETRY_WINDOW_MS, SessionCommand};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::{StationChange, StationConfigureEnvelope};
use fleetdeck_proto::{CommandEnvelope, CommandId, Receipt, StationId};
use serde::{Deserialize, Serialize};

use super::DeckLink;
use crate::store::{DeckCommand, DeckError, NewCommand, Settle};

/// A mutating command for one station.
// `Start` grew by the issue link and the origin flag (DL-164).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// `SessionCommand` carries the author of an input (R11.5); one command is
// built for each board call, so the size difference costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum CommandKind {
    /// `session.*`.
    Session { command: SessionCommand },
    /// `station.configure`.
    Configure { change: StationChange },
}

impl CommandKind {
    /// The link method of the command.
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Session { command } => command.method(),
            Self::Configure { .. } => "station.configure",
        }
    }
}

/// The commands with a send in flight.
#[derive(Debug, Default)]
pub(crate) struct Inflight(Mutex<HashSet<CommandId>>);

struct Claim<'a> {
    set: &'a Inflight,
    id: CommandId,
}

impl Inflight {
    fn claim(&self, id: &CommandId) -> Option<Claim<'_>> {
        let mut set = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        set.insert(id.clone()).then(|| Claim {
            set: self,
            id: id.clone(),
        })
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let mut set = self.set.0.lock().unwrap_or_else(PoisonError::into_inner);
        set.remove(&self.id);
    }
}

pub(crate) fn origin_name(origin: CommandOrigin) -> &'static str {
    match origin {
        CommandOrigin::Board => "board",
        CommandOrigin::Coordinator => "coordinator",
        CommandOrigin::Mail => "mail",
        CommandOrigin::Local => "local",
    }
}

impl DeckLink {
    /// A live link of `station`.
    pub(crate) fn control_for(&self, station: &StationId) -> Option<Control> {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.get(station)
            .into_iter()
            .flatten()
            .map(|(_, c, _)| c.clone())
            .find(|c| c.ended().is_none())
    }

    /// A live link of `station` with a data channel: its control channel
    /// and its streams (a stream token is valid only on the link that
    /// issued it).
    pub(crate) fn data_for(
        &self,
        station: &StationId,
    ) -> Option<(Control, Arc<super::streams::DataHub>)> {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.get(station)
            .into_iter()
            .flatten()
            .filter(|(_, c, _)| c.ended().is_none())
            .find_map(|(_, c, d)| d.as_ref().map(|d| (c.clone(), Arc::clone(d))))
    }

    /// The stations with a live link.
    pub(crate) fn live_stations(&self) -> Vec<StationId> {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.keys().cloned().collect()
    }

    /// Builds the stored command of `kind` for `station`: a new
    /// `command_id`, the station incarnation that the read model holds as
    /// `target_incarnation`, and `retry_until` = now + [`RETRY_WINDOW_MS`].
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`]; [`DeckError::NotSynced`] when the
    /// deck has no incarnation of the station yet; a database error.
    pub(crate) async fn build(
        &self,
        station: StationId,
        origin: CommandOrigin,
        kind: CommandKind,
    ) -> Result<NewCommand, DeckError> {
        self.build_until(station, origin, kind, i64::MAX, None, None)
            .await
    }

    /// [`DeckLink::build`] with `retry_until` capped at `latest` (an outbox
    /// attempt must not outlive its item's `expires_at`).
    ///
    /// # Errors
    /// See [`DeckLink::build`].
    pub(crate) async fn build_until(
        &self,
        station: StationId,
        origin: CommandOrigin,
        kind: CommandKind,
        latest: i64,
        command_id: Option<CommandId>,
        author: Option<String>,
    ) -> Result<NewCommand, DeckError> {
        let rec = self
            .store
            .station(station.clone())
            .await?
            .ok_or_else(|| DeckError::UnknownStation(station.0.clone()))?;
        let target = rec
            .incarnation
            .ok_or_else(|| DeckError::NotSynced(rec.name.clone()))?;
        let command_id = command_id.unwrap_or_else(|| self.store.new_command_id());
        let retry_until = (self.store.now_ms() + RETRY_WINDOW_MS).min(latest);
        let (method, envelope) = match kind {
            CommandKind::Session { command } => (
                command.method().to_owned(),
                serde_json::to_value(CommandEnvelope {
                    authority: None,
                    command_id: command_id.clone(),
                    target_incarnation: Some(target),
                    retry_until,
                    origin,
                    command,
                })?,
            ),
            CommandKind::Configure { change } => (
                "station.configure".to_owned(),
                serde_json::to_value(StationConfigureEnvelope {
                    command_id: command_id.clone(),
                    target_incarnation: target,
                    retry_until,
                    change,
                })?,
            ),
        };
        Ok(NewCommand {
            command_id,
            station_id: station,
            method,
            origin: origin_name(origin).to_owned(),
            envelope,
            retry_until,
            author,
        })
    }

    /// Stores `kind` for `station` as `pending` and starts its first send.
    /// Returns the stored command; [`DeckLink::wait_command`] waits for
    /// its outcome.
    ///
    /// # Errors
    /// See [`DeckLink::build`].
    pub async fn submit(
        self: &Arc<Self>,
        station: StationId,
        origin: CommandOrigin,
        kind: CommandKind,
    ) -> Result<DeckCommand, DeckError> {
        let new = self.build(station, origin, kind).await?;
        let cmd = self.store.insert_command(new).await?;
        let ctx = Arc::clone(self);
        let id = cmd.command_id.clone();
        tokio::spawn(async move { ctx.send(&id).await });
        Ok(cmd)
    }

    /// [`DeckLink::submit`] with a command id chosen by the client: a resend
    /// after a lost response stores and sends nothing new and returns the
    /// stored command (`false`), so a board retry never duplicates a
    /// command.
    ///
    /// # Errors
    /// [`DeckError::CommandIdReused`] and the errors of
    /// [`DeckLink::submit`].
    pub async fn submit_as(
        self: &Arc<Self>,
        station: StationId,
        origin: CommandOrigin,
        kind: CommandKind,
        command_id: CommandId,
    ) -> Result<(DeckCommand, bool), DeckError> {
        self.submit_by(station, origin, kind, command_id, None)
            .await
    }

    /// [`DeckLink::submit_as`] with the author of the command (R11.5): the
    /// stored command row keeps it.
    ///
    /// # Errors
    /// See [`DeckLink::submit_as`].
    pub async fn submit_by(
        self: &Arc<Self>,
        station: StationId,
        origin: CommandOrigin,
        kind: CommandKind,
        command_id: CommandId,
        author: Option<String>,
    ) -> Result<(DeckCommand, bool), DeckError> {
        let new = self
            .build_until(station, origin, kind, i64::MAX, Some(command_id), author)
            .await?;
        let (cmd, new) = self.store.insert_or_existing(new).await?;
        if new {
            let ctx = Arc::clone(self);
            let id = cmd.command_id.clone();
            tokio::spawn(async move { ctx.send(&id).await });
        }
        Ok((cmd, new))
    }

    /// Sends the `pending` commands of `station` in their order.
    pub async fn dispatch_pending(&self, station: StationId) {
        match self.store.pending_commands(station).await {
            Ok(cmds) => {
                for cmd in cmds {
                    self.send(&cmd.command_id).await;
                }
            }
            Err(e) => tracing::warn!(error = %e, "pending commands"),
        }
    }

    /// One router tick: ends the commands past `retry_until`, moves the
    /// outbox items by their commands, starts the due outbox attempts and
    /// resends the pending commands of every reachable station.
    pub async fn tick(&self) {
        if let Err(e) = self.store.expire_commands().await {
            tracing::warn!(error = %e, "command expiry");
        }
        if let Err(e) = self.store.reconcile_outbox().await {
            tracing::warn!(error = %e, "outbox reconcile");
        }
        for station in self.live_stations() {
            self.start_outbox(station.clone()).await;
            self.dispatch_pending(station).await;
        }
    }

    /// Sends the stored envelope of `id` once, if it is `pending`, before
    /// `retry_until`, not in flight, and its station has a live link.
    pub async fn send(&self, id: &CommandId) {
        let Some(_claim) = self.inflight.claim(id) else {
            return;
        };
        let cmd = match self.store.command(id.clone()).await {
            Ok(Some(cmd)) if cmd.state == DeckCommandState::Pending => cmd,
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %e, "command lookup");
                return;
            }
        };
        let Some(control) = self.control_for(&cmd.station_id) else {
            return;
        };
        let cmd = match self.store.mark_sent(id.clone()).await {
            Ok(Some(cmd)) => cmd,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "command send count");
                return;
            }
        };
        let outcome = control
            .request(&cmd.method, Some(&cmd.command_id), cmd.envelope.clone())
            .await;
        let settle = match outcome {
            Ok(Outcome::Ok(value)) => match serde_json::from_value::<Receipt>(value) {
                Ok(r) if r.command_id == cmd.command_id => Settle::Receipt(r),
                Ok(_) | Err(_) => Settle::Refused {
                    code: code::PROTOCOL_ERROR.to_owned(),
                    message: "the station answered without a valid receipt".to_owned(),
                },
            },
            Ok(Outcome::Err(e)) if e.code == code::TOO_MANY_REQUESTS || e.code == code::TIMEOUT => {
                tracing::debug!(command = %cmd.command_id, code = %e.code, "resend later");
                return;
            }
            Ok(Outcome::Err(e)) => Settle::Refused {
                code: e.code.clone(),
                message: e.message.clone(),
            },
            Err(e) => {
                tracing::debug!(command = %cmd.command_id, error = %e, "resend on the next link");
                return;
            }
        };
        if let Err(e) = self
            .store
            .settle_command(cmd.command_id.clone(), settle)
            .await
        {
            tracing::warn!(error = %e, "command settle");
        }
    }

    /// Waits until `id` leaves `pending` or `timeout` passes; returns the
    /// command as it is then.
    ///
    /// # Errors
    /// [`DeckError::UnknownCommand`], or a database error.
    pub async fn wait_command(
        &self,
        id: CommandId,
        timeout: Duration,
    ) -> Result<DeckCommand, DeckError> {
        let mut changes = self.store.watch_commands();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let cmd = self
                .store
                .command(id.clone())
                .await?
                .ok_or_else(|| DeckError::UnknownCommand(id.0.clone()))?;
            if cmd.state != DeckCommandState::Pending {
                return Ok(cmd);
            }
            match tokio::time::timeout_at(deadline, changes.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => return Ok(cmd),
            }
        }
    }
}
