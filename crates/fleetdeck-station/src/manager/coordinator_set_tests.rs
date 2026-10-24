//! MCP P4b §11 tests 5 and 6: `session.coordinator_set` orders by the session
//! sequence, rewrites the running binding in place on both hosts, and the
//! snapshot projection follows the record.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CoordinatorSpec, GrantKind, GrantScope, RETRY_WINDOW_MS,
    ReceiptState, SessionCommand,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, CoordinatorBindingPhase as Phase, Event, EventBody, RejectCode};
use fleetdeck_testkit::Det;
use serde_json::Value;

use crate::coordinator::bindings::{Auth, StreamAuth, StreamKind};
use crate::coordinator::fence::Fence;
use crate::coordinator::{Binding, BindingState, Bindings, ToolClass, Transport, mint};
use crate::manager::coordinator_tools::FLEET_SKILL;
use crate::manager::{Adapters, Policy, SessionManager};
use crate::store::session_grants::{GrantRecord, RecordState};
use crate::store::shim_bindings::{BindingRow, RowState};
use crate::store::{
    Caller, Decided, NewSession, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};

const FAR: i64 = 4_102_444_800_000;
const FIXED_ONLY: &str = "desktop, screenshot and input access is given only at start";

/// A deck link that negotiated `coordinator_live.v1` or not.
struct Link {
    live: bool,
}

#[async_trait]
impl Transport for Link {
    async fn request(
        &self,
        _method: &str,
        _command_id: Option<&CommandId>,
        _params: Value,
    ) -> Result<Outcome, String> {
        Err("not used".to_owned())
    }

    fn has_feature(&self, name: &str) -> bool {
        self.live && name == feature::COORDINATOR_LIVE
    }
}

fn spec(grant: &str, classes: &[&str]) -> CoordinatorSpec {
    serde_json::from_value(serde_json::json!({
        "grant_id": grant, "classes": classes, "idle_ttl_ms": 60_000,
        "expires_at": FAR,
        "control_skill": {"version": "1", "text": "You coordinate the fleet."}
    }))
    .unwrap()
}

fn classes(names: &[ToolClass]) -> BTreeSet<ToolClass> {
    names.iter().copied().chain([ToolClass::Own]).collect()
}

fn binding(id: &str, grant: &str, state: BindingState, generation: u32) -> Binding {
    Binding {
        binding_id: id.to_owned(),
        grant_id: grant.to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: (state != BindingState::Pending).then(|| "n1".to_owned()),
        generation,
        classes: classes(&[]),
        scope: GrantScope::Lineage,
        state,
        control_skill: FLEET_SKILL.to_owned(),
        kind: GrantKind::Coordinator,
    }
}

/// The fleet-only binding of a running session.
fn fleet(state: BindingState) -> Binding {
    let generation = u32::from(state != BindingState::Pending);
    binding("s1:fleet", "", state, generation)
}

