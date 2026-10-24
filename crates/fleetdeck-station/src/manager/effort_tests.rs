//! The effort command and its evidence at the station (B18 Phase 2, design
//! `docs/v2/design/effort.md` §9.2 items 2, 4 and 5): `valid_effort` at the
//! decision, `effort.chosen` in the start decision, the set-effort effect
//! and the provider results that settle it, and the choice that a resume
//! passes on. No provider process runs: the tests append the provider's
//! events themselves.

use std::collections::BTreeMap;

use fleetdeck_proto::capability::Capabilities;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
};
use fleetdeck_proto::event::{EffortSetOutcome, ProviderKind, ProviderRefs};
use fleetdeck_proto::{CommandId, Event, EventBody, RejectCode, SessionId};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

use super::Policy;
use super::decide;
use crate::coordinator::Bindings;
use crate::store::{
    Caller, Decided, Dispatch, Effect, EffectPayload, EffectState, OfflineApprovalPolicy,
    ReadResult, Store, StoreConfig,
};

const NATIVE: &str = "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc";
const PROCESS: &str = "proc-1";

fn from(value: Value) -> SessionCommand {
    serde_json::from_value(value).unwrap()
}

struct Rig {
    det: Det,
    store: Store,
    policy: Policy,
    root: tempfile::TempDir,
    next: u32,
}

impl Rig {
    async fn new() -> Self {
        let det = Det::new();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        let policy = Policy::new(
            vec![root.path().join("projects")],
            8,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        );
        Self {
            det,
            store,
            policy,
            root,
            next: 0,
        }
    }

    fn spec(&self, effort: Option<&str>) -> Value {
        let cwd = self.root.path().join("projects/app");
        json!({
            "provider_instance": "claude", "cwd": cwd.to_string_lossy(),
            "mode": null, "model": null, "title": null, "effort": effort,
        })
    }

    fn start(&self, effort: Option<&str>) -> SessionCommand {
        from(json!({"op": "start", "spec": self.spec(effort), "prompt": null}))
    }

    fn import(&self, effort: Option<&str>) -> SessionCommand {
        from(json!({
            "op": "import", "spec": self.spec(effort),
            "native_session_id": NATIVE, "prompt": null,
        }))
    }

