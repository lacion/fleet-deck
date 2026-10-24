//! The store functions of the binding start order and the rotation (DL-194
//! T4b): the row form of a binding, expiry, removal of other shims' rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Event, EventBody};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::{NewSession, OfflineApprovalPolicy, ReadResult, StoreConfig};

async fn store(det: &Det) -> Store {
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
    s
}

fn binding(id: &str, grant: &str, state: BindingState) -> Binding {
    Binding {
        binding_id: id.to_owned(),
        grant_id: grant.to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: (state == BindingState::Active).then(|| "n1".to_owned()),
        generation: u32::from(state == BindingState::Active),
        classes: [ToolClass::Read, ToolClass::Inspect, ToolClass::Own]
            .into_iter()
            .collect(),
        scope: GrantScope::Fleet,
        state,
        control_skill: "SKILL \"v1\"\nline two".to_owned(),
        kind: fleetdeck_proto::command::GrantKind::Coordinator,
    }
}

fn row(digest: &str, shim: &str, binding_id: &str, grant: &str, state: BindingState) -> BindingRow {
    BindingRow::from_binding(
        digest.to_owned(),
        shim.to_owned(),
        &binding(binding_id, grant, state),
        (!grant.is_empty()).then_some(5_000),
        7,
    )
}

/// `(binding id, reason)` of every `coordinator.ended` event.
async fn ended(s: &Store) -> Vec<(String, String)> {
    let ReadResult::Events { events, .. } = s.read_after(0, 1000).await.unwrap() else {
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

#[test]
fn a_binding_round_trips_through_its_row() {
    for state in [BindingState::Pending, BindingState::Active] {
        let b = binding("b1", "g1", state);
        let r = BindingRow::from_binding("d".repeat(64), "shim-1".to_owned(), &b, Some(9), 3);
        assert_eq!(r.classes, "[\"read\",\"inspect\"]", "Own is never stored");
        assert_eq!(r.scope, "\"fleet\"");
        assert_eq!(r.control_skill, "\"SKILL \\\"v1\\\"\\nline two\"");
        assert_eq!(
            r.state,
            if state == BindingState::Pending {
                RowState::Pending
            } else {
                RowState::Active
            }
        );
        assert_eq!(r.expires_at, Some(9));
        assert_eq!(r.to_binding().unwrap().classes, b.classes);
        let back = r.to_binding().unwrap();
        assert_eq!(
            (
                back.binding_id,
                back.grant_id,
                back.native_id,
                back.generation
            ),
            (b.binding_id, b.grant_id, b.native_id, b.generation)
        );
        assert_eq!(
            (back.scope, back.state, back.control_skill),
            (b.scope, b.state, b.control_skill)
        );
    }
    let ended = BindingRow::from_binding(
        "d".to_owned(),
        "s".to_owned(),
        &binding("b1", "g1", BindingState::Expired),
        None,
        0,
    );
    assert_eq!(ended.state, RowState::Ended);
    assert_eq!(ended.to_binding().unwrap().state, BindingState::Revoked);
}

#[test]
fn the_recording_allowance_survives_the_row_and_an_old_row_reads_without_it() {
    let mut b = binding("b1", "g1", BindingState::Active);
    b.classes.insert(ToolClass::Capture);
    b.classes.insert(ToolClass::Recording);
    let r = BindingRow::from_binding("d".repeat(64), "shim-1".to_owned(), &b, Some(9), 3);
    assert_eq!(
        r.classes,
        "[\"read\",\"inspect\",\"capture\",\"recording\"]"
    );
    let back = r.to_binding().unwrap();
    assert_eq!(back.classes, b.classes);
    assert!(back.classes.contains(&ToolClass::Recording));
    // An old row holds no `recording`: it reads without the allowance.
    let old = BindingRow {
        classes: "[\"read\",\"capture\"]".to_owned(),
        ..r
    };
    let back = old.to_binding().unwrap();
    assert!(back.classes.contains(&ToolClass::Capture));
    assert!(!back.classes.contains(&ToolClass::Recording));
}

#[test]
fn the_input_allowance_survives_the_row_and_an_old_row_reads_without_it() {
    let mut b = binding("b1", "g1", BindingState::Active);
    for class in [
        ToolClass::Desktop,
        ToolClass::Capture,
        ToolClass::Input,
        ToolClass::InputEnabled,
    ] {
        b.classes.insert(class);
    }
    let r = BindingRow::from_binding("d".repeat(64), "shim-1".to_owned(), &b, Some(9), 3);
    assert_eq!(
        r.classes,
        "[\"read\",\"inspect\",\"desktop\",\"capture\",\"input\",\"input_enabled\"]"
    );
    let back = r.to_binding().unwrap();
    assert_eq!(back.classes, b.classes);
    assert!(back.classes.contains(&ToolClass::InputEnabled));
    // An old row holds no `input_enabled`: it reads without the allowance.
    let old = BindingRow {
        classes: "[\"read\",\"desktop\",\"capture\",\"input\"]".to_owned(),
        ..r
    };
    let back = old.to_binding().unwrap();
    assert!(back.classes.contains(&ToolClass::Input));
    assert!(!back.classes.contains(&ToolClass::InputEnabled));
}

#[test]
fn a_row_with_a_damaged_column_does_not_parse() {
    let good = row("d1", "a", "b1", "g1", BindingState::Active);
    for (column, bad) in [
        (
            "classes",
            BindingRow {
                classes: "[\"nope\"]".to_owned(),
                ..good.clone()
            },
        ),
        (
            "classes",
            BindingRow {
                classes: "[\"own\"]".to_owned(),
                ..good.clone()
            },
        ),
        (
            "classes",
            BindingRow {
                classes: "read".to_owned(),
                ..good.clone()
            },
        ),
        (
            "scope",
            BindingRow {
                scope: "{\"scope\":\"own\"}".to_owned(),
                ..good.clone()
            },
        ),
        (
            "control_skill",
            BindingRow {
                control_skill: "{}".to_owned(),
                ..good.clone()
            },
        ),
    ] {
        assert_eq!(bad.to_binding().unwrap_err(), column);
    }
}

#[tokio::test]
async fn expire_ends_an_open_row_once_with_its_event() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_binding_insert(row("d1", "a", "b1", "g1", BindingState::Active))
        .await
        .unwrap();
    s.shim_binding_insert(row("d2", "a", "b2", "", BindingState::Active))
        .await
        .unwrap();
    s.shim_binding_insert(row("d3", "a", "b3", "g3", BindingState::Expired))
        .await
        .unwrap();
    assert!(s.shim_binding_expire("d1".to_owned()).await.unwrap());
    assert!(!s.shim_binding_expire("d1".to_owned()).await.unwrap());
    assert!(s.shim_binding_expire("d2".to_owned()).await.unwrap());
    assert!(!s.shim_binding_expire("d3".to_owned()).await.unwrap());
    assert!(!s.shim_binding_expire("zz".to_owned()).await.unwrap());
    let states: Vec<_> = s
        .shim_bindings_load()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.state)
        .collect();
    assert_eq!(states, [RowState::Ended; 3]);
    // The fleet-only row gives no event.
    assert_eq!(ended(&s).await, [("b1".to_owned(), "expired".to_owned())]);
}

