//! MCP P4b §5: the store functions that end a binding row leave the grant of
//! an `attached` record alone, and the writes of the record at a start.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::command::{CoordinatorSpec, GrantKind, GrantScope};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Event, EventBody};
use fleetdeck_testkit::Det;

use super::*;
use crate::coordinator::{Bindings, ToolClass};
use crate::store::session_grants::{GrantRecord, RecordState};
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

fn row(digest: &str, binding_id: &str, grant: &str, expires_at: i64) -> BindingRow {
    let b = Binding {
        binding_id: binding_id.to_owned(),
        grant_id: grant.to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("n1".to_owned()),
        generation: 2,
        classes: [ToolClass::Read, ToolClass::Own].into_iter().collect(),
        scope: GrantScope::Lineage,
        state: BindingState::Active,
        control_skill: String::new(),
        kind: GrantKind::Coordinator,
    };
    BindingRow::from_binding(
        digest.to_owned(),
        "shim-1".to_owned(),
        &b,
        (!grant.is_empty()).then_some(expires_at),
        7,
    )
}

fn grant_spec() -> CoordinatorSpec {
    serde_json::from_value(serde_json::json!({
        "grant_id": "g1", "classes": ["read"], "idle_ttl_ms": 60_000,
        "expires_at": 5_000,
        "control_skill": {"version": "1", "text": "skill"}
    }))
    .unwrap()
}

/// Stores an `attached` record of the grant `g1` at generation 2.
async fn attach(s: &Store) {
    let envelope = fleetdeck_proto::CommandEnvelope {
        authority: None,
        command_id: fleetdeck_proto::CommandId::from("put"),
        target_incarnation: None,
        retry_until: s.now_ms() + fleetdeck_proto::command::RETRY_WINDOW_MS,
        origin: fleetdeck_proto::command::CommandOrigin::Board,
        command: fleetdeck_proto::command::SessionCommand::CoordinatorRevoke {
            session_id: "s1".into(),
            grant_id: "x".to_owned(),
        },
    };
    s.decide(envelope, crate::store::Caller::local(), |d| {
        d.session_grant_put(&GrantRecord {
            session_id: "s1".into(),
            grant_id: "g1".to_owned(),
            seq: 1,
            spec: Some(grant_spec()),
            generation: 2,
            state: RecordState::Attached,
            ended_reason: None,
            updated_at: 1,
        })?;
        Ok(fleetdeck_proto::command::CommandResult {
            session_id: Some("s1".into()),
            input_id: None,
            seq: None,
        })
    })
    .await
    .unwrap();
}

async fn ended(s: &Store) -> Vec<(String, u32, String)> {
    let ReadResult::Events { events, .. } = s.read_after(0, 1000).await.unwrap() else {
        panic!("no events");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(Event::CoordinatorEnded {
                binding_id,
                generation,
                reason,
            }) => Some((binding_id, generation, reason)),
            _ => None,
        })
        .collect()
}

async fn record(s: &Store) -> GrantRecord {
    s.session_grant("s1".into()).await.unwrap().unwrap()
}

#[tokio::test]
async fn the_generation_and_the_end_of_a_record_are_own_writes() {
    let det = Det::new();
    let s = store(&det).await;
    // No record: nothing to write.
    assert!(!s.session_grant_generation("s1".into(), 3).await.unwrap());
    assert!(
        !s.session_grant_end("s1".into(), "expired".to_owned())
            .await
            .unwrap()
    );
    attach(&s).await;
    assert!(s.session_grant_generation("s1".into(), 3).await.unwrap());
    assert_eq!(record(&s).await.generation, 3);
    // The end stores the reason and the one event, with the generation of
    // the record, in one write; a second end does nothing.
    assert!(
        s.session_grant_end("s1".into(), "expired".to_owned())
            .await
            .unwrap()
    );
    let r = record(&s).await;
    assert_eq!(
        (r.state, r.ended_reason.as_deref(), r.generation, r.seq),
        (RecordState::Ended, Some("expired"), 3, 1)
    );
    assert_eq!(
        ended(&s).await,
        [("s1:g1".to_owned(), 3, "expired".to_owned())]
    );
    assert!(
        !s.session_grant_end("s1".into(), "revoked".to_owned())
            .await
            .unwrap()
    );
    assert!(
        !s.session_grant_generation("s1".into(), 4).await.unwrap(),
        "an ended record takes no generation"
    );
    assert_eq!(ended(&s).await.len(), 1);
}

