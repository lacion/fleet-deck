//! The start order of the shim bindings (DL-194 T4b): the fence, the rows
//! into memory, the replay hold and the rows of shims that are not
//! reattached.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Event, EventBody};
use fleetdeck_testkit::Det;

use super::*;
use crate::coordinator::bindings::Auth;
use crate::coordinator::{ToolClass, mint};
use crate::store::{NewSession, OfflineApprovalPolicy, ReadResult, StoreConfig};

struct Rig {
    store: Store,
    bindings: Bindings,
    tmp: tempfile::TempDir,
    tokens: BTreeMap<String, String>,
}

fn binding(id: &str, grant: &str, state: BindingState) -> Binding {
    Binding {
        binding_id: id.to_owned(),
        grant_id: grant.to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: (state == BindingState::Active).then(|| "n1".to_owned()),
        generation: u32::from(state == BindingState::Active),
        classes: [ToolClass::Read, ToolClass::Own].into_iter().collect(),
        scope: GrantScope::Lineage,
        state,
        control_skill: "SKILL".to_owned(),
    }
}

impl Rig {
    async fn new() -> Self {
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
        Self {
            store,
            bindings: Bindings::default(),
            tmp: tempfile::tempdir().unwrap(),
            tokens: BTreeMap::new(),
        }
    }

    fn run_dir(&self) -> PathBuf {
        self.tmp.path().join("run")
    }

    fn fence_path(&self) -> PathBuf {
        self.run_dir().join("bindings.fence")
    }

    /// Stores a row for a new real credential; returns its digest.
    async fn row(
        &mut self,
        name: &str,
        shim: &str,
        grant: &str,
        state: BindingState,
        expires_at: Option<i64>,
    ) -> String {
        let (token, digest) = mint().unwrap();
        self.tokens.insert(digest.clone(), token.into_exposed());
        let b = binding(&format!("b-{name}"), grant, state);
        self.store
            .shim_binding_insert(BindingRow::from_binding(
                digest.clone(),
                shim.to_owned(),
                &b,
                expires_at,
                1,
            ))
            .await
            .unwrap();
        digest
    }

    async fn prepare(&self) -> Held {
        prepare(&self.store, &self.bindings, self.run_dir())
            .await
            .unwrap()
    }

    fn fence_text(&self) -> String {
        std::fs::read_to_string(self.fence_path()).unwrap()
    }

    fn state(&self, digest: &str) -> Option<BindingState> {
        self.bindings.get_digest(digest).map(|b| b.state)
    }

    fn auth(&self, digest: &str) -> &'static str {
        let token = &self.tokens[digest];
        match self.bindings.authenticate(Some(&format!("Bearer {token}"))) {
            Auth::Unauthorized => "unauthorized",
            Auth::Limited => "limited",
            Auth::Starting => "starting",
            Auth::Ok { .. } => "ok",
        }
    }

    async fn row_states(&self) -> Vec<(String, RowState)> {
        self.store
            .shim_bindings_load()
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.digest, r.state))
            .collect()
    }

    async fn ended(&self) -> Vec<(String, String)> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 1000).await.unwrap()
        else {
            panic!("no events");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::CoordinatorEnded {
                    binding_id, reason, ..
                }) => Some((binding_id, reason)),
                _ => None,
            })
            .collect()
    }

    fn write_fence(&self, content: &str) {
        std::fs::create_dir_all(self.run_dir()).unwrap();
        std::fs::write(self.fence_path(), content).unwrap();
    }
}

#[tokio::test]
async fn a_missing_fence_without_rows_is_created_with_the_header_only() {
    let rig = Rig::new().await;
    let held = rig.prepare().await;
    assert!(held.is_empty());
    assert_eq!(rig.fence_text(), "fleetdeck-fence 1\n");
    assert!(rig.bindings.fence().is_some());
}

#[tokio::test]
async fn a_leftover_tmp_file_is_deleted_at_the_start() {
    let rig = Rig::new().await;
    rig.write_fence("fleetdeck-fence 1\n");
    let tmp = rig.run_dir().join("bindings.fence.tmp");
    std::fs::write(&tmp, "x").unwrap();
    rig.prepare().await;
    assert!(!tmp.exists());
}