#[tokio::test]
async fn a_row_of_a_missing_session_ends_without_an_event() {
    let det = Det::new();
    let s = store(&det).await;
    let mut r = row("d1", "a", "b1", "g1", BindingState::Active);
    r.session_id = "gone".into();
    s.shim_binding_insert(r).await.unwrap();
    assert!(s.shim_binding_expire("d1".to_owned()).await.unwrap());
    assert!(ended(&s).await.is_empty());
}

#[tokio::test]
async fn drop_others_deletes_the_rows_of_other_shims_and_ends_their_open_bindings() {
    let det = Det::new();
    let s = store(&det).await;
    for r in [
        row("d1", "keep", "b1", "g1", BindingState::Active),
        row("d2", "gone", "b2", "g2", BindingState::Active),
        row("d3", "gone", "b3", "g3", BindingState::Pending),
        row("d4", "gone", "b4", "", BindingState::Active),
        row("d5", "gone", "b5", "g5", BindingState::Expired),
    ] {
        s.shim_binding_insert(r).await.unwrap();
    }
    s.shim_binding_set_state("d3".to_owned(), RowState::Suspended)
        .await
        .unwrap();
    let dropped = s
        .shim_bindings_drop_others(vec!["keep".to_owned()])
        .await
        .unwrap();
    let digests: Vec<_> = dropped.iter().map(|r| r.digest.as_str()).collect();
    assert_eq!(digests, ["d2", "d3", "d4", "d5"]);
    let left: Vec<_> = s
        .shim_bindings_load()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.digest)
        .collect();
    assert_eq!(left, ["d1"]);
    // Only the open binding with a grant: d2. d3 is suspended, d4 is
    // fleet-only, d5 ended.
    assert_eq!(
        ended(&s).await,
        [("b2".to_owned(), "process_exit".to_owned())]
    );
}

#[tokio::test]
async fn rotate_end_ends_the_open_rows_in_one_transaction_with_rotated_events() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_binding_insert(row("d1", "a", "b1", "g1", BindingState::Active))
        .await
        .unwrap();
    s.shim_binding_insert(row("d2", "a", "b1", "g1", BindingState::Pending))
        .await
        .unwrap();
    s.shim_binding_insert(row("d3", "a", "b1", "g1", BindingState::Expired))
        .await
        .unwrap();
    let open = s.shim_bindings_by_binding("b1".to_owned()).await.unwrap();
    assert_eq!(
        open.iter().map(|r| r.digest.as_str()).collect::<Vec<_>>(),
        ["d1", "d2"]
    );
    assert!(
        s.shim_bindings_by_binding("zz".to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    let n = s
        .shim_bindings_rotate_end(vec!["d1".to_owned(), "d2".to_owned(), "d3".to_owned()])
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert!(
        s.shim_bindings_by_binding("b1".to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ended(&s).await,
        [
            ("b1".to_owned(), "rotated".to_owned()),
            ("b1".to_owned(), "rotated".to_owned())
        ]
    );
}
