//! The owner's revoke of a shim-hosted binding (DL-194 T4b): the fence
//! entry comes inside the admitted decision, before the row change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CommandResult, GrantScope, RETRY_WINDOW_MS, ReceiptState,
    SessionCommand,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandId, Event, EventBody, RejectCode};
use fleetdeck_testkit::Det;

use super::*;
use crate::coordinator::fence::{Fence, FenceState};
use crate::coordinator::{Binding, BindingState, ToolClass, mint};
use crate::store::shim_bindings::{BindingRow, RowState};
use crate::store::{NewSession, OfflineApprovalPolicy, ReadResult, StoreConfig};

const NOT_SAVED: &str =
    "The revoke was not saved. The old token is blocked until Fleet Deck restarts. Try again.";

struct Rig {
    det: Det,
    manager: SessionManager,
    fence: Arc<Fence>,
    tmp: tempfile::TempDir,
    deck: Caller,
    next: u32,
}

fn binding() -> Binding {
    Binding {
        binding_id: "b-1".to_owned(),
        grant_id: "g-1".to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("n1".to_owned()),
        generation: 3,
        classes: [ToolClass::Read, ToolClass::Own].into_iter().collect(),
        scope: GrantScope::Lineage,
        state: BindingState::Active,
        control_skill: String::new(),
    }
}

/// A paired deck with `drive` (the revoke is an owner command from the deck).
async fn pair_deck(manager: &SessionManager) -> Caller {
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
    Caller::peer(key.fingerprint(), Scope::Drive)
}

impl Rig {
    async fn new() -> Self {
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
        let fence = Arc::new(Fence::new(tmp.path().join("run")));
        fence.ensure().unwrap();
        manager.coordinator_bindings().set_fence(Arc::clone(&fence));
        let deck = pair_deck(&manager).await;
        Self {
            det,
            manager,
            fence,
            tmp,
            deck,
            next: 0,
        }
    }

    /// Stores and loads the binding; returns its digest and shim id.
    async fn bind(&self, state: RowState) -> (String, String) {
        let (_, digest) = mint().unwrap();
        let b = binding();
        self.manager
            .coordinator_bindings()
            .insert(digest.clone(), b.clone());
        let mut row =
            BindingRow::from_binding(digest.clone(), "shim1".to_owned(), &b, Some(i64::MAX), 1);
        row.state = state;
        self.manager.store().shim_binding_insert(row).await.unwrap();
        (digest, "shim1".to_owned())
    }