struct Rig {
    det: Det,
    manager: SessionManager,
    fence: Option<Arc<Fence>>,
    deck: Caller,
    next: u32,
    _tmp: tempfile::TempDir,
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
        let fence = shim.then(|| {
            let fence = Arc::new(Fence::new(tmp.path().join("run")));
            fence.ensure().unwrap();
            manager.coordinator_bindings().set_fence(Arc::clone(&fence));
            fence
        });
        manager
            .coordinator_bindings()
            .set_deck(1, Arc::new(Link { live: true }));
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
            fence,
            deck: Caller::peer(key.fingerprint(), Scope::Drive),
            next: 0,
            _tmp: tmp,
        }
    }

    fn bindings(&self) -> &Bindings {
        self.manager.coordinator_bindings()
    }

    async fn submit(
        &mut self,
        origin: CommandOrigin,
        caller: Caller,
        command: SessionCommand,
    ) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("set-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin,
            command,
        };
        self.manager.submit(envelope, caller).await.unwrap()
    }

    /// The deck sets (`Some`) or ends (`None`) the grant of `s1`.
    async fn set(&mut self, seq: u64, grant: &str, spec: Option<&CoordinatorSpec>) -> Decided {
        let command = SessionCommand::CoordinatorSet {
            session_id: "s1".into(),
            seq,
            grant_id: grant.to_owned(),
            spec: spec.cloned().map(Box::new),
        };
        let deck = self.deck.clone();
        self.submit(CommandOrigin::Board, deck, command).await
    }

    /// Stores the binding of a running process, with a row on the shim host.
    /// Returns the token and the digest.
    async fn run(&self, b: &Binding, row: Option<RowState>) -> (String, String) {
        let (token, digest) = mint().unwrap();
        self.bindings().insert(digest.clone(), b.clone());
        if let Some(state) = row {
            let mut stored =
                BindingRow::from_binding(digest.clone(), "shim1".to_owned(), b, None, 1);
            stored.state = state;
            self.manager
                .store()
                .shim_binding_insert(stored)
                .await
                .unwrap();
        }
        (token.into_exposed(), digest)
    }

    async fn record(&self) -> Option<GrantRecord> {
        self.manager
            .store()
            .session_grant("s1".into())
            .await
            .unwrap()
    }

    /// Writes a record directly, as a start would.
    async fn put(&self, record: GrantRecord) {
        self.manager
            .store()
            .decide(
                CommandEnvelope {
                    authority: None,
                    command_id: CommandId::from("put"),
                    target_incarnation: None,
                    retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
                    origin: CommandOrigin::Board,
                    command: SessionCommand::CoordinatorRevoke {
                        session_id: "s1".into(),
                        grant_id: "x".to_owned(),
                    },
                },
                Caller::local(),
                move |d| {
                    d.session_grant_put(&record)?;
                    Ok(crate::manager::decide::result(&"s1".into(), None))
                },
            )
            .await
            .unwrap();
    }

    /// The lifecycle events, as `bound:<binding>@<generation>` and
    /// `ended:<binding>@<generation>:<reason>`.
    async fn events(&self) -> Vec<String> {
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
                EventBody::Known(Event::CoordinatorEnded {
                    binding_id,
                    generation,
                    reason,
                }) => Some(format!("ended:{binding_id}@{generation}:{reason}")),
                _ => None,
            })
            .collect()
    }

    fn memory(&self, digest: &str) -> Binding {
        self.bindings().get_digest(digest).unwrap()
    }

    async fn row(&self, digest: &str) -> BindingRow {
        self.manager
            .store()
            .shim_bindings_load()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.digest == digest)
            .unwrap()
    }

    /// The binding that the token authenticates as.
    fn authenticated(&self, token: &str) -> Binding {
        match self
            .bindings()
            .authenticate(Some(&format!("Bearer {token}")))
        {
            Auth::Ok { binding, .. } => *binding,
            _ => panic!("the token does not authenticate"),
        }
    }
}

fn rejected(d: &Decided) -> (RejectCode, String) {
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    let r = d.receipt.reject.clone().unwrap();
    (r.code, r.message)
}

fn accepted(d: &Decided) -> Option<u64> {
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    d.receipt.result.clone().unwrap().seq
}

#[tokio::test]
async fn coordinator_set_orders_by_the_session_sequence() {
    let mut rig = Rig::new(false).await;
    // An end that was never recorded still fences the later messages.
    assert_eq!(accepted(&rig.set(2, "g-0", None).await), Some(2));
    let r = rig.record().await.unwrap();
    assert_eq!(
        (r.grant_id.as_str(), r.seq, r.state),
        ("g-0", 2, RecordState::Ended)
    );
    assert_eq!(
        (r.spec.is_none(), r.ended_reason.as_deref()),
        (true, Some("revoked"))
    );
    assert_eq!(
        accepted(&rig.set(1, "g-0", Some(&spec("g-0", &["read"]))).await),
        Some(2)
    );
    assert_eq!(
        rig.record().await.unwrap(),
        r,
        "the delayed give changed nothing"
    );

    // A give; a stale or equal sequence is accepted and ignored, and names
    // the record's sequence.
    assert_eq!(
        accepted(&rig.set(5, "g-1", Some(&spec("g-1", &["read"]))).await),
        Some(5)
    );
    let r = rig.record().await.unwrap();
    assert_eq!(
        (r.grant_id.as_str(), r.seq, r.state),
        ("g-1", 5, RecordState::Attached)
    );
    assert_eq!(r.generation, 0);
    for seq in [5, 3] {
        let wide = spec("g-1", &["read", "mail"]);
        assert_eq!(accepted(&rig.set(seq, "g-1", Some(&wide)).await), Some(5));
        assert_eq!(rig.record().await.unwrap(), r);
    }

    // A replacement (another grant, a higher sequence) ends the old grant.
    assert_eq!(
        accepted(&rig.set(6, "g-2", Some(&spec("g-2", &["mail"]))).await),
        Some(6)
    );
    let r = rig.record().await.unwrap();
    assert_eq!(
        (r.grant_id.as_str(), r.seq, r.state),
        ("g-2", 6, RecordState::Attached)
    );
    assert_eq!(rig.events().await, ["ended:s1:g-1@0:replaced"]);

    // A delayed give or end of the replaced grant is ignored; an end of
    // another grant only moves the sequence.
    assert_eq!(
        accepted(&rig.set(4, "g-1", Some(&spec("g-1", &["read"]))).await),
        Some(6)
    );
    assert_eq!(rig.record().await.unwrap().grant_id, "g-2");
    assert_eq!(accepted(&rig.set(7, "g-1", None).await), Some(7));
    let r = rig.record().await.unwrap();
    assert_eq!(
        (r.grant_id.as_str(), r.seq, r.state),
        ("g-2", 7, RecordState::Attached)
    );
    assert_eq!(rig.events().await, ["ended:s1:g-1@0:replaced"]);

    // The end of the grant of the record; a delayed give of it is ignored.
    assert_eq!(accepted(&rig.set(9, "g-2", None).await), Some(9));
    let r = rig.record().await.unwrap();
    assert_eq!(
        (r.state, r.ended_reason.as_deref()),
        (RecordState::Ended, Some("revoked"))
    );
    assert_eq!(
        accepted(&rig.set(8, "g-2", Some(&spec("g-2", &["mail"]))).await),
        Some(9)
    );
    assert_eq!(rig.record().await.unwrap().state, RecordState::Ended);

    // A new give after the end attaches again with generation 0.
    assert_eq!(
        accepted(&rig.set(10, "g-3", Some(&spec("g-3", &["read"]))).await),
        Some(10)
    );
    assert_eq!(rig.record().await.unwrap().state, RecordState::Attached);
}

