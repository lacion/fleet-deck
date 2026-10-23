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
use fleetdeck_proto::command::{
    CommandOrigin, DeckCommandState, GrantKind, RETRY_WINDOW_MS, SessionCommand,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::{
    CommandAuthority, PolicyNow, PolicyNowAnswer, StationChange, StationConfigureEnvelope,
};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandEnvelope, CommandId, Receipt, SessionId, StationId};
use serde::{Deserialize, Serialize};

use super::DeckLink;
use crate::store::outbox::{Gate, authority_of};
use crate::store::{DeckCommand, DeckError, LinkFeature, NewCommand, Settle};

/// How long the deck waits for the caller station's `coordinator.policy_now`
/// answer (ADR-008 section 6.2).
pub const POLICY_NOW_DEADLINE: Duration = Duration::from_secs(3);

/// Held for reading by every send, and for writing by a share revocation
/// (NV-17).
static MEMBER_DISPATCH: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

/// The fence of a share revocation. A revocation holds it, so it waits for
/// every member mail that is being sent now; a later send sees the
/// revocation (NV-17).
pub(crate) async fn member_revocation_fence() -> tokio::sync::RwLockWriteGuard<'static, ()> {
    MEMBER_DISPATCH.write().await
}

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

    /// The live fleet-control policy of the caller station (ADR-008 section
    /// 6.2): `coordinator.policy_now` with a deadline of 3 s. `None` for any
    /// error, a timeout, no live link or a link without `coordinator.v2`;
    /// the caller treats it as `fleet_control: false` (fail closed).
    pub async fn policy_now(
        &self,
        station: &StationId,
        caller_session: &SessionId,
    ) -> Option<PolicyNowAnswer> {
        let query = PolicyNow {
            caller_session: caller_session.clone(),
            binding: None,
        };
        self.policy_now_with(station, query, POLICY_NOW_DEADLINE)
            .await
    }

    /// [`DeckLink::policy_now`] with the request `query` (R13 P6 §5.6 sends
    /// the expected binding) and the wait `timeout`.
    pub(crate) async fn policy_now_with(
        &self,
        station: &StationId,
        query: PolicyNow,
        timeout: Duration,
    ) -> Option<PolicyNowAnswer> {
        let control = self.control_for(station)?;
        let params = serde_json::to_value(query).ok()?;
        let outcome = tokio::time::timeout(
            timeout,
            control.request("coordinator.policy_now", None, params),
        )
        .await;
        match outcome {
            Ok(Ok(Outcome::Ok(value))) => serde_json::from_value(value).ok(),
            Ok(Ok(Outcome::Err(e))) => {
                tracing::debug!(station = %station, code = %e.code, "policy_now refused");
                None
            }
            Ok(Err(e)) => {
                tracing::debug!(station = %station, error = %e, "policy_now failed");
                None
            }
            Err(_) => {
                tracing::debug!(station = %station, "policy_now timed out");
                None
            }
        }
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
        self.build_until(station, origin, kind, i64::MAX, (None, None), None)
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
        (command_id, author): (Option<CommandId>, Option<String>),
        authority: Option<CommandAuthority>,
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
                    authority,
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
            .build_until(
                station,
                origin,
                kind,
                i64::MAX,
                (Some(command_id), author),
                None,
            )
            .await?;
        let (cmd, new) = self.store.insert_or_existing(new).await?;
        if new {
            let ctx = Arc::clone(self);
            let id = cmd.command_id.clone();
            tokio::spawn(async move { ctx.send(&id).await });
        }
        Ok((cmd, new))
    }

    /// The live `worker_grant` setting; off when the deck has no live
    /// settings (fail closed).
    pub(crate) fn worker_setting(&self) -> fleetdeck_core::config_worker_grant::WorkerGrantConfig {
        self.desktop_live.get().map_or_else(
            || fleetdeck_core::config_worker_grant::WorkerGrantConfig {
                enabled: false,
                ..Default::default()
            },
            |cell| cell.get().worker_grant.clone(),
        )
    }

    /// [`DeckLink::submit_by`] for a session start that may get a worker
    /// grant (MCP P4a §5): the store decides, stores the grant and the
    /// command in one transaction, and the first send starts when the command
    /// is new.
    ///
    /// # Errors
    /// See [`crate::store::DeckStore::submit_start_with_worker_decision`].
    pub async fn submit_start_by(
        self: &Arc<Self>,
        req: crate::store::worker_grant::WorkerStart,
    ) -> Result<(DeckCommand, bool), DeckError> {
        let out = self.store.submit_start_with_worker_decision(req).await?;
        if out.new {
            let ctx = Arc::clone(self);
            let id = out.command.command_id.clone();
            tokio::spawn(async move { ctx.send(&id).await });
        }
        Ok((out.command, out.new))
    }

    /// Starts the first send of a command that a caller stored in its own
    /// transaction (the review send, DL-179), after that transaction
    /// committed. The router's pending-command recovery sends it too if this
    /// call never runs.
    pub(crate) fn dispatch_after_commit(self: &Arc<Self>, command_id: &CommandId) {
        let ctx = Arc::clone(self);
        let id = command_id.clone();
        tokio::spawn(async move { ctx.send(&id).await });
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
        // A share revocation waits for this send to end (NV-17).
        let _fence = MEMBER_DISPATCH.read().await;
        let Some(control) = self.control_for(&cmd.station_id) else {
            return;
        };
        // A fleet-control command checks its authority before every send and
        // resend (ADR-008 section 6.2).
        if let Some((authority, target)) = authority_of(&cmd.envelope) {
            let gate = self
                .store
                .gate_authority(
                    cmd.command_id.clone(),
                    cmd.station_id.clone(),
                    authority,
                    target,
                )
                .await;
            match gate {
                Ok(Gate::Open) => {}
                Ok(Gate::Wait | Gate::Lost) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "authority gate");
                    return;
                }
            }
        }
        // A pinned import goes only to a station that verifies the pin, at
        // every send and resend (B7 PRE-PR r1).
        if pinned_import(&cmd.envelope) {
            match self
                .store
                .link_feature(&cmd.station_id, feature::IMPORT_PIN)
            {
                LinkFeature::Negotiated => {}
                LinkFeature::Down => return,
                LinkFeature::Without => {
                    let refused = Settle::Refused {
                        code: "import_pin_unsupported".to_owned(),
                        message: "This station cannot verify an import. Update it.".to_owned(),
                    };
                    if let Err(e) = self
                        .store
                        .settle_command(cmd.command_id.clone(), refused)
                        .await
                    {
                        tracing::warn!(error = %e, "command settle");
                    }
                    return;
                }
            }
        }
        // An effort command goes only to a link that negotiated
        // `session_effort.v1`, at every send and resend, so a reconnect to
        // an older station never sends a spec without its effort (B18 Phase 2
        // §9.2 item 1). A link that is down waits.
        if effort_command(&cmd.envelope) {
            match self
                .store
                .link_feature(&cmd.station_id, feature::SESSION_EFFORT)
            {
                LinkFeature::Negotiated => {}
                LinkFeature::Down => return,
                LinkFeature::Without => {
                    let refused = Settle::Refused {
                        code: EFFORT_UNSUPPORTED.to_owned(),
                        message: EFFORT_UNSUPPORTED_TEXT.to_owned(),
                    };
                    if let Err(e) = self
                        .store
                        .settle_command(cmd.command_id.clone(), refused)
                        .await
                    {
                        tracing::warn!(error = %e, "command settle");
                    }
                    return;
                }
            }
        }
        // A start with a worker grant goes only to a link that negotiated
        // `worker_grant.v1`, so no older station decodes `kind`. Otherwise it
        // waits for a compatible link or its expiry (MCP P4a §6.3).
        if worker_start(&cmd.envelope)
            && self
                .store
                .link_feature(&cmd.station_id, feature::WORKER_GRANT)
                != LinkFeature::Negotiated
        {
            return;
        }
        // A `session.coordinator_set` goes only to a link that negotiated
        // `coordinator_live.v1`; otherwise it waits for a compatible link or
        // its expiry (MCP P4b §9).
        if holds(
            &cmd.method,
            self.store
                .link_feature(&cmd.station_id, feature::COORDINATOR_LIVE),
        ) {
            return;
        }
        // A mail of a member checks its share before every send and resend
        // (NV-17).
        match self.store.gate_member_mail(cmd.command_id.clone()).await {
            Ok(Gate::Open) => {}
            Ok(Gate::Wait | Gate::Lost) => return,
            Err(e) => {
                tracing::warn!(error = %e, "member mail gate");
                return;
            }
        }
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

