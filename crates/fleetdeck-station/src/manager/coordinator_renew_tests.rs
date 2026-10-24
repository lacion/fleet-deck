//! MCP P4b §5: the renewal of a grant with a record moves the generation of
//! the record, and the bind keeps the generation that a start prepared.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, GrantKind, GrantScope, RETRY_WINDOW_MS, ReceiptState,
    SessionCommand,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandId, Event, EventBody, RejectCode};
use fleetdeck_testkit::Det;

use super::plan;
use crate::coordinator::fence::Fence;
use crate::coordinator::{Binding, BindingState, Bindings, ToolClass, mint};
use crate::manager::coordinator_spec_tests::coordinator_spec;
use crate::manager::{Adapters, Policy, SessionManager};
use crate::store::session_grants::{GrantRecord, RecordState};
use crate::store::shim_bindings::{
    BindingRow, IdentitySignal, RowState, Transition, ends_record, evented, transitions,
};
use crate::store::{
    Caller, Decided, NewSession, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};

const GRANT: &str = "g1";
const BINDING: &str = "s1:g1";

struct Rig {
    det: Det,
    manager: SessionManager,
    deck: Caller,
    next: u32,
    _tmp: tempfile::TempDir,
}

fn binding(state: BindingState, generation: u32) -> Binding {
    Binding {
        binding_id: BINDING.to_owned(),
        grant_id: GRANT.to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: (state != BindingState::Pending).then(|| "n1".to_owned()),
        generation,
        classes: BTreeSet::from([ToolClass::Own]),
        scope: GrantScope::Lineage,
        state,
        control_skill: String::new(),
        kind: GrantKind::Coordinator,
    }
}

impl Rig {
    /// A station with the session `s1`; `shim` adds the fence of the shim
    /// host.
    async fn new(shim: bool) -> Self {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store
            .create_session(NewSession {
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
        let tmp = tempfile::tempdir().unwrap();
        let policy = Policy::new(
            vec![tmp.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        );
        let manager = SessionManager::start(
            store,
            policy,
            Adapters::new(),
            det.ids(),
            Arc::new(crate::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap();
        if shim {
            let fence = Arc::new(Fence::new(tmp.path().join("run")));
            fence.ensure().unwrap();
            manager.coordinator_bindings().set_fence(fence);
        }
        let key = Identity::generate().unwrap().public_key();
        manager
            .store()
            .pin_peer(crate::store::peers::PairedKey {
                key,
                role: fleetdeck_link::tls::Role::Deck,
                scopes: [Scope::Read, Scope::Drive].into_iter().collect(),
                dial_url: None,
            })
            .await
            .unwrap();
        Self {
            det,
            manager,
            deck: Caller::peer(key.fingerprint(), Scope::Drive),
            next: 0,
            _tmp: tmp,
        }
    }

    fn bindings(&self) -> &Bindings {
        self.manager.coordinator_bindings()
    }

    async fn renew(&mut self, generation: u32) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("renew-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::CoordinatorRenew {
                session_id: "s1".into(),
                grant_id: GRANT.to_owned(),
                generation,
            },
        };
        self.manager
            .submit(envelope, self.deck.clone())
            .await
            .unwrap()
    }

    /// Writes the record of `s1` as a start would.
    async fn put(&self, generation: u32, state: RecordState) {
        let record = GrantRecord {
            session_id: "s1".into(),
            grant_id: GRANT.to_owned(),
            seq: 1,
            spec: Some(coordinator_spec()),
            generation,
            state,
            ended_reason: None,
            updated_at: 1,
        };
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("put-{generation}-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::CoordinatorRevoke {
                session_id: "s1".into(),
                grant_id: "x".to_owned(),
            },
        };
        self.manager
            .store()
            .decide(envelope, Caller::local(), move |d| {
                d.session_grant_put(&record)?;
                Ok(crate::manager::decide::result(&"s1".into(), None))
            })
            .await
            .unwrap();
    }

    async fn record(&self) -> Option<GrantRecord> {
        self.manager
            .store()
            .session_grant("s1".into())
            .await
            .unwrap()
    }

    /// Stores the live binding with its row (shim host), under a new token.
    async fn run(&self, b: &Binding, row: bool) -> String {
        let (_, digest) = mint().unwrap();
        self.bindings().insert(digest.clone(), b.clone());
        if row {
            let mut stored = BindingRow::from_binding(
                digest.clone(),
                "shim1".to_owned(),
                b,
                Some(coordinator_spec().expires_at),
                1,
            );
            stored.state = match b.state {
                BindingState::Pending => RowState::Pending,
                _ => RowState::Active,
            };
            self.manager
                .store()
                .shim_binding_insert(stored)
                .await
                .unwrap();
        }
        digest
    }

    async fn row_generation(&self, digest: &str) -> u32 {
        self.manager
            .store()
            .shim_bindings_load()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.digest == digest)
            .unwrap()
            .generation
    }

    /// The lifecycle events as `bound:<binding>@<generation>`.
    async fn bound(&self) -> Vec<String> {
        let ReadResult::Events { events, .. } =
            self.manager.store().read_after(0, 1000).await.unwrap()
        else {
            panic!("no events");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::CoordinatorBound {
                    binding_id,
                    generation,
                    ..
                }) => Some(format!("bound:{binding_id}@{generation}")),
                _ => None,
            })
            .collect()
    }
}