#[tokio::test]
async fn coordinator_set_refuses_what_a_live_grant_may_not_carry() {
    let mut rig = Rig::new(false).await;
    // Desktop, capture and input are given only at start.
    for fixed in ["desktop", "capture", "input"] {
        let d = rig
            .set(1, "g-1", Some(&spec("g-1", &["read", fixed])))
            .await;
        assert_eq!(
            rejected(&d),
            (RejectCode::InvalidCommand, FIXED_ONLY.to_owned())
        );
    }
    assert!(rig.record().await.is_none());
    // A change keeps their membership: the record of a start holds them.
    let mut held = spec("g-1", &["read", "desktop", "capture", "input"]);
    rig.put(GrantRecord {
        session_id: "s1".into(),
        grant_id: "g-1".to_owned(),
        seq: 1,
        spec: Some(held.clone()),
        generation: 1,
        state: RecordState::Attached,
        ended_reason: None,
        updated_at: 1,
    })
    .await;
    held.classes.retain(|c| c != "input");
    let d = rig.set(2, "g-1", Some(&held)).await;
    assert_eq!(
        rejected(&d),
        (RejectCode::InvalidCommand, FIXED_ONLY.to_owned())
    );
    held.classes.push("input".to_owned());
    held.classes.push("mail".to_owned());
    assert_eq!(accepted(&rig.set(2, "g-1", Some(&held)).await), Some(2));
    // Another grant may not take them over.
    let other = spec("g-2", &["read", "desktop", "capture", "input"]);
    let d = rig.set(3, "g-2", Some(&other)).await;
    assert_eq!(
        rejected(&d),
        (RejectCode::InvalidCommand, FIXED_ONLY.to_owned())
    );

    // A worker grant, another grant in the spec, a bad structure.
    let mut worker = spec("g-3", &["read"]);
    worker.kind = GrantKind::Worker;
    let d = rig.set(4, "g-3", Some(&worker)).await;
    assert_eq!(
        rejected(&d),
        (
            RejectCode::InvalidCommand,
            "a live grant is an owner grant".to_owned()
        )
    );
    let d = rig.set(4, "g-4", Some(&spec("g-3", &["read"]))).await;
    assert_eq!(
        rejected(&d),
        (
            RejectCode::InvalidCommand,
            "the spec names another grant".to_owned()
        )
    );
    let d = rig.set(4, "g-3", Some(&spec("g-3", &["nope"]))).await;
    assert_eq!(rejected(&d).0, RejectCode::InvalidCommand);
    // The range of the sequence and the grant of an end.
    let d = rig
        .set(u64::try_from(i64::MAX).unwrap() + 1, "g-3", None)
        .await;
    assert_eq!(
        rejected(&d),
        (RejectCode::InvalidCommand, "seq is out of range".to_owned())
    );
    let d = rig.set(4, "", None).await;
    assert_eq!(
        rejected(&d),
        (RejectCode::InvalidCommand, "grant_id is empty".to_owned())
    );
    assert_eq!(
        rig.record().await.unwrap().seq,
        2,
        "no refused set changed the record"
    );

    // The origin, the caller and the feature.
    let deck = rig.deck.clone();
    let command = SessionCommand::CoordinatorSet {
        session_id: "s1".into(),
        seq: 5,
        grant_id: "g-5".to_owned(),
        spec: Some(Box::new(spec("g-5", &["read"]))),
    };
    for origin in [CommandOrigin::Coordinator, CommandOrigin::Mail] {
        let d = rig.submit(origin, deck.clone(), command.clone()).await;
        assert_eq!(rejected(&d).0, RejectCode::Forbidden);
    }
    let d = rig
        .submit(CommandOrigin::Board, Caller::local(), command.clone())
        .await;
    assert_eq!(rejected(&d).0, RejectCode::Forbidden);
    let off = (
        RejectCode::InvalidState,
        "the deck link has no coordinator_live.v1".to_owned(),
    );
    rig.bindings().set_deck(2, Arc::new(Link { live: false }));
    let d = rig
        .submit(CommandOrigin::Board, deck.clone(), command.clone())
        .await;
    assert_eq!(rejected(&d), off);
    rig.bindings().clear_deck(2);
    let d = rig.submit(CommandOrigin::Board, deck, command).await;
    assert_eq!(rejected(&d), off);
    assert_eq!(rig.record().await.unwrap().seq, 2);
}