/// True for a stored `session.start` envelope that carries a worker grant
/// (MCP P4a §6.3).
#[must_use]
fn worker_start(envelope: &serde_json::Value) -> bool {
    serde_json::from_value::<CommandEnvelope>(envelope.clone()).is_ok_and(|env| {
        matches!(
            &env.command,
            SessionCommand::Start { spec, .. }
                if spec.coordinator.as_deref().is_some_and(|c| c.kind == GrantKind::Worker)
        )
    })
}

/// True when a command waits for a better link: a `session.coordinator_set`
/// whose station link did not negotiate `coordinator_live.v1` (MCP P4b §9).
#[must_use]
fn holds(method: &str, live: LinkFeature) -> bool {
    method == "session.coordinator_set" && live != LinkFeature::Negotiated
}

/// True for a stored `session.import` envelope that carries a `transcript`
/// pin (B7 PRE-PR r1).
#[must_use]
fn pinned_import(envelope: &serde_json::Value) -> bool {
    serde_json::from_value::<CommandEnvelope>(envelope.clone()).is_ok_and(|env| {
        matches!(
            env.command,
            SessionCommand::Import {
                transcript: Some(_),
                ..
            }
        )
    })
}

/// The refusal code of an effort command for a station without
/// `session_effort.v1` (B18 Phase 2).
pub(crate) const EFFORT_UNSUPPORTED: &str = "effort_unsupported";
/// The text of that refusal.
pub(crate) const EFFORT_UNSUPPORTED_TEXT: &str = "This station cannot set the effort. Update it.";

