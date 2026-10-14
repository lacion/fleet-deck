//! Auto-continue (L19, ADR-007 §5, DL-127): the negative tests, the stored
//! intent, and one continuation end to end against `fake-claude sim`.
//!
//! Each test builds a session whose stored events say that a turn was
//! running, then starts a station on the same store: recovery loses the
//! session (its direct child is gone) and the auto-continue evaluation
//! decides before any command.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, StopMode, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, RunOrigin, SessionState, TurnStatus};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, RejectCode, SessionId};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};

use super::auto_continue::{evaluate, recheck};
use super::policy::Policy;
use super::{Adapters, SessionManager};
use crate::store::auto_continue::{CONTINUATION_TEXT, WINDOW_MS};
use crate::store::{
    Caller, EffectPayload, EffectState, InputRecord, NewSession, OfflineApprovalPolicy, Store,
    StoreConfig,
};

const WAIT: Duration = Duration::from_secs(10);
const MODE: &str = "default";

/// What the stored events say the session did before the restart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Idle,
    Turn,
    Maintenance,
    CompletedTurn,
}

struct Rig {
    det: Det,
    root: tempfile::TempDir,
    store: Store,
    next: u32,
}

fn adapters() -> Adapters {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let mut config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    config.stop = StopPolicy {
        grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(300),
    };
    config.delivery_grace = Duration::from_millis(500);
    Adapters::from([(
        "claude".into(),
        Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())))
            as Arc<dyn fleetdeck_providers::ProviderAdapter>,
    )])
}

impl Rig {
    async fn new() -> Self {
        let det = Det::new();
        det.set(1_000_000);
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        Self {
            det,
            root,
            store,
            next: 0,
        }
    }