/// The record of `grant` with the classes of `names`, as the deck gives it.
fn give(grant: &str, names: &[&str]) -> CoordinatorSpec {
    spec(grant, names)
}

/// Memory and, on the shim host, the stored row show the same grant.
async fn assert_grant(rig: &Rig, digest: &str, shim: bool, expect: &Binding) {
    let got = rig.memory(digest);
    assert_eq!(got.binding_id, expect.binding_id);
    assert_eq!(got.grant_id, expect.grant_id);
    assert_eq!(got.classes, expect.classes);
    assert_eq!(got.generation, expect.generation);
    assert_eq!(got.state, expect.state);
    assert_eq!(got.native_id, expect.native_id);
    assert_eq!(got.kind, expect.kind);
    assert_eq!(got.scope, expect.scope);
    assert_eq!(got.control_skill, expect.control_skill);
    if shim {
        let row = rig.row(digest).await;
        let stored = row.to_binding().unwrap();
        assert_eq!(stored.binding_id, got.binding_id);
        assert_eq!(stored.grant_id, got.grant_id);
        assert_eq!(stored.classes, got.classes);
        assert_eq!(stored.generation, got.generation);
        assert_eq!(stored.native_id, got.native_id);
        assert_eq!(stored.kind, got.kind);
        assert_eq!(stored.scope, got.scope);
        assert_eq!(stored.control_skill, got.control_skill);
        let expires = if got.grant_id.is_empty() {
            None
        } else {
            Some(FAR)
        };
        assert_eq!(row.expires_at, expires);
        assert!(matches!(row.state, RowState::Pending | RowState::Active));
    }
}

