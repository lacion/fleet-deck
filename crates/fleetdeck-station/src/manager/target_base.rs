//! The base commit and the start failure of a session (R1.1): small writes
//! of the station into the log that the effect target makes around a start.

use std::sync::Arc;

use fleetdeck_proto::command::StopMode;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::mutation::LaunchMode;
use fleetdeck_proto::session::SessionBase;
use fleetdeck_proto::{Event, ProviderInstanceId, SessionId};
use fleetdeck_providers::{Answer, ControlId, ModelId, PermissionMode};

use super::{Live, SessionTarget, classify};
use crate::manager::durable::host_input;
use crate::manager::fleet_control::{self, Refusal};
#[cfg(test)]
use crate::manager::turns::SessionTurns;
use crate::store::{Dispatch, Effect, EffectPayload, NewEvent};

/// Why the starts of an instance are refused (B28 §3.8).
pub(super) enum BrokenMark {
    /// The probed version is on the deny list (R8.2, DL-162). A refresh sets
    /// and clears it.
    Denied(&'static str),
    /// A failed update or an unfinished recovery (B28 §3.8.1). Only
    /// `release_quarantine` clears it.
    Quarantined(Arc<str>),
}

impl SessionTarget {
    /// Refuses the starts of `instance`: its probed version is on the deny
    /// list of its adapter (R8.2, DL-162).
    pub fn mark_broken(&self, instance: &ProviderInstanceId, kind: ProviderKind) {
        self.set_denied(instance, kind, true);
    }

    /// Sets or clears the deny mark of `instance`. A clear removes only a
    /// `Denied` mark, and a set never replaces a `Quarantined` mark.
    pub fn set_denied(&self, instance: &ProviderInstanceId, kind: ProviderKind, denied: bool) {
        let name: &'static str = match kind {
            ProviderKind::Claude => "claude",
            ProviderKind::Codex => "codex",
            ProviderKind::Unknown => "provider",
        };
        let Ok(mut broken) = self.broken.lock() else {
            return;
        };
        if denied {
            if !matches!(broken.get(instance), Some(BrokenMark::Quarantined(_))) {
                broken.insert(instance.clone(), BrokenMark::Denied(name));
            }
        } else if matches!(broken.get(instance), Some(BrokenMark::Denied(_))) {
            broken.remove(instance);
        }
    }

    /// Quarantines every instance of `kind`: its starts are refused with
    /// `text`. It replaces a `Denied` mark.
    pub fn quarantine(&self, kind: ProviderKind, text: &str) {
        let Ok(mut broken) = self.broken.lock() else {
            return;
        };
        let text: Arc<str> = Arc::from(text);
        for (instance, adapter) in &self.adapters {
            if adapter.kind() == kind {
                broken.insert(instance.clone(), BrokenMark::Quarantined(Arc::clone(&text)));
            }
        }
    }

    /// Removes the `Quarantined` marks of every instance of `kind`. A
    /// `Denied` mark stays, and the next refresh sets it again when it
    /// applies.
    pub fn release_quarantine(&self, kind: ProviderKind) {
        let Ok(mut broken) = self.broken.lock() else {
            return;
        };
        broken.retain(|instance, mark| {
            let of_kind = self
                .adapters
                .get(instance)
                .is_some_and(|a| a.kind() == kind);
            !(of_kind && matches!(mark, BrokenMark::Quarantined(_)))
        });
    }

    /// The refusal text of a start on a broken instance, if it is one.
    pub(in crate::manager) fn broken_reason(
        &self,
        instance: &ProviderInstanceId,
    ) -> Option<String> {
        let broken = self.broken.lock().ok()?;
        broken.get(instance).map(|mark| match mark {
            BrokenMark::Denied(name) => {
                format!("this {name} version is known to break the protocol")
            }
            BrokenMark::Quarantined(text) => text.to_string(),
        })
    }

    /// The refusal text of a start on an instance whose provider kind is
    /// under an update (B28 §3.7.1), if it is.
    pub(in crate::manager) fn updating_reason(
        &self,
        instance: &ProviderInstanceId,
    ) -> Option<String> {
        let kind = self.adapters.get(instance)?.kind();
        if self.restart.updating() != Some(kind) {
            return None;
        }
        let name = match kind {
            ProviderKind::Claude => "claude",
            ProviderKind::Codex => "codex",
            ProviderKind::Unknown => "the provider",
        };
        Some(format!(
            "the station was updating {name}; start the session again"
        ))
    }

    /// True when the session has a base already, or when the summary cannot
    /// be read (no base is then recorded).
    pub(super) async fn has_base(&self, session_id: &SessionId) -> bool {
        match self.store.summary(session_id.clone()).await {
            Ok(summary) => summary.is_some_and(|s| s.base.is_some()),
            Err(e) => {
                tracing::warn!(error = %e, "the summary is unreadable; no base is recorded");
                true
            }
        }
    }