fn accepted(d: &Decided) {
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
}

fn rejected(d: &Decided) -> (RejectCode, String) {
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    let r = d.receipt.reject.clone().unwrap();
    (r.code, r.message)
}

#[tokio::test]
async fn a_renewal_of_a_grant_with_a_record_moves_its_generation() {
    for shim in [false, true] {
        // A live binding: generation g' = max(p, record + 1), state Active.
        let mut rig = Rig::new(shim).await;
        rig.put(1, RecordState::Attached).await;
        let digest = rig.run(&binding(BindingState::Active, 1), shim).await;
        accepted(&rig.renew(2).await);
        let b = rig.bindings().get(BINDING).unwrap();
        assert_eq!((b.state, b.generation), (BindingState::Active, 2));
        assert_eq!(rig.record().await.unwrap().generation, 2);
        assert_eq!(rig.bound().await, [format!("bound:{BINDING}@2")]);
        if shim {
            assert_eq!(rig.row_generation(&digest).await, 2, "the row follows");
        }
        // An idle-expired binding, and a command generation above the next.
        rig.bindings()
            .update(BINDING, |b| b.state = BindingState::Expired);
        accepted(&rig.renew(5).await);
        let b = rig.bindings().get(BINDING).unwrap();
        assert_eq!((b.state, b.generation), (BindingState::Active, 5));
        assert_eq!(rig.record().await.unwrap().generation, 5);
        if shim {
            assert_eq!(rig.row_generation(&digest).await, 5);
        }
        // A command generation below the next one: the next one wins.
        accepted(&rig.renew(1).await);
        assert_eq!(rig.bindings().get(BINDING).unwrap().generation, 6);
        assert_eq!(rig.record().await.unwrap().generation, 6);
        assert_eq!(
            rig.bound().await,
            [
                format!("bound:{BINDING}@2"),
                format!("bound:{BINDING}@5"),
                format!("bound:{BINDING}@6")
            ]
        );

        // A pending binding is not bound yet.
        rig.bindings().update(BINDING, |b| {
            b.state = BindingState::Pending;
            b.native_id = None;
        });
        let (code, message) = rejected(&rig.renew(7).await);
        assert_eq!(code, RejectCode::InvalidState);
        assert_eq!(message, "the binding is not bound yet");
        assert_eq!(rig.record().await.unwrap().generation, 6);

        // No process (the credential is dead): the record keeps g - 1, and
        // the next process start binds at g.
        rig.bindings()
            .update(BINDING, |b| b.state = BindingState::Revoked);
        accepted(&rig.renew(9).await);
        assert_eq!(rig.record().await.unwrap().generation, 8);
        assert_eq!(rig.bound().await.len(), 3, "no event without a process");
        let (_, record) = plan(
            rig.manager.store(),
            &"s1".into(),
            &crate::manager::tests::spec("/w", "claude"),
            Some("n1"),
        )
        .await
        .unwrap()
        .expect("a record start");
        assert_eq!(record.generation, 9, "the next start binds at g");
        // A command generation below the base: no move backwards.
        rig.put(8, RecordState::Attached).await;
        accepted(&rig.renew(2).await);
        assert_eq!(rig.record().await.unwrap().generation, 8);
        rig.manager
            .shutdown(fleetdeck_proto::command::StopMode::Kill)
            .await;
    }
}

#[tokio::test]
async fn a_grant_without_a_record_keeps_the_renewal_rule_of_today() {
    let mut rig = Rig::new(false).await;
    // No binding at all.
    let (code, _) = rejected(&rig.renew(2).await);
    assert_eq!(code, RejectCode::InvalidCommand);
    // An active binding does not renew; an idle-expired one at the previous
    // generation does, and only with exactly the next generation.
    rig.run(&binding(BindingState::Active, 1), false).await;
    let (code, message) = rejected(&rig.renew(2).await);
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains("idle-expired"), "{message}");
    rig.bindings()
        .update(BINDING, |b| b.state = BindingState::Expired);
    let (code, _) = rejected(&rig.renew(3).await);
    assert_eq!(code, RejectCode::InvalidState);
    accepted(&rig.renew(2).await);
    let b = rig.bindings().get(BINDING).unwrap();
    assert_eq!((b.state, b.generation), (BindingState::Active, 2));
    assert!(rig.record().await.is_none(), "no record is created");
    // An ended record is no record of the grant either.
    rig.put(2, RecordState::Ended).await;
    rig.bindings()
        .update(BINDING, |b| b.state = BindingState::Expired);
    accepted(&rig.renew(3).await);
    assert_eq!(rig.record().await.unwrap().generation, 2);
    rig.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