    fn policy_with(&self, kind: ProviderKind) -> Policy {
        Policy::new(
            vec![self.root.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), kind)]),
        )
    }

    fn policy(&self) -> Policy {
        self.policy_with(ProviderKind::Claude)
    }

    fn workspace(&self) -> String {
        self.root
            .path()
            .join("projects/app")
            .to_string_lossy()
            .into_owned()
    }

    /// A Claude session with a live process record, `auto_continue` set
    /// to `auto`, and `work` in progress.
    async fn session(&self, name: &str, auto: bool, work: Work) -> SessionId {
        let s = SessionId::from(name);
        self.store
            .create_session(NewSession {
                session_id: s.clone(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: self.workspace(),
                worktree: None,
                branch: None,
                mode: Some(MODE.to_owned()),
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        self.store
            .process_started(s.clone(), format!("proc-{name}").into(), None)
            .await
            .unwrap();
        let mut refs = serde_json::Map::new();
        refs.insert("session_id".into(), format!("native-{name}").into());
        let mut events = vec![Event::SessionStarted {
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            provider_version: None,
            model: None,
            cwd: self.workspace(),
            mode: Some(MODE.to_owned()),
            capabilities: Box::new(fleetdeck_providers::claude::args::capabilities(false)),
            resumed: None,
            provider_refs: refs,
            title: None,
            branch: None,
            repo: None,
        }];
        if auto {
            events.push(Event::AutoContinueChanged { enabled: true });
        }
        if work != Work::Idle {
            let summary = if work == Work::Maintenance {
                "/compact"
            } else {
                "build the thing"
            };
            events.push(Event::RunStarted {
                author: None,
                input_id: None,
                run_id: "run-1".into(),
                input_summary: summary.to_owned(),
                origin: RunOrigin::Board,
            });
            events.push(Event::TurnStarted {
                run_id: "run-1".into(),
                turn_id: "turn-1".into(),
                provider_refs: serde_json::Map::new(),
            });
        }
        if work == Work::CompletedTurn {
            events.push(Event::TurnCompleted {
                turn_id: "turn-1".into(),
                status: TurnStatus::Done,
                usage: None,
            });
        }
        self.store
            .append(s.clone(), events.into_iter().map(Into::into).collect())
            .await
            .unwrap();
        s
    }

    /// An owner effect admitted now (as the decision of its command would
    /// store it).
    async fn admit(&mut self, s: &SessionId, payload: EffectPayload) {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("owner-{}", self.next)),
            target_incarnation: None,
            retry_until: self.store.now_ms() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::Interrupt {
                session_id: s.clone(),
            },
        };
        let id = s.clone();
        let d = self
            .store
            .decide(envelope, Caller::local(), move |d| {
                d.enqueue(&id, payload)?;
                Ok(super::decide::result(&id, None))
            })
            .await
            .unwrap();
        assert_eq!(d.receipt.state, ReceiptState::Accepted);
    }

    /// A station start without a worker: recovery, then the evaluation.
    async fn restart(&self, policy: &Policy) -> usize {
        let head = self.store.head();
        self.store.recover(Vec::new()).await.unwrap();
        evaluate(&self.store, policy, head, false).await.unwrap()
    }

    async fn events(&self, s: &SessionId) -> Vec<Event> {
        let mut out = Vec::new();
        let mut cursor = 0;
        loop {
            let crate::store::ReadResult::Events { events, .. } =
                self.store.read_after(cursor, 1000).await.unwrap()
            else {
                panic!("snapshot required");
            };
            let Some(last) = events.last() else {
                return out;
            };
            cursor = last.station_seq;
            out.extend(
                events
                    .into_iter()
                    .filter(|e| e.session_id == *s)
                    .filter_map(|e| match e.event {
                        EventBody::Known(k) => Some(k),
                        EventBody::Unknown { .. } => None,
                    }),
            );
        }
    }

    async fn skips(&self, s: &SessionId) -> Vec<String> {
        self.events(s)
            .await
            .into_iter()
            .filter_map(|e| match e {
                Event::AutoContinueSkipped { reason } => Some(reason),
                _ => None,
            })
            .collect()
    }

    async fn resumes(&self, s: &SessionId) -> Vec<crate::store::Effect> {
        self.store
            .session_effects(s.clone())
            .await
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.payload, EffectPayload::Resume { .. }))
            .collect()
    }

    async fn state(&self, s: &SessionId) -> SessionState {
        self.store.summary(s.clone()).await.unwrap().unwrap().state
    }

    /// Asserts that `s` stayed `lost` with exactly the skip `reason` and
    /// no continuation.
    async fn skipped(&self, s: &SessionId, reason: &str) {
        assert_eq!(self.state(s).await, SessionState::Lost);
        assert_eq!(self.skips(s).await, vec![reason.to_owned()]);
        assert!(self.resumes(s).await.is_empty());
    }
}

fn follow_up(text: &str) -> EffectPayload {
    EffectPayload::FollowUp {
        input: InputRecord {
            input_id: format!("in-{text}").into(),
            origin: RunOrigin::Board,
            input: UserInput::new(text),
        },
    }
}

#[tokio::test]
async fn auto_continue_is_off_by_default_and_a_restart_never_continues_a_session_without_it() {
    let rig = Rig::new().await;
    let s = rig.session("off", false, Work::Turn).await;
    assert!(
        !rig.store
            .summary(s.clone())
            .await
            .unwrap()
            .unwrap()
            .auto_continue
    );
    // A summary stored before the field existed decodes as off.
    let mut old = serde_json::to_value(fleetdeck_proto::SessionSummary::new(s.clone())).unwrap();
    old.as_object_mut().unwrap().remove("auto_continue");
    let old: fleetdeck_proto::SessionSummary = serde_json::from_value(old).unwrap();
    assert!(!old.auto_continue);
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    assert_eq!(rig.state(&s).await, SessionState::Lost);
    assert!(rig.resumes(&s).await.is_empty());
    // Off is the default: the station adds no skip event (DL-127).
    assert!(rig.skips(&s).await.is_empty());
}

#[tokio::test]
async fn a_session_lost_with_execution_unknown_is_never_continued() {
    let rig = Rig::new().await;
    let s = rig.session("unknown", true, Work::Turn).await;
    rig.store
        .shim_starting(crate::store::NewShim {
            shim_id: "shim-1".to_owned(),
            session_id: s.clone(),
            socket_path: "/nonexistent/sock".into(),
        })
        .await
        .unwrap();
    rig.store
        .shim_end(
            "shim-1".to_owned(),
            crate::store::ShimState::Lost,
            "execution_unknown".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "execution_unknown").await;
}