    /// Appends `session.base` (R1.1). The station emits it, not the provider.
    pub(super) async fn record_base(&self, session_id: &SessionId, base: Option<SessionBase>) {
        let Some(base) = base else { return };
        let event = Event::SessionBase {
            git_ref: base.git_ref,
            oid: base.oid,
            inferred: base.inferred,
        };
        if let Err(e) = self
            .store
            .append(session_id.clone(), vec![NewEvent::from(event)])
            .await
        {
            tracing::warn!(error = %e, "the session base was not stored");
        }
    }

    /// The dispatch recheck of an effect that carries the authority of a
    /// fleet-control command (ADR-008 §6.2): `None` to go on, or the refusal
    /// to settle the effect with. The caller runs it after every awaited
    /// preparation and fence, immediately before the provider write or
    /// launch, and writes with no await after a `None`. A refused effect
    /// writes an `error` event (`authority_lost`, `authority_unknown`); a
    /// start or a resume that is refused ends the session as a failed
    /// start. Nothing is retried.
    pub(super) async fn gate_now(&self, effect: &Effect) -> Option<Dispatch> {
        self.gate(effect, true).await
    }

    /// The same gate for the first input of a start or a resume, right
    /// before its write: the process is live, so a refusal ends no session.
    /// It sends the launch mode of the effect like every gate of a launching
    /// effect.
    pub(super) async fn gate_input(&self, effect: &Effect) -> Option<Dispatch> {
        self.gate(effect, false).await
    }

    async fn gate(&self, effect: &Effect, launch_point: bool) -> Option<Dispatch> {
        effect.payload.authority()?;
        let result = match self.launch_mode(effect).await {
            // The live setting is read again after the round trip.
            Ok(launch) => {
                fleet_control::recheck(&self.coordinator, &|| self.live_switch(), effect, launch)
                    .await
            }
            Err(refusal) => Err(refusal),
        };
        let Err(refusal) = result else {
            return None;
        };
        self.authority_refused(effect, refusal, launch_point).await;
        Some(Dispatch::NotDispatched(refusal.code().into()))
    }

    /// The turn checkpoints of a live session, to hold its workspace fence
    /// (tests only).
    #[cfg(test)]
    pub(in crate::manager) async fn turns_of(
        &self,
        session_id: &SessionId,
    ) -> Option<Arc<SessionTurns>> {
        let live = self.live(session_id)?;
        let turns = Arc::clone(&live.lock().await.turns);
        Some(turns)
    }

    /// The live setting `allow_fleet_control`: the snapshot of the policy
    /// hub, else the value that the bindings hold.
    fn live_switch(&self) -> bool {
        self.current_policy().map_or_else(
            || self.coordinator.fleet_control(),
            |policy| policy.allow_fleet_control(),
        )
    }

    /// The provider launch or the mode change that the effect makes (ADR-008
    /// §5.2): the provider kind and the mode that it really uses. Every
    /// gate of a start, a resume, an import or a set_mode sends it, the
    /// first-input gate of a start too. `None` for any other effect. An
    /// unknown provider fails closed.
    async fn launch_mode(&self, effect: &Effect) -> Result<Option<LaunchMode>, Refusal> {
        let (provider, mode) = match &effect.payload {
            EffectPayload::Start { spec, .. } | EffectPayload::Resume { spec, .. } => (
                self.adapters.get(&spec.provider_instance).map(|a| a.kind()),
                spec.mode.clone(),
            ),
            // (FG-9) A moved session resumes with no mode of its own.
            EffectPayload::MoveIn {
                provider_instance, ..
            } => (self.adapters.get(provider_instance).map(|a| a.kind()), None),
            EffectPayload::SetMode { mode, .. } => {
                let summary = match self.store.summary(effect.session_id.clone()).await {
                    Ok(summary) => summary,
                    Err(e) => {
                        tracing::warn!(error = %e, "the summary is unreadable; no launch mode");
                        return Err(Refusal::Unknown);
                    }
                };
                (summary.and_then(|s| s.provider), Some(mode.clone()))
            }
            _ => return Ok(None),
        };
        match provider {
            Some(provider @ (ProviderKind::Claude | ProviderKind::Codex)) => {
                Ok(Some(LaunchMode { provider, mode }))
            }
            Some(ProviderKind::Unknown) | None => Err(Refusal::Unknown),
        }
    }

    async fn authority_refused(&self, effect: &Effect, refusal: Refusal, ends_session: bool) {
        let mut events = vec![NewEvent::from(Event::Error {
            code: refusal.code().into(),
            message: refusal.message().into(),
            retry_at: None,
        })];
        if ends_session
            && matches!(
                effect.payload,
                EffectPayload::Start { .. } | EffectPayload::Resume { .. }
            )
        {
            events.push(NewEvent::from(Event::SessionEnded {
                reason: "start_failed".into(),
                exit_code: None,
            }));
        }
        if let Err(e) = self.store.append(effect.session_id.clone(), events).await {
            tracing::error!(error = %e, "the authority refusal was not stored");
        }
    }