#[tokio::test]
async fn a_rotation_and_a_drop_end_the_rows_of_a_record_grant_without_an_event() {
    let det = Det::new();
    let s = store(&det).await;
    attach(&s).await;
    // d1 and d4 belong to the grant of the record; d2 is another grant.
    for r in [
        row("d1", "s1:g1", "g1", 5_000),
        row("d2", "s1:g2", "g2", 5_000),
        row("d3", "s1:fleet", "", 5_000),
    ] {
        s.shim_binding_insert(r).await.unwrap();
    }
    assert_eq!(
        s.shim_bindings_rotate_end(vec!["d1".to_owned(), "d2".to_owned(), "d3".to_owned()])
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        ended(&s).await,
        [("s1:g2".to_owned(), 2, "rotated".to_owned())]
    );
    let mut dropped = row("d4", "s1:g1", "g1", 5_000);
    dropped.shim_id = "gone".to_owned();
    let mut dropped_other = row("d5", "s1:g2", "g2", 5_000);
    dropped_other.shim_id = "gone".to_owned();
    s.shim_binding_insert(dropped).await.unwrap();
    s.shim_binding_insert(dropped_other).await.unwrap();
    s.shim_bindings_drop_others(vec!["shim-1".to_owned()])
        .await
        .unwrap();
    assert_eq!(
        ended(&s).await,
        [
            ("s1:g2".to_owned(), 2, "rotated".to_owned()),
            ("s1:g2".to_owned(), 2, "process_exit".to_owned())
        ]
    );
    assert_eq!(record(&s).await.state, RecordState::Attached);
}

#[tokio::test]
async fn an_expired_row_ends_the_record_with_the_one_expired_event() {
    let det = Det::new();
    let s = store(&det).await;
    attach(&s).await;
    s.shim_binding_insert(row("d1", "s1:g1", "g1", 5_000))
        .await
        .unwrap();
    assert!(s.shim_binding_expire("d1".to_owned()).await.unwrap());
    let r = record(&s).await;
    assert_eq!(
        (r.state, r.ended_reason.as_deref()),
        (RecordState::Ended, Some("expired"))
    );
    assert_eq!(
        ended(&s).await,
        [("s1:g1".to_owned(), 2, "expired".to_owned())]
    );
    // The row of another grant leaves an attached record alone.
    let s2 = store(&det).await;
    attach(&s2).await;
    s2.shim_binding_insert(row("d2", "s1:g2", "g2", 5_000))
        .await
        .unwrap();
    assert!(s2.shim_binding_expire("d2".to_owned()).await.unwrap());
    assert_eq!(record(&s2).await.state, RecordState::Attached);
}

#[tokio::test]
async fn the_station_start_ends_the_record_of_an_expired_row() {
    let det = Det::new();
    let s = store(&det).await;
    attach(&s).await;
    let now = s.now_ms();
    s.shim_binding_insert(row("d1", "s1:g1", "g1", now))
        .await
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    crate::coordinator::fence::Fence::new(run.clone())
        .ensure()
        .unwrap();
    let bindings = Bindings::default();
    crate::manager::binding_start::prepare(&s, &bindings, run)
        .await
        .unwrap();
    assert_eq!(
        bindings.get_digest("d1").unwrap().state,
        BindingState::Revoked
    );
    let r = record(&s).await;
    assert_eq!(
        (r.state, r.ended_reason.as_deref()),
        (RecordState::Ended, Some("expired"))
    );
    assert_eq!(
        ended(&s).await,
        [("s1:g1".to_owned(), 2, "expired".to_owned())]
    );
}