#[tokio::test]
async fn an_idle_session_is_not_continued() {
    let rig = Rig::new().await;
    let s = rig.session("idle", true, Work::Idle).await;
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "idle").await;
}

#[tokio::test]
async fn a_command_admitted_after_the_last_turn_start_prevents_the_continuation() {
    let mut rig = Rig::new().await;
    let s = rig.session("newer", true, Work::Turn).await;
    rig.det.advance(Duration::from_millis(5));
    rig.admit(&s, follow_up("more")).await;
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "newer_owner_work").await;
}

#[tokio::test]
async fn a_requested_stop_prevents_the_continuation() {
    let mut rig = Rig::new().await;
    let s = rig.session("stop", true, Work::Turn).await;
    rig.det.advance(Duration::from_millis(5));
    rig.admit(
        &s,
        EffectPayload::Stop {
            how: StopMode::Graceful,
        },
    )
    .await;
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "stop_requested").await;
}

#[tokio::test]
async fn a_maintenance_turn_is_not_continued() {
    let rig = Rig::new().await;
    let s = rig.session("compact", true, Work::Maintenance).await;
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "maintenance_turn").await;
}

#[tokio::test]
async fn a_completed_turn_is_not_continued() {
    let rig = Rig::new().await;
    let s = rig.session("done", true, Work::CompletedTurn).await;
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "turn_completed").await;
}

#[tokio::test]
async fn an_expired_window_prevents_the_continuation() {
    let rig = Rig::new().await;
    let s = rig.session("late", true, Work::Turn).await;
    rig.det
        .advance(Duration::from_millis(u64::try_from(WINDOW_MS).unwrap() + 1));
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "expired").await;
}

#[tokio::test]
async fn an_input_with_an_unknown_outcome_prevents_the_continuation_and_is_never_resent() {
    let mut rig = Rig::new().await;
    let s = rig.session("ambiguous", true, Work::Turn).await;
    // Admitted with the turn start (not newer owner work), then claimed: the
    // restart cannot tell whether it reached the provider.
    rig.admit(&s, follow_up("maybe")).await;
    let fence = rig.store.register_worker().await.unwrap();
    let claimed = rig.store.claim_next(fence).await.unwrap().unwrap();
    assert!(matches!(claimed.payload, EffectPayload::FollowUp { .. }));
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "input_outcome_unknown").await;
    let effects = rig.store.session_effects(s.clone()).await.unwrap();
    assert_eq!(effects.len(), 1, "never resent: {effects:?}");
    assert_eq!(effects[0].state, EffectState::OutcomeUnknown);
}

#[tokio::test]
async fn a_changed_provider_instance_or_native_identity_prevents_the_continuation() {
    let rig = Rig::new().await;
    let s = rig.session("moved", true, Work::Turn).await;
    // The instance `claude` is now configured as another provider kind.
    assert_eq!(rig.restart(&rig.policy_with(ProviderKind::Codex)).await, 0);
    rig.skipped(&s, "identity_changed").await;
}

#[tokio::test]
async fn a_second_station_start_finds_the_stored_intent_and_stores_no_second_one() {
    let rig = Rig::new().await;
    let s = rig.session("twice", true, Work::Turn).await;
    let head = rig.store.head();
    rig.store.recover(Vec::new()).await.unwrap();
    assert_eq!(
        evaluate(&rig.store, &rig.policy(), head, false)
            .await
            .unwrap(),
        1
    );
    // The same start evaluated again (a crash before the first dispatch).
    assert_eq!(
        evaluate(&rig.store, &rig.policy(), head, false)
            .await
            .unwrap(),
        0
    );
    assert_eq!(rig.resumes(&s).await.len(), 1);
    assert!(rig.skips(&s).await.is_empty());
}