    /// A start that never got a process ends the session (`exited`).
    pub(super) async fn start_failed(&self, session_id: &SessionId, message: &str) {
        let events = vec![
            NewEvent::from(Event::Error {
                code: "start_failed".into(),
                message: message.to_owned(),
                retry_at: None,
            }),
            NewEvent::from(Event::SessionEnded {
                reason: "start_failed".into(),
                exit_code: None,
            }),
        ];
        if let Err(e) = self.store.append(session_id.clone(), events).await {
            tracing::error!(error = %e, "start failure not stored");
        }
    }
}

/// True for an input effect: it runs its own gate after the workspace fence
/// and the checkpoint work.
pub(super) fn gates_itself(payload: &EffectPayload) -> bool {
    matches!(
        payload,
        EffectPayload::Prompt { .. } | EffectPayload::FollowUp { .. } | EffectPayload::Steer { .. }
    )
}

impl SessionTarget {
    /// One claimed effect of a session with a direct provider process. The
    /// authority gate of each write is the last step before it: inputs run
    /// it after the fence and the start checkpoint, and nothing awaits
    /// between the gate and the provider call.
    pub(super) async fn operate_direct(&self, effect: &Effect, live: &mut Live) -> Dispatch {
        if let EffectPayload::Stop { how, .. } = &effect.payload {
            let Some(mut session) = live.session.take() else {
                return Dispatch::NotDispatched("already_stopping".into());
            };
            let how: StopMode = *how;
            // A graceful stop can take the whole grace period; the worker
            // goes on. `session.ended` confirms the stop.
            tokio::spawn(async move {
                if let Err(e) = session.stop(how).await {
                    tracing::warn!(error = %e, "stop");
                }
            });
            return Dispatch::Sent;
        }
        let turns = Arc::clone(&live.turns);
        let process = live.process.clone();
        let Some(session) = live.session.as_mut() else {
            return Dispatch::NotDispatched("stopping".into());
        };
        let sent = match &effect.payload {
            EffectPayload::Prompt { input, .. } | EffectPayload::FollowUp { input, .. } => {
                // The start checkpoint is published before the write, under
                // the workspace fence (PLAN §6.6).
                let fence = turns.fence().await;
                turns.begin_input(&fence, &input.input_id).await;
                if let Some(refused) = self.gate_now(effect).await {
                    turns.input_not_sent(&input.input_id).await;
                    return refused;
                }
                turns.input_sent(&input.input_id);
                let sent = if matches!(effect.payload, EffectPayload::Prompt { .. }) {
                    session.prompt(host_input(input)).await
                } else {
                    session.follow_up(host_input(input)).await
                };
                if let Err(e) = &sent
                    && matches!(classify(e), Dispatch::NotDispatched(_))
                {
                    turns.input_not_sent(&input.input_id).await;
                }
                sent
            }
            EffectPayload::Steer { input, .. } => {
                // A due completion capture runs before the steer can write.
                let _fence = turns.fence().await;
                if let Some(refused) = self.gate_now(effect).await {
                    return refused;
                }
                session.steer(host_input(input)).await
            }
            EffectPayload::Interrupt { .. } => {
                session
                    .interrupt(Some(ControlId(effect.effect_id.as_str().to_owned())))
                    .await
            }
            EffectPayload::Answer {
                request_id,
                decision,
                by,
                ..
            } => {
                session
                    .answer(Answer {
                        request: request_id.clone(),
                        decision: decision.clone(),
                        by: *by,
                    })
                    .await
            }
            EffectPayload::SetMode { mode, .. } => {
                session.set_mode(PermissionMode(mode.clone())).await
            }
            EffectPayload::SetModel { model } => session.set_model(ModelId(model.clone())).await,
            EffectPayload::SetEffort { effort } => {
                session
                    .set_effort(
                        effort.clone(),
                        Some(ControlId(effect.effect_id.as_str().to_owned())),
                    )
                    .await
            }
            EffectPayload::Revert { revert_id, .. } => {
                let env = crate::manager::revert_run::RevertEnv {
                    store: &self.store,
                    git: &self.git,
                    limits: self.limits,
                    private: &self.private,
                };
                return crate::manager::revert_run::run(
                    env,
                    &turns,
                    session.as_mut(),
                    &process,
                    revert_id,
                )
                .await;
            }
            EffectPayload::Start { .. }
            | EffectPayload::Resume { .. }
            | EffectPayload::Worktree { .. }
            | EffectPayload::Land { .. }
            | EffectPayload::MoveIn { .. }
            | EffectPayload::StopExecution { .. }
            | EffectPayload::Stop { .. } => {
                return Dispatch::NotDispatched("not_a_live_operation".into());
            }
            // B8: a Codex `!` command goes to the provider; the station has
            // checked it already (`dispatch_codex_shell`).
            EffectPayload::ShellCommand {
                command,
                timeout_ms,
                ..
            } => {
                session
                    .shell_command(effect.effect_id.to_string(), command.clone(), *timeout_ms)
                    .await
            }
        };
        match sent {
            Ok(()) => Dispatch::Sent,
            Err(e) => classify(&e),
        }
    }
}
