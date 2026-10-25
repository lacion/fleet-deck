//! The effect worker (M2 P2-5).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, RETRY_WINDOW_MS, SessionCommand, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, RunOrigin};
use fleetdeck_proto::{CommandEnvelope, CommandId};
use fleetdeck_testkit::Det;
use tokio::sync::watch;

use super::*;
use crate::store::{
    Caller, EffectId, EffectPayload, EffectState, InputRecord, NewSession, OfflineApprovalPolicy,
    StoreConfig,
};

const WAIT: Duration = Duration::from_secs(5);

/// Records what the store says about each effect while it is dispatched.
struct Probe {
    store: Store,
    answer: Dispatch,
    seen: Mutex<Vec<(EffectId, EffectState, Option<u64>)>>,
}

#[async_trait]
impl EffectTarget for Probe {
    async fn dispatch(&self, effect: &Effect) -> Dispatch {
        let stored = self
            .store
            .effect(effect.effect_id.clone())
            .await
            .unwrap()
            .unwrap();
        self.seen
            .lock()
            .unwrap()
            .push((stored.effect_id, stored.state, stored.fence_token));
        self.answer.clone()
    }
}

impl Probe {
    fn new(store: &Store, answer: Dispatch) -> Arc<Self> {
        Arc::new(Self {
            store: store.clone(),
            answer,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

async fn running(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s.create_session(NewSession {
        session_id: "s1".into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
    s.process_started("s1".into(), "proc-1".into(), None)
        .await
        .unwrap();
    s
}

async fn queue(s: &Store, det: &Det, id: &str) -> Effect {
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Prompt {
            session_id: "s1".into(),
            input: UserInput::new(id.to_owned()),
        },
    };
    let input = id.to_owned();
    let d = s
        .decide(envelope, Caller::local(), move |d| {
            d.enqueue(
                &"s1".into(),
                EffectPayload::Prompt {
                    input: InputRecord {
                        input_id: input.into(),
                        origin: RunOrigin::Board,
                        input: UserInput::new("hi".to_owned()),
                        shell_context: Vec::new(),
                    },
                    authority: None,
                },
            )?;
            Ok(CommandResult {
                session_id: Some("s1".into()),
                input_id: None,
                seq: None,
            })
        })
        .await
        .unwrap();
    d.effects.into_iter().next().unwrap()
}

async fn until(what: &str, f: impl Fn() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test]
async fn the_worker_commits_the_claim_with_its_fence_before_it_dispatches() {
    let det = Det::new();
    let s = running(&det).await;
    let e = queue(&s, &det, "c1").await;
    let probe = Probe::new(&s, Dispatch::Sent);
    let worker = EffectWorker::register(s.clone(), probe.clone())
        .await
        .unwrap();
    let ran = worker.run_once().await.unwrap().unwrap();
    assert_eq!(ran.state, EffectState::Claimed, "sent waits for evidence");
    assert_eq!(
        *probe.seen.lock().unwrap(),
        [(e.effect_id, EffectState::Claimed, Some(worker.fence()))]
    );
    assert!(worker.run_once().await.unwrap().is_none());
}

#[tokio::test]
async fn a_refusal_of_the_target_retires_the_effect() {
    let det = Det::new();
    let s = running(&det).await;
    let e = queue(&s, &det, "c1").await;
    let probe = Probe::new(&s, Dispatch::NotDispatched("stale process".to_owned()));
    let worker = EffectWorker::register(s.clone(), probe).await.unwrap();
    assert_eq!(worker.drain().await.unwrap(), 1);
    let stored = s.effect(e.effect_id).await.unwrap().unwrap();
    assert_eq!(stored.state, EffectState::RetiredNotDispatched);
    assert_eq!(stored.reason.as_deref(), Some("stale process"));
}

#[tokio::test]
async fn a_deferred_effect_is_not_settled_and_the_target_settles_it_later() {
    let det = Det::new();
    let s = running(&det).await;
    let e = queue(&s, &det, "c1").await;
    let probe = Probe::new(&s, Dispatch::Deferred);
    let worker = EffectWorker::register(s.clone(), probe.clone())
        .await
        .unwrap();
    let ran = worker.run_once().await.unwrap().unwrap();
    assert_eq!(ran.effect_id, e.effect_id);
    assert_eq!(ran.state, EffectState::Claimed);
    let stored = s.effect(e.effect_id.clone()).await.unwrap().unwrap();
    assert_eq!(
        stored.state,
        EffectState::Claimed,
        "the worker stored nothing"
    );
    assert!(worker.run_once().await.unwrap().is_none(), "no second run");
    // The target settles with the fence of the claim.
    let fence = stored.fence_token.unwrap();
    let settled = s
        .settle(e.effect_id, fence, Dispatch::Confirmed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, EffectState::Confirmed);
}

#[tokio::test]
async fn the_worker_wakes_on_new_effects_and_stops_on_shutdown() {
    let det = Det::new();
    let s = running(&det).await;
    let probe = Probe::new(&s, Dispatch::Confirmed);
    let worker = EffectWorker::register(s.clone(), probe.clone())
        .await
        .unwrap();
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(worker.run(shutdown));
    queue(&s, &det, "c1").await;
    until("the first effect", || probe.count() == 1).await;
    queue(&s, &det, "c2").await;
    queue(&s, &det, "c3").await;
    until("three effects", || probe.count() == 3).await;
    stop.send(true).unwrap();
    tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let states: Vec<_> = s
        .session_effects("s1".into())
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.state)
        .collect();
    assert_eq!(states, [EffectState::Confirmed; 3]);
}

#[tokio::test]
async fn a_replaced_worker_stops_and_the_new_one_dispatches_each_effect_once() {
    let det = Det::new();
    let s = running(&det).await;
    let old_probe = Probe::new(&s, Dispatch::Sent);
    let old = EffectWorker::register(s.clone(), old_probe.clone())
        .await
        .unwrap();
    let (_stop, shutdown) = watch::channel(false);
    let old_task = tokio::spawn(old.run(shutdown));

    let new_probe = Probe::new(&s, Dispatch::Sent);
    let new = EffectWorker::register(s.clone(), new_probe.clone())
        .await
        .unwrap();
    queue(&s, &det, "c1").await;
    // The old worker wakes, is fenced at its claim, and returns.
    tokio::time::timeout(WAIT, old_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(old_probe.count(), 0);
    assert_eq!(new.drain().await.unwrap(), 1);
    assert_eq!(new_probe.count(), 1);
}