#[tokio::test]
async fn an_owner_command_after_the_intent_retires_the_undispatched_continuation() {
    let mut rig = Rig::new().await;
    let s = rig.session("retire", true, Work::Turn).await;
    assert_eq!(rig.restart(&rig.policy()).await, 1);
    let intent = rig.resumes(&s).await.remove(0);
    rig.det.advance(Duration::from_millis(5));
    // The raw enqueue below needs a process record; the decision path of
    // a real owner command is not under test here, only the recheck.
    rig.store
        .process_started(s.clone(), "proc-owner".into(), None)
        .await
        .unwrap();
    rig.admit(&s, follow_up("new work")).await;
    assert_eq!(
        recheck(&rig.store, Some(&rig.policy()), &intent)
            .await
            .as_deref(),
        Some("newer_owner_work")
    );
    assert_eq!(rig.state(&s).await, SessionState::Lost);
    assert_eq!(rig.skips(&s).await, vec!["newer_owner_work".to_owned()]);
}

#[tokio::test]
async fn a_crash_after_the_resume_before_the_input_never_starts_a_second_process_and_never_resends()
{
    let rig = Rig::new().await;
    let s = rig.session("crash", true, Work::Turn).await;
    assert_eq!(rig.restart(&rig.policy()).await, 1);
    // The worker claims the continuation; the station dies before the
    // dispatch result is stored.
    let fence = rig.store.register_worker().await.unwrap();
    let claimed = rig.store.claim_next(fence).await.unwrap().unwrap();
    assert!(matches!(claimed.payload, EffectPayload::Resume { .. }));
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    let resumes = rig.resumes(&s).await;
    assert_eq!(resumes.len(), 1, "no second process: {resumes:?}");
    assert_eq!(resumes[0].state, EffectState::OutcomeUnknown);
    let inputs = rig
        .store
        .session_effects(s.clone())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !matches!(e.payload, EffectPayload::Resume { .. }))
        .count();
    assert_eq!(inputs, 0, "the continuation is never resent as a prompt");
    assert_eq!(rig.state(&s).await, SessionState::Lost);
}

#[tokio::test]
async fn the_continued_session_runs_with_the_stored_mode_never_a_more_permissive_one() {
    let rig = Rig::new().await;
    let s = rig.session("mode", true, Work::Turn).await;
    assert_eq!(rig.restart(&rig.policy()).await, 1);
    let intent = rig.resumes(&s).await.remove(0);
    let EffectPayload::Resume {
        native_session_id,
        spec,
        prompt,
        continue_by,
    } = intent.payload
    else {
        unreachable!()
    };
    assert_eq!(native_session_id, "native-mode");
    assert_eq!(spec.mode.as_deref(), Some(MODE));
    let prompt = prompt.unwrap();
    assert_eq!(prompt.origin, RunOrigin::Continuation);
    assert_eq!(prompt.input.text, CONTINUATION_TEXT);
    assert!(continue_by.is_some());
}

// Dispatch rechecks of ADR-007 §2.4 and A10 (DL-133): every rule that can
// change between the stored intent and its dispatch is checked again, and
// a failed recheck retires the intent with the rule's code.

/// A stored continuation intent of a session lost in a turn.
async fn stored_intent(rig: &Rig, name: &str) -> (SessionId, crate::store::Effect) {
    let s = rig.session(name, true, Work::Turn).await;
    assert_eq!(rig.restart(&rig.policy()).await, 1);
    let intent = rig.resumes(&s).await.remove(0);
    rig.det.advance(Duration::from_millis(5));
    (s, intent)
}

/// Rechecks `intent` and asserts the retirement with `code`.
async fn retired(
    rig: &Rig,
    policy: &Policy,
    s: &SessionId,
    intent: &crate::store::Effect,
    code: &str,
) {
    assert_eq!(
        recheck(&rig.store, Some(policy), intent).await.as_deref(),
        Some(code)
    );
    assert_eq!(rig.state(s).await, SessionState::Lost);
    assert_eq!(rig.skips(s).await, vec![code.to_owned()]);
}

#[tokio::test]
async fn an_unchanged_intent_passes_the_dispatch_recheck() {
    let rig = Rig::new().await;
    let (_, intent) = stored_intent(&rig, "fine").await;
    assert_eq!(
        recheck(&rig.store, Some(&rig.policy()), &intent).await,
        None
    );
}