#[tokio::test]
async fn a_set_rewrites_the_running_binding_in_place_on_both_hosts() {
    for shim in [false, true] {
        let mut rig = Rig::new(shim).await;
        let row = shim.then_some(RowState::Active);
        let (token, digest) = rig.run(&fleet(BindingState::Active), row).await;
        let StreamAuth::Ok { mut handle, .. } = rig
            .bindings()
            .authenticate_stream(Some(&format!("Bearer {token}")), StreamKind::Listen)
        else {
            panic!("the stream was not admitted");
        };
        assert!(!handle.changes.has_changed().unwrap());

        // A give to the fleet binding: the same token, the new classes, a
        // signal, a bound event at generation 1.
        let mut expect = binding("s1:g-1", "g-1", BindingState::Active, 1);
        expect.classes = classes(&[ToolClass::Read, ToolClass::Mail]);
        expect.control_skill = "You coordinate the fleet.".to_owned();
        assert_eq!(
            accepted(
                &rig.set(1, "g-1", Some(&give("g-1", &["read", "mail"])))
                    .await
            ),
            Some(1)
        );
        assert_eq!(rig.authenticated(&token).classes, expect.classes);
        assert_grant(&rig, &digest, shim, &expect).await;
        assert!(
            handle.changes.has_changed().unwrap(),
            "the change signal bumped"
        );
        handle.changes.borrow_and_update();
        assert_eq!(rig.events().await, ["bound:s1:g-1@1"]);
        assert_eq!(rig.record().await.unwrap().generation, 1);

        // A change of the same grant: no event, the generation stays.
        expect.classes = classes(&[ToolClass::Read, ToolClass::Mail, ToolClass::Drive]);
        let wide = give("g-1", &["read", "mail", "drive"]);
        assert_eq!(accepted(&rig.set(2, "g-1", Some(&wide)).await), Some(2));
        assert_eq!(rig.authenticated(&token).classes, expect.classes);
        assert_grant(&rig, &digest, shim, &expect).await;
        assert!(handle.changes.has_changed().unwrap());
        handle.changes.borrow_and_update();
        assert_eq!(rig.events().await, ["bound:s1:g-1@1"]);

        // A change of the sequence only: nothing to rewrite.
        assert_eq!(accepted(&rig.set(3, "g-1", Some(&wide)).await), Some(3));
        assert!(
            !handle.changes.has_changed().unwrap(),
            "an unchanged binding is not signalled"
        );
        assert_eq!(rig.record().await.unwrap().seq, 3);

        // A binding that is out of sync is brought in line, row and memory.
        rig.bindings()
            .update_digest(&digest, |b| b.classes = classes(&[ToolClass::Read]));
        assert_eq!(accepted(&rig.set(4, "g-1", Some(&wide)).await), Some(4));
        assert_grant(&rig, &digest, shim, &expect).await;
        handle.changes.borrow_and_update();

        // The end downgrades to the fleet form and keeps the token.
        assert_eq!(accepted(&rig.set(5, "g-1", None).await), Some(5));
        let down = fleet(BindingState::Active);
        assert_eq!(rig.authenticated(&token).classes, down.classes);
        assert_grant(&rig, &digest, shim, &down).await;
        assert!(handle.changes.has_changed().unwrap());
        assert!(handle.changes.has_changed().is_ok(), "the watch stays open");
        assert_eq!(
            rig.events().await,
            ["bound:s1:g-1@1", "ended:s1:g-1@1:revoked"]
        );
        let r = rig.record().await.unwrap();
        assert_eq!(
            (r.state, r.ended_reason.as_deref()),
            (RecordState::Ended, Some("revoked"))
        );
        // A delayed end of the grant changes nothing more.
        assert_eq!(accepted(&rig.set(5, "g-1", None).await), Some(5));
        assert_eq!(rig.events().await.len(), 2);
    }
}

#[tokio::test]
async fn a_pending_binding_stays_pending_and_a_worker_binding_is_replaced() {
    for shim in [false, true] {
        let mut rig = Rig::new(shim).await;
        let row = shim.then_some(RowState::Pending);
        let (token, digest) = rig.run(&fleet(BindingState::Pending), row).await;
        let mut expect = binding("s1:g-1", "g-1", BindingState::Pending, 1);
        expect.classes = classes(&[ToolClass::Read]);
        expect.control_skill = "You coordinate the fleet.".to_owned();
        accepted(&rig.set(1, "g-1", Some(&give("g-1", &["read"]))).await);
        assert_grant(&rig, &digest, shim, &expect).await;
        assert!(
            rig.events().await.is_empty(),
            "a pending binding is not bound yet"
        );
        assert_eq!(rig.authenticated(&token).grant_id, "g-1");
        assert_eq!(rig.memory(&digest).state, BindingState::Pending);

        // A worker binding that a give replaces: it ends with `replaced`.
        let mut rig = Rig::new(shim).await;
        let mut worker = binding("s1:gw", "gw", BindingState::Active, 2);
        worker.kind = GrantKind::Worker;
        worker.classes = classes(&[ToolClass::Mail]);
        let row = shim.then_some(RowState::Active);
        let (_, digest) = rig.run(&worker, row).await;
        accepted(&rig.set(1, "g-1", Some(&give("g-1", &["read"]))).await);
        let mut expect = binding("s1:g-1", "g-1", BindingState::Active, 1);
        expect.classes = classes(&[ToolClass::Read]);
        expect.control_skill = "You coordinate the fleet.".to_owned();
        assert_grant(&rig, &digest, shim, &expect).await;
        assert_eq!(
            rig.events().await,
            ["ended:s1:gw@2:replaced", "bound:s1:g-1@1"]
        );
    }
}

#[path = "coordinator_set_skip_tests.rs"]
mod skip;
