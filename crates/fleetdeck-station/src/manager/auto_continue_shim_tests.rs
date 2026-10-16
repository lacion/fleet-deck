//! Auto-continue on the shim host (DL-188 step 3, design `shim-parity.md`
//! §2 G4): a lost shim session is continued only with the DL-182 proof that
//! its run ended, and the dispatch checks the proof again like a manual
//! resume. The shim rows are written the way a shim station leaves them.

use std::collections::BTreeMap;

use fleetdeck_proto::event::{ProviderKind, RunOrigin, SessionState};
use fleetdeck_proto::{Event, EventBody, SessionId};
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use fleetdeck_testkit::Det;

use super::{evaluate, recheck};
use crate::manager::policy::Policy;
use crate::manager::shim_resume_rig::{Boot, dead_identity};
use crate::store::{
    Effect, EffectPayload, NewSession, NewShim, OfflineApprovalPolicy, ShimState, Store,
    StoreConfig,
};

struct Rig {
    det: Det,
    root: tempfile::TempDir,
    store: Store,
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
        Self { det, root, store }
    }

    /// The policy of a shim station.
    fn policy(&self) -> Policy {
        Policy::new(
            vec![self.root.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        )
        .on_shim_host()
    }

    fn workspace(&self) -> String {
        self.root
            .path()
            .join("projects/app")
            .to_string_lossy()
            .into_owned()
    }

    /// A Claude session of the native conversation `native` with a live
    /// process record and a turn in progress; `auto` sets auto-continue.
    async fn session(&self, name: &str, native: &str, auto: bool) -> SessionId {
        let s = SessionId::from(name);
        self.store
            .create_session(NewSession {
                session_id: s.clone(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: self.workspace(),
                worktree: None,
                branch: None,
                mode: Some("default".to_owned()),
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
        refs.insert("session_id".into(), native.into());
        let mut events = vec![Event::SessionStarted {
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            provider_version: None,
            model: None,
            cwd: self.workspace(),
            mode: Some("default".to_owned()),
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
        events.push(Event::RunStarted {
            author: None,
            input_id: None,
            run_id: "run-1".into(),
            input_summary: "build the thing".to_owned(),
            origin: RunOrigin::Board,
        });
        events.push(Event::TurnStarted {
            run_id: "run-1".into(),
            turn_id: "turn-1".into(),
            provider_refs: serde_json::Map::new(),
        });
        self.store
            .append(s.clone(), events.into_iter().map(Into::into).collect())
            .await
            .unwrap();
        s
    }

    /// One ended shim row of `session`; `proof_boot` stores a reboot proof
    /// with that boot id.
    async fn row(
        &self,
        session: &SessionId,
        id: &str,
        (state, reason, inspection): (ShimState, &str, u32),
        identity: Option<ProviderIdentity>,
        proof_boot: Option<&str>,
    ) {
        self.store
            .shim_starting(NewShim {
                shim_id: id.to_owned(),
                session_id: session.clone(),
                socket_path: format!("/nonexistent/{id}.s").into(),
                launch_boot_id: identity.as_ref().map(|i| i.boot_id.clone()),
                native_target: None,
            })
            .await
            .unwrap();
        if let Some(identity) = identity {
            self.store
                .shim_running_with(
                    id.to_owned(),
                    identity,
                    crate::store::RunningFacts::default(),
                )
                .await
                .unwrap();
        }
        self.store
            .shim_end(id.to_owned(), state, reason.to_owned(), inspection)
            .await
            .unwrap();
        if let Some(boot) = proof_boot {
            assert!(
                self.store
                    .shim_prove_reboot(id.to_owned(), boot.to_owned())
                    .await
                    .unwrap()
            );
        }
    }

    /// A lost row with a reboot proof: the run of `boot-old` ended.
    async fn proven_row(&self, session: &SessionId, id: &str) {
        self.row(
            session,
            id,
            (ShimState::Lost, "execution_unknown", 0),
            Some(dead_identity("boot-old")),
            Some("boot-new"),
        )
        .await;
    }

    /// A station start without a worker: recovery, then the evaluation.
    async fn restart(&self, policy: &Policy) -> usize {
        let head = self.store.head();
        self.store.recover(Vec::new()).await.unwrap();
        evaluate(&self.store, policy, head).await.unwrap()
    }

    async fn events(&self, s: &SessionId) -> Vec<Event> {
        let crate::store::ReadResult::Events { events, .. } =
            self.store.read_after(0, 100_000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        events
            .into_iter()
            .filter(|e| e.session_id == *s)
            .filter_map(|e| match e.event {
                EventBody::Known(k) => Some(k),
                EventBody::Unknown { .. } => None,
            })
            .collect()
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

    async fn resumes(&self, s: &SessionId) -> Vec<Effect> {
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

    /// Asserts that `s` stayed `lost` with exactly the skip `reason`.
    async fn skipped(&self, s: &SessionId, reason: &str) {
        assert_eq!(self.state(s).await, SessionState::Lost);
        assert_eq!(self.skips(s).await, vec![reason.to_owned()]);
        assert!(self.resumes(s).await.is_empty());
    }
}

fn boot(id: &str) -> Boot {
    let boot = Boot(std::sync::Arc::default());
    boot.set(Some(id));
    boot
}

#[tokio::test]
async fn a_lost_session_with_a_reboot_proof_gets_one_continuation() {
    let rig = Rig::new().await;
    let s = rig.session("proven", "native-proven", true).await;
    rig.proven_row(&s, "shim-1").await;
    assert_eq!(rig.restart(&rig.policy()).await, 1);
    let resumes = rig.resumes(&s).await;
    assert_eq!(resumes.len(), 1, "{resumes:?}");
    let EffectPayload::Resume {
        prompt,
        continue_by,
        ..
    } = &resumes[0].payload
    else {
        unreachable!()
    };
    assert!(continue_by.is_some());
    assert_eq!(prompt.as_ref().unwrap().origin, RunOrigin::Continuation);
    assert!(rig.skips(&s).await.is_empty());
}

#[tokio::test]
async fn a_lost_session_without_a_proof_waits_for_the_owner() {
    let rig = Rig::new().await;
    let s = rig.session("unproven", "native-unproven", true).await;
    rig.row(
        &s,
        "shim-1",
        (ShimState::Lost, "execution_unknown", 0),
        Some(dead_identity("boot-old")),
        None,
    )
    .await;
    assert_eq!(rig.restart(&rig.policy()).await, 0);
    rig.skipped(&s, "execution_unknown").await;
}

#[tokio::test]
async fn descendants_remaining_and_an_unverified_exit_are_not_a_proof() {
    for (n, reason) in ["exited_descendants_remaining", "exit_unverified"]
        .into_iter()
        .enumerate()
    {
        let rig = Rig::new().await;
        let s = rig
            .session(&format!("open-{n}"), &format!("native-open-{n}"), true)
            .await;
        rig.row(
            &s,
            "shim-1",
            (ShimState::Exited, reason, 1),
            Some(dead_identity("boot-old")),
            None,
        )
        .await;
        assert_eq!(rig.restart(&rig.policy()).await, 0, "{reason}");
        rig.skipped(&s, "execution_unknown").await;
    }
}

#[tokio::test]
async fn an_old_clean_exit_and_a_newest_reboot_proof_are_eligible() {
    let rig = Rig::new().await;
    let s = rig.session("history", "native-history", true).await;
    rig.row(
        &s,
        "shim-old",
        (ShimState::Exited, "exited", 1),
        Some(dead_identity("boot-old")),
        None,
    )
    .await;
    rig.det.advance(std::time::Duration::from_millis(5));
    rig.proven_row(&s, "shim-new").await;
    assert_eq!(rig.restart(&rig.policy()).await, 1);
    assert_eq!(rig.resumes(&s).await.len(), 1);
    assert!(rig.skips(&s).await.is_empty());
}

#[tokio::test]
async fn another_session_that_holds_the_conversation_is_not_resumable() {
    let rig = Rig::new().await;
    let s = rig.session("first", "native-shared", true).await;
    rig.proven_row(&s, "shim-1").await;
    let head = rig.store.head();
    rig.store.recover(Vec::new()).await.unwrap();
    // A second session holds the conversation after the restart (it has not
    // ended).
    let _holder = rig.session("holder", "native-shared", false).await;
    assert_eq!(evaluate(&rig.store, &rig.policy(), head).await.unwrap(), 0);
    rig.skipped(&s, "not_resumable").await;
}

#[tokio::test]
async fn a_platform_without_a_boot_reader_is_not_resumable() {
    let rig = Rig::new().await;
    let s = rig.session("platform", "native-platform", true).await;
    rig.proven_row(&s, "shim-1").await;
    let policy = rig.policy().with_shim_host(false);
    assert_eq!(rig.restart(&policy).await, 0);
    rig.skipped(&s, "not_resumable").await;
}

#[tokio::test]
async fn a_second_evaluation_stores_no_second_intent() {
    let rig = Rig::new().await;
    let s = rig.session("twice", "native-twice", true).await;
    rig.proven_row(&s, "shim-1").await;
    let head = rig.store.head();
    rig.store.recover(Vec::new()).await.unwrap();
    let policy = rig.policy();
    assert_eq!(evaluate(&rig.store, &policy, head).await.unwrap(), 1);
    assert_eq!(evaluate(&rig.store, &policy, head).await.unwrap(), 0);
    assert_eq!(rig.resumes(&s).await.len(), 1);
    assert!(rig.skips(&s).await.is_empty());
}

#[tokio::test]
async fn the_dispatch_checks_the_reboot_proof_again_and_skips_when_it_fails() {
    let rig = Rig::new().await;
    let s = rig.session("dispatch", "native-dispatch", true).await;
    rig.proven_row(&s, "shim-1").await;
    let policy = rig.policy();
    assert_eq!(rig.restart(&policy).await, 1);
    let intent = rig.resumes(&s).await.remove(0);
    // A boot id that differs from the one of the row: the proof holds.
    let new = boot("boot-new").reader();
    assert_eq!(
        recheck(&rig.store, Some(&policy), Some(&new), &intent).await,
        None
    );
    assert!(rig.skips(&s).await.is_empty());
    // The boot id of the row's run is the current one: the proof is gone.
    let old = boot("boot-old").reader();
    assert_eq!(
        recheck(&rig.store, Some(&policy), Some(&old), &intent)
            .await
            .as_deref(),
        Some("execution_unknown")
    );
    assert_eq!(rig.state(&s).await, SessionState::Lost);
    assert_eq!(rig.skips(&s).await, vec!["execution_unknown".to_owned()]);
}