#[tokio::test]
async fn a_maintenance_turn_after_the_intent_retires_the_undispatched_continuation() {
    let rig = Rig::new().await;
    let (s, intent) = stored_intent(&rig, "compact-later").await;
    let run = Event::RunStarted {
        author: None,
        input_id: None,
        run_id: "run-2".into(),
        input_summary: "/compact".to_owned(),
        origin: RunOrigin::Board,
    };
    rig.store.append(s.clone(), vec![run.into()]).await.unwrap();
    retired(&rig, &rig.policy(), &s, &intent, "maintenance_turn").await;
}

#[tokio::test]
async fn a_completion_after_the_intent_retires_the_undispatched_continuation() {
    let rig = Rig::new().await;
    let (s, intent) = stored_intent(&rig, "done-later").await;
    let done = Event::TurnCompleted {
        turn_id: "turn-1".into(),
        status: TurnStatus::Done,
        usage: None,
    };
    rig.store
        .append(s.clone(), vec![done.into()])
        .await
        .unwrap();
    retired(&rig, &rig.policy(), &s, &intent, "turn_completed").await;

    let (s, intent) = stored_intent(&rig, "ended-later").await;
    let ended = Event::SessionEnded {
        reason: "exited".to_owned(),
        exit_code: Some(0),
    };
    rig.store
        .append(s.clone(), vec![ended.into()])
        .await
        .unwrap();
    retired(&rig, &rig.policy(), &s, &intent, "turn_completed").await;
}

#[tokio::test]
async fn an_expired_window_at_dispatch_retires_the_undispatched_continuation() {
    let rig = Rig::new().await;
    let (s, intent) = stored_intent(&rig, "late-dispatch").await;
    rig.det
        .advance(Duration::from_millis(u64::try_from(WINDOW_MS).unwrap() + 1));
    retired(&rig, &rig.policy(), &s, &intent, "expired").await;
}

#[tokio::test]
async fn a_lost_resume_capability_retires_the_undispatched_continuation() {
    let rig = Rig::new().await;
    let (s, intent) = stored_intent(&rig, "no-resume").await;
    let mut capabilities = fleetdeck_providers::claude::args::capabilities(false);
    capabilities.resume = false;
    let mut refs = serde_json::Map::new();
    refs.insert("session_id".into(), "native-no-resume".into());
    let started = Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: rig.workspace(),
        mode: Some(MODE.to_owned()),
        capabilities: Box::new(capabilities),
        resumed: None,
        provider_refs: refs,
        title: None,
        branch: None,
        repo: None,
    };
    rig.store
        .append(s.clone(), vec![started.into()])
        .await
        .unwrap();
    // The event does not move the session out of `lost` for the recheck.
    let state = Event::SessionStateChanged {
        state: SessionState::Lost,
        detail: None,
    };
    rig.store
        .append(s.clone(), vec![state.into()])
        .await
        .unwrap();
    retired(&rig, &rig.policy(), &s, &intent, "unsupported").await;
}

#[tokio::test]
async fn a_full_station_a_removed_root_or_a_removed_instance_retires_the_undispatched_continuation()
{
    let rig = Rig::new().await;
    let (s, intent) = stored_intent(&rig, "full").await;
    let full = Policy::new(
        vec![rig.root.path().join("projects")],
        0,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    );
    retired(&rig, &full, &s, &intent, "not_resumable").await;

    let (s, intent) = stored_intent(&rig, "moved-root").await;
    let elsewhere = tempfile::tempdir().unwrap();
    let other_root = Policy::new(
        vec![elsewhere.path().to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    );
    retired(&rig, &other_root, &s, &intent, "not_resumable").await;

    let (s, intent) = stored_intent(&rig, "no-instance").await;
    let no_instance = Policy::new(
        vec![rig.root.path().join("projects")],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    );
    retired(&rig, &no_instance, &s, &intent, "identity_changed").await;
}

#[cfg(test)]
#[path = "auto_continue_start_tests.rs"]
mod start_tests;