fn pending_row(generation: u32, native: Option<&str>) -> BindingRow {
    let mut b = binding(BindingState::Pending, generation);
    b.native_id = native.map(str::to_owned);
    BindingRow::from_binding("d1".to_owned(), "shim1".to_owned(), &b, Some(i64::MAX), 1)
}

fn started(native: &str) -> crate::store::NewEvent {
    started_event(native).into()
}

fn started_event(native: &str) -> Event {
    let mut refs = fleetdeck_proto::event::ProviderRefs::new();
    refs.insert("session_id".to_owned(), native.into());
    Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: "/w".to_owned(),
        mode: None,
        capabilities: Box::new(fleetdeck_proto::capability::Capabilities::minimal()),
        resumed: None,
        provider_refs: refs,
        title: None,
        branch: None,
        repo: None,
    }
}

#[test]
fn a_changed_native_id_of_a_running_grant_with_a_record_is_an_identity_change() {
    use crate::manager::coordinator_start::{Bound, bound_events};
    let session = "s1".into();
    // Direct host: no event here, the record ends with its own event.
    let bindings = Bindings::default();
    let (_, digest) = mint().unwrap();
    bindings.insert(digest.clone(), binding(BindingState::Active, 2));
    let Bound {
        events,
        identity_changed,
    } = bound_events(&bindings, &session, &[started_event("n2")], Some(GRANT));
    assert!(events.is_empty() && identity_changed);
    assert_eq!(bindings.get(BINDING).unwrap().state, BindingState::Revoked);
    // Without a record the event of today: `revoked`.
    bindings.insert(digest, binding(BindingState::Active, 2));
    let found = bound_events(&bindings, &session, &[started_event("n2")], None);
    assert!(!found.identity_changed);
    assert_eq!(found.events.len(), 1);
    // Shim host: the transition is `native_replaced`; the event of a record
    // grant says `identity_changed` and the record ends.
    let mut active = pending_row(2, Some("n1"));
    active.state = RowState::Active;
    let rows = [active];
    let signal = [IdentitySignal {
        native_id: "n2".to_owned(),
        after: 0,
    }];
    let ts = transitions(&rows, &[], &signal, false);
    let replaced = Transition::Ended {
        digest: "d1".to_owned(),
        reason: "native_replaced".to_owned(),
    };
    assert_eq!(ts, std::slice::from_ref(&replaced));
    assert_eq!(evented(&rows, &ts, None), [replaced]);
    let changed = Transition::Ended {
        digest: "d1".to_owned(),
        reason: "identity_changed".to_owned(),
    };
    let ts = evented(&rows, &ts, Some(GRANT));
    assert_eq!(ts, [changed]);
    assert!(ends_record(&rows, &ts, Some(GRANT)));
    assert!(!ends_record(&rows, &ts, None));
}

#[test]
fn the_bind_keeps_the_prepared_generation() {
    // Direct host: `native_identity`.
    let bindings = Bindings::default();
    let (_, d1) = mint().unwrap();
    bindings.insert(d1, binding(BindingState::Pending, 3));
    assert_eq!(
        bindings.native_identity(BINDING, "n1"),
        Some(BindingState::Active)
    );
    let b = bindings.get(BINDING).unwrap();
    assert_eq!((b.generation, b.native_id.as_deref()), (3, Some("n1")));
    // A binding without a prepared generation binds at 1, as today.
    let (_, d2) = mint().unwrap();
    bindings.insert(d2, binding(BindingState::Pending, 0));
    bindings.native_identity(BINDING, "n1");
    assert_eq!(bindings.get(BINDING).unwrap().generation, 1);
    // A pending binding that expects a native id is revoked by another one.
    let mut expecting = binding(BindingState::Pending, 4);
    expecting.native_id = Some("n1".to_owned());
    let (_, d3) = mint().unwrap();
    bindings.insert(d3, expecting.clone());
    assert_eq!(
        bindings.native_identity(BINDING, "n2"),
        Some(BindingState::Revoked)
    );
    let (_, d4) = mint().unwrap();
    bindings.insert(d4, expecting);
    assert_eq!(
        bindings.native_identity(BINDING, "n1"),
        Some(BindingState::Active)
    );
    assert_eq!(bindings.get(BINDING).unwrap().generation, 4);

    // Shim host: `bind()` of the pure transitions.
    let bound = |ts: Vec<Transition>| match ts.as_slice() {
        [Transition::Bound { generation, .. }] => *generation,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        bound(transitions(
            &[pending_row(3, None)],
            &[started("n1")],
            &[],
            false
        )),
        3
    );
    assert_eq!(
        bound(transitions(
            &[pending_row(0, None)],
            &[started("n1")],
            &[],
            false
        )),
        1
    );
    let signal = [IdentitySignal {
        native_id: "n1".to_owned(),
        after: 0,
    }];
    assert_eq!(
        bound(transitions(
            &[pending_row(6, Some("n1"))],
            &[],
            &signal,
            false
        )),
        6
    );
    assert_eq!(
        transitions(&[pending_row(5, Some("n1"))], &[started("n2")], &[], false),
        [Transition::Ended {
            digest: "d1".to_owned(),
            reason: "identity_changed".to_owned()
        }]
    );
}