/// True for a command that needs `session_effort.v1`: a `session.start` or
/// `session.import` whose `spec.effort` is set, or a `session.set_effort`
/// (B18 Phase 2, design §9.2 item 1).
#[must_use]
pub(crate) fn is_effort_command(command: &SessionCommand) -> bool {
    match command {
        SessionCommand::Start { spec, .. } | SessionCommand::Import { spec, .. } => {
            spec.effort.is_some()
        }
        SessionCommand::SetEffort { .. } => true,
        _ => false,
    }
}

/// [`is_effort_command`] for a stored envelope.
#[must_use]
pub(crate) fn effort_command(envelope: &serde_json::Value) -> bool {
    serde_json::from_value::<CommandEnvelope>(envelope.clone())
        .is_ok_and(|env| is_effort_command(&env.command))
}

#[cfg(test)]
#[path = "router_effort_tests.rs"]
mod router_effort_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(transcript: Option<&str>) -> serde_json::Value {
        let pin = transcript.map(|home| serde_json::json!({"home": home, "dev": 1, "ino": 2}));
        serde_json::json!({
            "command_id": "c1",
            "target_incarnation": null,
            "retry_until": 1,
            "origin": "board",
            "command": {
                "op": "import",
                "spec": {
                    "provider_instance": "claude", "cwd": "/w", "mode": null,
                    "model": null, "title": null,
                },
                "native_session_id": "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc",
                "prompt": null,
                "transcript": pin,
            },
        })
    }

    #[test]
    fn only_an_import_with_a_pin_is_a_pinned_import() {
        assert!(pinned_import(&envelope(Some("/home/u/.claude"))));
        assert!(!pinned_import(&envelope(None)));
        let interrupt = serde_json::json!({
            "command_id": "c2",
            "target_incarnation": null,
            "retry_until": 1,
            "origin": "board",
            "command": {"op": "interrupt", "session_id": "s1"},
        });
        assert!(!pinned_import(&interrupt));
        assert!(!pinned_import(&serde_json::json!({"nonsense": true})));
    }

    #[test]
    fn a_coordinator_set_waits_for_a_link_with_coordinator_live() {
        for live in [LinkFeature::Down, LinkFeature::Without] {
            assert!(holds("session.coordinator_set", live));
            assert!(!holds("session.coordinator_revoke", live));
        }
        assert!(!holds("session.coordinator_set", LinkFeature::Negotiated));
    }

    #[tokio::test]
    async fn the_revocation_fence_waits_for_a_send_in_flight() {
        let send = MEMBER_DISPATCH.read().await;
        let blocked =
            tokio::time::timeout(Duration::from_millis(100), member_revocation_fence()).await;
        assert!(blocked.is_err(), "the fence must wait for the send");
        drop(send);
        let free = tokio::time::timeout(Duration::from_secs(5), member_revocation_fence()).await;
        assert!(free.is_ok(), "the fence opens when the send ended");
    }
}