    /// Decides `command` under the command id `id` (a redelivery uses the
    /// same id again).
    async fn decide_as(&self, id: &str, command: SessionCommand) -> Decided {
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(id),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command: command.clone(),
        };
        let policy = self.policy.clone();
        self.store
            .decide(envelope, Caller::local(), move |d| {
                decide::decide(
                    d,
                    &policy,
                    &Bindings::default(),
                    CommandOrigin::Local,
                    false,
                    command,
                )
            })
            .await
            .unwrap()
    }

    async fn send(&mut self, command: SessionCommand) -> Decided {
        self.next += 1;
        let id = format!("cmd-{}", self.next);
        self.decide_as(&id, command).await
    }

    async fn accepted(&mut self, command: SessionCommand) -> Decided {
        let d = self.send(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d
    }

    async fn rejected(&mut self, command: SessionCommand) -> (RejectCode, String) {
        let d = self.send(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
        let reject = d.receipt.reject.expect("a rejection");
        (reject.code, reject.message)
    }

    /// A session with a live process `proc-1`.
    async fn live(&mut self, effort: Option<&str>) -> SessionId {
        let d = self.accepted(self.start(effort)).await;
        let session = d.receipt.result.unwrap().session_id.unwrap();
        self.store
            .process_started(session.clone(), PROCESS.into(), Some(42))
            .await
            .unwrap();
        session
    }

    async fn set_effort(&mut self, session: &SessionId, effort: &str) -> Effect {
        let d = self
            .accepted(from(json!({
                "op": "set_effort", "session_id": session, "effort": effort,
            })))
            .await;
        d.effects.into_iter().next().expect("a set-effort effect")
    }

    /// Claims and sends every queued effect.
    async fn send_all(&self) {
        let fence = self.store.register_worker().await.unwrap();
        while let Some(effect) = self.store.claim_next(fence).await.unwrap() {
            self.store
                .settle(effect.effect_id, fence, Dispatch::Sent)
                .await
                .unwrap();
        }
    }

    async fn emit(&self, session: &SessionId, event: Event) {
        self.store
            .append(session.clone(), vec![event.into()])
            .await
            .unwrap();
    }

    async fn effect(&self, effect: &Effect) -> Effect {
        self.store
            .effect(effect.effect_id.clone())
            .await
            .unwrap()
            .unwrap()
    }

    async fn summary(&self, session: &SessionId) -> fleetdeck_proto::SessionSummary {
        self.store.summary(session.clone()).await.unwrap().unwrap()
    }

    async fn events(&self) -> Vec<Event> {
        match self.store.read_after(0, 10_000).await.unwrap() {
            ReadResult::Events { events, .. } => events
                .into_iter()
                .filter_map(|e| match e.event {
                    EventBody::Known(event) => Some(event),
                    EventBody::Unknown { .. } => None,
                })
                .collect(),
            ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
        }
    }

    async fn chosen(&self) -> Vec<String> {
        self.events()
            .await
            .into_iter()
            .filter_map(|e| match e {
                Event::EffortChosen { effort } => Some(effort),
                _ => None,
            })
            .collect()
    }

    /// The provider's report that the session starts and can resume, then
    /// the end of the session: it can be resumed.
    async fn ended(&self, session: &SessionId) {
        let mut refs = ProviderRefs::new();
        refs.insert("session_id".to_owned(), Value::from(NATIVE));
        let mut caps = Capabilities::minimal();
        caps.resume = true;
        caps.set_effort = true;
        let cwd = self.summary(session).await.cwd.unwrap();
        self.emit(
            session,
            Event::SessionStarted {
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                provider_version: None,
                model: None,
                cwd,
                mode: None,
                capabilities: Box::new(caps),
                resumed: None,
                provider_refs: refs,
                title: None,
                branch: None,
                repo: None,
            },
        )
        .await;
        self.store
            .process_ended(PROCESS.into(), "exit".to_owned())
            .await
            .unwrap();
        self.emit(
            session,
            Event::SessionEnded {
                reason: "exit".to_owned(),
                exit_code: Some(0),
            },
        )
        .await;
    }

    /// The effort of the spec of the resume effect that a `session.resume`
    /// queues.
    async fn resume_effort(&mut self, session: &SessionId) -> Option<String> {
        let d = self
            .accepted(SessionCommand::Resume {
                session_id: session.clone(),
            })
            .await;
        let resume = d
            .effects
            .into_iter()
            .find_map(|e| match e.payload {
                EffectPayload::Resume { spec, .. } => Some(spec),
                _ => None,
            })
            .expect("a resume effect");
        resume.effort
    }
}

fn result(effect: &Effect, outcome: EffortSetOutcome) -> Event {
    Event::EffortSetResult {
        control_id: Some(effect.effect_id.as_str().to_owned()),
        requested: "low".to_owned(),
        effort: None,
        outcome,
        message: None,
    }
}

#[tokio::test]
async fn an_invalid_effort_is_rejected_by_start_import_and_set_effort() {
    let mut rig = Rig::new().await;
    for bad in ["bad value", "UPPER", "seventeen-chars-xx", "a;b"] {
        let (code, message) = rig.rejected(rig.start(Some(bad))).await;
        assert_eq!(code, RejectCode::InvalidCommand, "start {bad}");
        assert_eq!(message, "use 1-16 of a-z, 0-9, `-`, `_`");
        let (code, _) = rig.rejected(rig.import(Some(bad))).await;
        assert_eq!(code, RejectCode::InvalidCommand, "import {bad}");
    }
    let session = rig.live(None).await;
    for bad in ["bad value", "UPPER", ""] {
        let (code, message) = rig
            .rejected(from(json!({
                "op": "set_effort", "session_id": session, "effort": bad,
            })))
            .await;
        assert_eq!(code, RejectCode::InvalidCommand, "set {bad:?}");
        assert_eq!(message, "use 1-16 of a-z, 0-9, `-`, `_`");
    }
    // Nothing was stored for the refused commands.
    assert!(rig.chosen().await.is_empty());
    assert_eq!(rig.store.session_effects(session).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_start_with_an_effort_appends_effort_chosen_once() {
    let mut rig = Rig::new().await;
    let plain = rig.accepted(rig.start(None)).await;
    let plain = plain.receipt.result.unwrap().session_id.unwrap();
    assert!(rig.chosen().await.is_empty());
    assert_eq!(rig.summary(&plain).await.effort_choice, None);

    let first = rig.decide_as("start-high", rig.start(Some("high"))).await;
    assert_eq!(
        first.receipt.state,
        ReceiptState::Accepted,
        "{:?}",
        first.receipt
    );
    let session = first.receipt.result.unwrap().session_id.unwrap();
    assert_eq!(rig.chosen().await, ["high"]);
    assert_eq!(
        rig.summary(&session).await.effort_choice.as_deref(),
        Some("high")
    );
    assert_eq!(rig.summary(&session).await.effort, None, "no observation");

    // A redelivered start decides nothing again.
    let again = rig.decide_as("start-high", rig.start(Some("high"))).await;
    assert!(again.duplicate);
    assert_eq!(rig.chosen().await, ["high"]);

    // An import carries its choice the same way. It needs a station where no
    // run without a known conversation id may still run (an import waits for
    // those), so it gets a rig of its own.
    let mut rig = Rig::new().await;
    let imported = rig.accepted(rig.import(Some("low"))).await;
    let imported = imported.receipt.result.unwrap().session_id.unwrap();
    assert_eq!(rig.chosen().await, ["low"]);
    assert_eq!(
        rig.summary(&imported).await.effort_choice.as_deref(),
        Some("low")
    );
}

#[tokio::test]
async fn a_set_effort_queues_an_effect_for_the_live_process() {
    let mut rig = Rig::new().await;
    let session = rig.live(None).await;
    let effect = rig.set_effort(&session, "low").await;
    assert_eq!(
        effect.payload,
        EffectPayload::SetEffort {
            effort: "low".to_owned()
        }
    );
    assert_eq!(effect.payload.kind(), "set_effort");
    assert_eq!(effect.state, EffectState::Queued);
    assert_eq!(effect.process_incarnation, Some(PROCESS.into()));
    // The command alone changes neither the choice nor the observed value.
    let summary = rig.summary(&session).await;
    assert_eq!((summary.effort_choice, summary.effort), (None, None));
}

#[tokio::test]
async fn a_set_effort_to_a_provider_without_the_capability_is_refused() {
    let mut rig = Rig::new().await;
    let session = rig.live(None).await;
    let mut caps = Capabilities::minimal();
    caps.set_effort = false;
    rig.emit(
        &session,
        Event::SessionStarted {
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            provider_version: None,
            model: None,
            cwd: "/w".to_owned(),
            mode: None,
            capabilities: Box::new(caps),
            resumed: None,
            provider_refs: ProviderRefs::new(),
            title: None,
            branch: None,
            repo: None,
        },
    )
    .await;
    let (code, _) = rig
        .rejected(from(json!({
            "op": "set_effort", "session_id": session, "effort": "low",
        })))
        .await;
    assert_eq!(code, RejectCode::Unsupported);
}

#[tokio::test]
async fn the_effect_settles_only_on_a_result_with_its_own_effect_id() {
    let mut rig = Rig::new().await;
    let session = rig.live(None).await;
    let effect = rig.set_effort(&session, "low").await;
    rig.send_all().await;
    assert_eq!(rig.effect(&effect).await.state, EffectState::Claimed);

    rig.emit(
        &session,
        Event::EffortChanged {
            effort: Some("low".to_owned()),
        },
    )
    .await;
    for control_id in [None, Some("effect-of-another-command".to_owned())] {
        rig.emit(
            &session,
            Event::EffortSetResult {
                control_id,
                requested: "low".to_owned(),
                effort: None,
                outcome: EffortSetOutcome::Accepted,
                message: None,
            },
        )
        .await;
    }
    assert_eq!(
        rig.effect(&effect).await.state,
        EffectState::Claimed,
        "an observation, a result without an id and another effect's result settle nothing"
    );
    rig.emit(&session, result(&effect, EffortSetOutcome::Accepted))
        .await;
    assert_eq!(rig.effect(&effect).await.state, EffectState::Confirmed);
}

#[tokio::test]
async fn each_outcome_settles_the_effect_as_the_design_says() {
    for (outcome, state, reason) in [
        (EffortSetOutcome::Accepted, EffectState::Confirmed, None),
        (EffortSetOutcome::Applied, EffectState::Confirmed, None),
        (EffortSetOutcome::Adjusted, EffectState::Confirmed, None),
        (EffortSetOutcome::Unverified, EffectState::Confirmed, None),
        (
            EffortSetOutcome::Refused,
            EffectState::RetiredNotDispatched,
            Some("refused_by_provider"),
        ),
        (
            EffortSetOutcome::Superseded,
            EffectState::RetiredNotDispatched,
            Some("superseded_by_a_newer_choice"),
        ),
    ] {
        let mut rig = Rig::new().await;
        let session = rig.live(None).await;
        let effect = rig.set_effort(&session, "low").await;
        rig.send_all().await;
        rig.emit(&session, result(&effect, outcome)).await;
        let settled = rig.effect(&effect).await;
        assert_eq!(settled.state, state, "{outcome:?}");
        assert_eq!(settled.reason.as_deref(), reason, "{outcome:?}");
    }
}

#[tokio::test]
async fn a_verification_result_after_the_acceptance_leaves_the_effect_confirmed() {
    let mut rig = Rig::new().await;
    let session = rig.live(None).await;
    let effect = rig.set_effort(&session, "low").await;
    rig.send_all().await;
    rig.emit(&session, result(&effect, EffortSetOutcome::Accepted))
        .await;
    for outcome in [
        EffortSetOutcome::Applied,
        EffortSetOutcome::Adjusted,
        EffortSetOutcome::Unverified,
        EffortSetOutcome::Refused,
        EffortSetOutcome::Superseded,
    ] {
        rig.emit(&session, result(&effect, outcome)).await;
        let settled = rig.effect(&effect).await;
        assert_eq!(settled.state, EffectState::Confirmed, "{outcome:?}");
        assert_eq!(settled.reason, None, "{outcome:?}");
    }
}

#[tokio::test]
async fn an_accepted_choice_survives_an_exit_before_verification() {
    let mut rig = Rig::new().await;
    let session = rig.live(Some("high")).await;
    assert_eq!(rig.chosen().await, ["high"]);
    let effect = rig.set_effort(&session, "low").await;
    rig.send_all().await;
    // The provider's acknowledgement, then the process ends before any
    // verifying read.
    rig.emit(&session, result(&effect, EffortSetOutcome::Accepted))
        .await;
    rig.ended(&session).await;

    assert_eq!(rig.effect(&effect).await.state, EffectState::Confirmed);
    let summary = rig.summary(&session).await;
    assert_eq!(summary.effort_choice.as_deref(), Some("low"));
    assert_eq!(
        summary.effort, None,
        "no effort.changed was appended for low"
    );
    assert_eq!(rig.resume_effort(&session).await.as_deref(), Some("low"));
}

#[tokio::test]
async fn a_change_without_an_acceptance_resumes_with_the_previous_choice() {
    let mut rig = Rig::new().await;
    let session = rig.live(Some("high")).await;
    let effect = rig.set_effort(&session, "low").await;
    rig.send_all().await;
    rig.ended(&session).await;

    assert_eq!(rig.effect(&effect).await.state, EffectState::OutcomeUnknown);
    assert_eq!(
        rig.summary(&session).await.effort_choice.as_deref(),
        Some("high")
    );
    assert_eq!(rig.resume_effort(&session).await.as_deref(), Some("high"));
}

#[tokio::test]
async fn a_resume_passes_the_choice_and_never_the_observed_value() {
    let mut rig = Rig::new().await;
    // The choice is `high`; the provider reported `max` and then nothing.
    let session = rig.live(Some("high")).await;
    rig.emit(
        &session,
        Event::EffortChanged {
            effort: Some("max".to_owned()),
        },
    )
    .await;
    rig.ended(&session).await;
    // `session.started` cleared the observed value; the choice stays.
    let summary = rig.summary(&session).await;
    assert_eq!(summary.effort, None);
    assert_eq!(summary.effort_choice.as_deref(), Some("high"));
    assert_eq!(rig.resume_effort(&session).await.as_deref(), Some("high"));
}

#[tokio::test]
async fn a_session_without_a_choice_resumes_without_an_effort() {
    let mut rig = Rig::new().await;
    let session = rig.live(None).await;
    rig.emit(
        &session,
        Event::EffortChanged {
            effort: Some("max".to_owned()),
        },
    )
    .await;
    rig.ended(&session).await;
    assert_eq!(rig.resume_effort(&session).await, None);
}
