//! `rotate_out` (DL-194 T4b): the old credentials end before a resume.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Event, EventBody};
use fleetdeck_testkit::Det;

use super::*;
use crate::coordinator::fence::{Fence, FenceState};
use crate::coordinator::{Binding, BindingState, ToolClass, mint};
use crate::store::shim_bindings::{BindingRow, RowState};
use crate::store::{NewSession, OfflineApprovalPolicy, ReadResult, StoreConfig};

struct Rig {
    store: Store,
    bindings: Bindings,
    tmp: tempfile::TempDir,
}

fn binding(grant: &str) -> Binding {
    Binding {
        binding_id: "b-1".to_owned(),
        grant_id: grant.to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("n1".to_owned()),
        generation: 1,
        classes: [ToolClass::Read, ToolClass::Own].into_iter().collect(),
        scope: GrantScope::Lineage,
        state: BindingState::Active,
        control_skill: String::new(),
    }
}

impl Rig {
    async fn new(with_fence: bool) -> Self {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
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
        let bindings = Bindings::default();
        if with_fence {
            let fence = Arc::new(Fence::new(tmp.path().join("run")));
            fence.ensure().unwrap();
            bindings.set_fence(fence);
        }
        Self {
            store,
            bindings,
            tmp,
        }
    }

    /// A stored and loaded binding of grant `grant`; its digest.
    async fn bind(&self, grant: &str) -> String {
        let (_, digest) = mint().unwrap();
        let b = binding(grant);
        self.bindings.insert(digest.clone(), b.clone());
        self.store
            .shim_binding_insert(BindingRow::from_binding(
                digest.clone(),
                "shim-1".to_owned(),
                &b,
                (!grant.is_empty()).then_some(i64::MAX),
                1,
            ))
            .await
            .unwrap();
        digest
    }

    async fn rows(&self) -> Vec<RowState> {
        self.store
            .shim_bindings_load()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.state)
            .collect()
    }

    async fn reasons(&self) -> Vec<String> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 1000).await.unwrap()
        else {
            panic!("no events");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::CoordinatorEnded { reason, .. }) => Some(reason),
                _ => None,
            })
            .collect()
    }
}

#[tokio::test]
async fn the_open_rows_end_as_rotated_the_fence_is_empty_after_and_memory_refuses_the_old_digest() {
    let rig = Rig::new(true).await;
    let digest = rig.bind("g1").await;
    rotate_out(&rig.store, &rig.bindings, "b-1").await.unwrap();
    assert_eq!(rig.rows().await, [RowState::Ended]);
    assert_eq!(rig.reasons().await, ["rotated"]);
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
    assert_eq!(
        rig.bindings.fence().unwrap().read(),
        FenceState::Entries(std::collections::BTreeSet::new())
    );
}

#[tokio::test]
async fn a_fleet_only_row_ends_without_an_event() {
    let rig = Rig::new(true).await;
    rig.bind("").await;
    rotate_out(&rig.store, &rig.bindings, "b-1").await.unwrap();
    assert_eq!(rig.rows().await, [RowState::Ended]);
    assert!(rig.reasons().await.is_empty());
}

#[tokio::test]
async fn no_open_row_is_a_success_that_changes_nothing() {
    let rig = Rig::new(false).await;
    // Not even a fence is needed.
    rotate_out(&rig.store, &rig.bindings, "b-1").await.unwrap();
    let digest = rig.bind("g1").await;
    rig.store
        .shim_binding_set_state(digest.clone(), RowState::Suspended)
        .await
        .unwrap();
    rotate_out(&rig.store, &rig.bindings, "b-1").await.unwrap();
    assert_eq!(rig.rows().await, [RowState::Suspended]);
    assert!(rig.reasons().await.is_empty());
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Active,
        "a rotation with no open row does not gate memory"
    );
}

#[tokio::test]
async fn without_a_fence_the_rotation_fails_with_the_fixed_text_and_the_gate_stays() {
    let rig = Rig::new(false).await;
    let digest = rig.bind("g1").await;
    let err = rotate_out(&rig.store, &rig.bindings, "b-1")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "The station cannot save the session credentials. Try again."
    );
    assert_eq!(rig.rows().await, [RowState::Active]);
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
}

#[tokio::test]
async fn an_unreadable_fence_fails_the_rotation_and_leaves_the_row_open() {
    let rig = Rig::new(true).await;
    let digest = rig.bind("g1").await;
    std::fs::write(rig.tmp.path().join("run/bindings.fence"), "junk\n").unwrap();
    let err = rotate_out(&rig.store, &rig.bindings, "b-1")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "The station cannot save the session credentials. Try again."
    );
    assert_eq!(rig.rows().await, [RowState::Active]);
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
}