#[tokio::test]
async fn open_rows_load_with_their_state_and_are_held_until_released() {
    let mut rig = Rig::new().await;
    let active = rig
        .row("a", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    let pending = rig
        .row("p", "shim-2", "g2", BindingState::Pending, Some(i64::MAX))
        .await;
    let fleet = rig.row("f", "shim-1", "", BindingState::Active, None).await;
    rig.write_fence("fleetdeck-fence 1\n");
    let held = rig.prepare().await;
    assert_eq!(rig.state(&active), Some(BindingState::Active));
    assert_eq!(rig.state(&pending), Some(BindingState::Pending));
    assert_eq!(rig.state(&fleet), Some(BindingState::Active));
    let mut shim1 = held["shim-1"].clone();
    shim1.sort();
    let mut want = vec![active.clone(), fleet.clone()];
    want.sort();
    assert_eq!(shim1, want);
    assert_eq!(held["shim-2"], std::slice::from_ref(&pending));
    for d in [&active, &pending, &fleet] {
        assert!(rig.bindings.is_held(d));
        assert_eq!(rig.auth(d), "starting");
    }
    rig.bindings.release(&held["shim-1"]);
    assert_eq!(rig.auth(&active), "ok");
    assert_eq!(rig.auth(&pending), "starting");
    // The rows stay as they were.
    assert!(
        rig.row_states()
            .await
            .iter()
            .all(|(_, s)| matches!(s, RowState::Active | RowState::Pending))
    );
}

#[tokio::test]
async fn a_fenced_row_loads_revoked_and_becomes_suspended() {
    let mut rig = Rig::new().await;
    let fenced = rig
        .row("a", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    let other = rig
        .row("b", "shim-1", "g2", BindingState::Active, Some(i64::MAX))
        .await;
    rig.write_fence(&format!("fleetdeck-fence 1\nshim-1 {fenced}\n"));
    let held = rig.prepare().await;
    assert_eq!(rig.state(&fenced), Some(BindingState::Revoked));
    assert!(!rig.bindings.is_held(&fenced));
    assert_eq!(rig.auth(&fenced), "unauthorized");
    assert_eq!(held["shim-1"], std::slice::from_ref(&other));
    let states: BTreeMap<_, _> = rig.row_states().await.into_iter().collect();
    assert_eq!(states[&fenced], RowState::Suspended);
    assert_eq!(states[&other], RowState::Active);
    // The fence file is not rewritten by the load.
    assert!(rig.fence_text().contains(&fenced));
}

#[tokio::test]
async fn an_entry_for_another_shim_does_not_fence_the_row() {
    let mut rig = Rig::new().await;
    let d = rig
        .row("a", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    rig.write_fence(&format!("fleetdeck-fence 1\nshim-9 {d}\n"));
    rig.prepare().await;
    assert_eq!(rig.state(&d), Some(BindingState::Active));
}

#[tokio::test]
async fn an_unreadable_fence_suspends_every_row_then_is_rewritten_header_only() {
    let mut rig = Rig::new().await;
    let a = rig
        .row("a", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    let p = rig
        .row("p", "shim-2", "g2", BindingState::Pending, Some(i64::MAX))
        .await;
    let ended = rig
        .row("e", "shim-2", "g3", BindingState::Revoked, Some(i64::MAX))
        .await;
    rig.write_fence("not a fence\n");
    let held = rig.prepare().await;
    assert!(held.is_empty());
    for d in [&a, &p] {
        assert_eq!(rig.state(d), Some(BindingState::Revoked));
        assert_eq!(rig.auth(d), "unauthorized");
    }
    // An ended row has no entry in memory.
    assert_eq!(rig.state(&ended), None);
    let states: BTreeMap<_, _> = rig.row_states().await.into_iter().collect();
    assert_eq!(states[&a], RowState::Suspended);
    assert_eq!(states[&p], RowState::Suspended);
    assert_eq!(states[&ended], RowState::Ended);
    assert_eq!(rig.fence_text(), "fleetdeck-fence 1\n");
}

#[tokio::test]
async fn a_missing_fence_with_rows_suspends_every_row_and_creates_the_fence() {
    let mut rig = Rig::new().await;
    let a = rig
        .row("a", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    let held = rig.prepare().await;
    assert!(held.is_empty());
    assert_eq!(rig.state(&a), Some(BindingState::Revoked));
    assert_eq!(rig.row_states().await[0].1, RowState::Suspended);
    assert_eq!(rig.fence_text(), "fleetdeck-fence 1\n");
}

#[tokio::test]
async fn a_suspended_row_stays_refused_and_an_ended_row_has_no_entry() {
    let mut rig = Rig::new().await;
    let s = rig
        .row("s", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    rig.store
        .shim_binding_set_state(s.clone(), RowState::Suspended)
        .await
        .unwrap();
    rig.write_fence("fleetdeck-fence 1\n");
    let held = rig.prepare().await;
    assert!(held.is_empty());
    assert_eq!(rig.state(&s), Some(BindingState::Revoked));
    assert_eq!(rig.auth(&s), "unauthorized");
}

#[tokio::test]
async fn an_expired_row_loads_expired_ends_and_appends_one_expired_event() {
    let mut rig = Rig::new().await;
    let now = rig.store.now_ms();
    let expired = rig
        .row("x", "shim-1", "g1", BindingState::Active, Some(now))
        .await;
    let fleet_only = rig
        .row("f", "shim-1", "", BindingState::Active, Some(now - 1))
        .await;
    let live = rig
        .row(
            "l",
            "shim-1",
            "g2",
            BindingState::Active,
            Some(now + 60_000),
        )
        .await;
    rig.write_fence("fleetdeck-fence 1\n");
    let held = rig.prepare().await;
    assert_eq!(rig.state(&expired), Some(BindingState::Expired));
    assert_eq!(rig.state(&fleet_only), Some(BindingState::Expired));
    assert_eq!(rig.auth(&expired), "unauthorized");
    assert!(!rig.bindings.is_held(&expired));
    assert_eq!(held["shim-1"], std::slice::from_ref(&live));
    let states: BTreeMap<_, _> = rig.row_states().await.into_iter().collect();
    assert_eq!(states[&expired], RowState::Ended);
    assert_eq!(states[&fleet_only], RowState::Ended);
    assert_eq!(states[&live], RowState::Active);
    // The fleet-only binding has no event.
    assert_eq!(
        rig.ended().await,
        [("b-x".to_owned(), "expired".to_owned())]
    );
}

#[tokio::test]
async fn a_row_that_does_not_parse_loads_revoked_with_no_classes_and_is_suspended() {
    let rig = Rig::new().await;
    let (_, digest) = mint().unwrap();
    let mut bad = BindingRow::from_binding(
        digest.clone(),
        "shim-1".to_owned(),
        &binding("b-bad", "g1", BindingState::Active),
        Some(i64::MAX),
        1,
    );
    bad.classes = "[\"nope\"]".to_owned();
    rig.store.shim_binding_insert(bad).await.unwrap();
    rig.write_fence("fleetdeck-fence 1\n");
    let held = rig.prepare().await;
    assert!(held.is_empty());
    let loaded = rig.bindings.get_digest(&digest).unwrap();
    assert_eq!(loaded.state, BindingState::Revoked);
    assert!(loaded.classes.is_empty());
    assert_eq!(loaded.scope, GrantScope::Lineage);
    assert_eq!(loaded.control_skill, "");
    assert_eq!(rig.row_states().await[0].1, RowState::Suspended);
}

#[tokio::test]
async fn rows_of_a_shim_that_is_not_reattached_are_deleted_with_a_process_exit_event() {
    let mut rig = Rig::new().await;
    let kept = rig
        .row("k", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    let lost = rig
        .row("l", "shim-2", "g2", BindingState::Active, Some(i64::MAX))
        .await;
    let lost_fleet = rig
        .row("lf", "shim-2", "", BindingState::Pending, None)
        .await;
    rig.write_fence("fleetdeck-fence 1\n");
    rig.prepare().await;
    drop_unattached(&rig.store, &rig.bindings, vec!["shim-1".to_owned()])
        .await
        .unwrap();
    assert_eq!(rig.state(&lost), Some(BindingState::Revoked));
    assert_eq!(rig.state(&lost_fleet), Some(BindingState::Revoked));
    assert!(!rig.bindings.is_held(&lost));
    assert!(!rig.bindings.is_held(&lost_fleet));
    assert_eq!(rig.auth(&lost), "unauthorized");
    // The reattached shim stays held.
    assert!(rig.bindings.is_held(&kept));
    assert_eq!(
        rig.row_states()
            .await
            .into_iter()
            .map(|(d, _)| d)
            .collect::<Vec<_>>(),
        [kept]
    );
    assert_eq!(
        rig.ended().await,
        [("b-l".to_owned(), "process_exit".to_owned())]
    );
}

#[tokio::test]
async fn fence_entries_of_suspended_ended_or_missing_rows_are_pruned_after_the_start() {
    let mut rig = Rig::new().await;
    let fenced = rig
        .row("a", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    let gone = rig
        .row("g", "shim-2", "g2", BindingState::Active, Some(i64::MAX))
        .await;
    let ghost = "f".repeat(64);
    rig.write_fence(&format!(
        "fleetdeck-fence 1\nshim-1 {fenced}\nshim-2 {gone}\nshim-3 {ghost}\n"
    ));
    let held = rig.prepare().await;
    finish(&rig.store, &rig.bindings, &[], held).await.unwrap();
    assert_eq!(rig.fence_text(), "fleetdeck-fence 1\n");
}

#[tokio::test]
async fn a_fence_entry_of_a_row_that_is_still_open_stays() {
    let mut rig = Rig::new().await;
    let open = rig
        .row("a", "shim-1", "g1", BindingState::Active, Some(i64::MAX))
        .await;
    rig.write_fence(&format!("fleetdeck-fence 1\nshim-1 {open}\n"));
    // The load would suspend it; prune before that shows the guard.
    let fence = Fence::new(rig.run_dir());
    fence.prune(&rig.store).await;
    assert!(rig.fence_text().contains(&open));
}