    async fn revoke(&mut self) -> Decided {
        self.next += 1;
        let caller = self.deck.clone();
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("revoke-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::CoordinatorRevoke {
                session_id: "s1".into(),
                grant_id: "g-1".to_owned(),
            },
        };
        self.manager.submit(envelope, caller).await.unwrap()
    }

    /// The deck renews the grant `g-1` at `generation`.
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
                grant_id: "g-1".to_owned(),
                generation,
            },
        };
        self.manager
            .submit(envelope, self.deck.clone())
            .await
            .unwrap()
    }

    async fn row_state(&self) -> RowState {
        self.manager.store().shim_bindings_load().await.unwrap()[0].state
    }

    async fn reasons(&self) -> Vec<String> {
        let ReadResult::Events { events, .. } =
            self.manager.store().read_after(0, 1000).await.unwrap()
        else {
            panic!("no events");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::CoordinatorEnded {
                    reason, generation, ..
                }) => Some(format!("{reason}@{generation}")),
                _ => None,
            })
            .collect()
    }

    fn memory(&self, digest: &str) -> BindingState {
        self.manager
            .coordinator_bindings()
            .get_digest(digest)
            .unwrap()
            .state
    }

    fn entries(&self) -> usize {
        match self.fence.read() {
            FenceState::Entries(e) => e.len(),
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn a_revoke_ends_the_row_appends_the_event_and_settles_the_fence_entry() {
    let mut rig = Rig::new().await;
    let (digest, _) = rig.bind(RowState::Active).await;
    let decided = rig.revoke().await;
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    assert_eq!(rig.row_state().await, RowState::Ended);
    assert_eq!(rig.memory(&digest), BindingState::Revoked);
    assert_eq!(rig.reasons().await, ["revoked@3"]);
    assert_eq!(rig.entries(), 0, "the entry is removed after the commit");
    // A second revoke is a success with no new event.
    let again = rig.revoke().await;
    assert_eq!(again.receipt.state, ReceiptState::Accepted);
    assert_eq!(rig.reasons().await, ["revoked@3"]);
}

#[tokio::test]
async fn the_fence_entry_is_in_the_file_before_the_row_change_and_settle_removes_it() {
    let rig = Rig::new().await;
    let (digest, shim) = rig.bind(RowState::Active).await;
    let bindings = rig.manager.coordinator_bindings().clone();
    // Decide without the manager, so that `settle` has not run.
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from("direct"),
        target_incarnation: None,
        retry_until: i64::try_from(rig.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::CoordinatorRevoke {
            session_id: "s1".into(),
            grant_id: "g-1".to_owned(),
        },
    };
    rig.manager
        .store()
        .decide(envelope, Caller::local(), move |d| {
            super::coordinator_command::revoke(
                d,
                &bindings,
                CommandOrigin::Board,
                &"s1".into(),
                "g-1",
            )
        })
        .await
        .unwrap();
    assert_eq!(rig.row_state().await, RowState::Ended);
    assert_eq!(
        rig.fence.read(),
        FenceState::Entries(std::collections::BTreeSet::from([(shim, digest)])),
        "the entry stays until settle"
    );
    rig.fence.settle(rig.manager.store()).await;
    assert_eq!(rig.entries(), 0);
}

#[tokio::test]
async fn a_failed_fence_write_rejects_the_revoke_and_keeps_the_gate_and_the_open_row() {
    let mut rig = Rig::new().await;
    let (digest, _) = rig.bind(RowState::Active).await;
    // An unreadable fence takes no write.
    std::fs::write(rig.tmp.path().join("run/bindings.fence"), "junk\n").unwrap();
    let decided = rig.revoke().await;
    assert_eq!(decided.receipt.state, ReceiptState::Rejected);
    let reject = decided.receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::InvalidCommand);
    assert_eq!(reject.message, NOT_SAVED);
    assert_eq!(rig.row_state().await, RowState::Active);
    assert_eq!(rig.memory(&digest), BindingState::Revoked);
    assert!(
        rig.reasons().await.is_empty(),
        "no event, the decision rolled back"
    );
}

#[tokio::test]
async fn a_revoke_of_a_binding_that_memory_already_gated_still_ends_the_open_row() {
    let mut rig = Rig::new().await;
    let (digest, _) = rig.bind(RowState::Pending).await;
    rig.manager
        .coordinator_bindings()
        .update_digest(&digest, |b| b.state = BindingState::Revoked);
    rig.revoke().await;
    assert_eq!(rig.row_state().await, RowState::Ended);
    assert_eq!(rig.reasons().await, ["revoked@3"]);
    assert_eq!(rig.entries(), 0);
}

#[tokio::test]
async fn a_revoke_of_a_suspended_row_is_a_success_with_no_event_and_no_fence_entry() {
    let mut rig = Rig::new().await;
    rig.bind(RowState::Suspended).await;
    let decided = rig.revoke().await;
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    assert_eq!(rig.row_state().await, RowState::Suspended);
    assert!(rig.reasons().await.is_empty());
    assert_eq!(rig.entries(), 0);
}

#[tokio::test]
async fn a_binding_without_a_row_takes_the_direct_path() {
    let mut rig = Rig::new().await;
    let (_, digest) = mint().unwrap();
    rig.manager
        .coordinator_bindings()
        .insert(digest.clone(), binding());
    let decided = rig.revoke().await;
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    assert_eq!(rig.memory(&digest), BindingState::Revoked);
    assert_eq!(rig.reasons().await, ["revoked@3"]);
    assert_eq!(rig.entries(), 0);
}

#[tokio::test]
async fn a_station_without_a_fence_takes_the_direct_path() {
    let rig = Rig::new().await;
    let (digest, _) = rig.bind(RowState::Active).await;
    // The direct host has no fence: a fresh `Bindings` without one.
    let bare = crate::coordinator::Bindings::default();
    assert!(bare.fence().is_none());
    let b = binding();
    bare.insert(digest.clone(), b);
    let sent = rig.manager.store().clone();
    let decided = sent
        .decide(
            CommandEnvelope {
                authority: None,
                command_id: CommandId::from("bare"),
                target_incarnation: None,
                retry_until: i64::try_from(rig.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
                origin: CommandOrigin::Board,
                command: SessionCommand::CoordinatorRevoke {
                    session_id: "s1".into(),
                    grant_id: "g-1".to_owned(),
                },
            },
            Caller::local(),
            move |d| -> Result<CommandResult, crate::store::DecideError> {
                super::coordinator_command::revoke(
                    d,
                    &bare,
                    CommandOrigin::Board,
                    &"s1".into(),
                    "g-1",
                )
            },
        )
        .await
        .unwrap();
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    assert_eq!(
        rig.row_state().await,
        RowState::Active,
        "the row is untouched"
    );
}

const ENDED: &str = "the binding has ended";

fn assert_ended(decided: &Decided) {
    assert_eq!(decided.receipt.state, ReceiptState::Rejected);
    let reject = decided.receipt.reject.clone().unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert_eq!(reject.message, ENDED);
}

#[tokio::test]
async fn an_expired_grant_never_renews_and_a_later_revoke_keeps_it_refused() {
    let mut rig = Rig::new().await;
    // An absolute-expiry row at a station start (DL-194).
    let (_, digest) = mint().unwrap();
    let now = rig.manager.store().now_ms();
    rig.manager
        .store()
        .shim_binding_insert(BindingRow::from_binding(
            digest.clone(),
            "shim1".to_owned(),
            &binding(),
            Some(now),
            1,
        ))
        .await
        .unwrap();
    let bindings = rig.manager.coordinator_bindings().clone();
    crate::manager::binding_start::prepare(
        rig.manager.store(),
        &bindings,
        rig.tmp.path().join("run"),
    )
    .await
    .unwrap();
    assert_eq!(rig.memory(&digest), BindingState::Revoked);
    assert_eq!(rig.row_state().await, RowState::Ended);
    assert_ended(&rig.renew(4).await);
    assert_eq!(rig.memory(&digest), BindingState::Revoked);
    let revoked = rig.revoke().await;
    assert_eq!(revoked.receipt.state, ReceiptState::Accepted);
    assert_eq!(rig.memory(&digest), BindingState::Revoked);
    assert_ended(&rig.renew(4).await);
    // Only the expiry event of the start; the renews and the revoke add none.
    assert_eq!(rig.reasons().await, ["expired@3"]);
}

#[tokio::test]
async fn a_renew_of_an_ended_row_is_rejected_whatever_memory_says() {
    let mut rig = Rig::new().await;
    let (digest, _) = rig.bind(RowState::Ended).await;
    rig.manager
        .coordinator_bindings()
        .update_digest(&digest, |b| b.state = BindingState::Expired);
    assert_ended(&rig.renew(4).await);
    assert_eq!(rig.memory(&digest), BindingState::Expired);
    assert_eq!(rig.row_state().await, RowState::Ended);
}

#[tokio::test]
async fn a_renew_of_an_active_row_with_an_idle_expired_binding_renews() {
    let mut rig = Rig::new().await;
    let (digest, _) = rig.bind(RowState::Active).await;
    rig.manager
        .coordinator_bindings()
        .update_digest(&digest, |b| b.state = BindingState::Expired);
    let decided = rig.renew(4).await;
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    let renewed = rig
        .manager
        .coordinator_bindings()
        .get_digest(&digest)
        .unwrap();
    assert_eq!(renewed.state, BindingState::Active);
    assert_eq!(renewed.generation, 4);
}

#[tokio::test]
async fn a_revoke_of_an_ended_row_gates_memory() {
    let mut rig = Rig::new().await;
    let (digest, _) = rig.bind(RowState::Ended).await;
    assert_eq!(rig.memory(&digest), BindingState::Active);
    let decided = rig.revoke().await;
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    assert_eq!(rig.memory(&digest), BindingState::Revoked);
    assert_eq!(rig.row_state().await, RowState::Ended);
    assert!(rig.reasons().await.is_empty());
}
