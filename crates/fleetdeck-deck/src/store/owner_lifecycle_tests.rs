//! MCP P4b deck test 11 at the store level: the rebind rule, the `>=`
//! renewal, the terminal events by binding id, the synthetic terminal entry
//! of a snapshot projection and the late `coordinator.bound`, for a grant with
//! a row; a grant without a row keeps today's rules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use fleetdeck_proto::command::DeckCommandState;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::{CoordinatorBindingPhase, CoordinatorBindingState, EventEnvelope};
use rusqlite::params;

use super::*;
use super::{World, give, grant, new_grant, row_of, sets, world};

const MIN: u64 = 60_000;

/// An event of `session` at the deck clock; the caller numbers the events.
fn event(w: &World, seq: u64, session: &str, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: w.s.now_ms(),
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn bound(session: &str, grant_id: &str, generation: u32, native: &str) -> Event {
    Event::CoordinatorBound {
        binding_id: format!("{session}:{grant_id}"),
        grant_id: grant_id.to_owned(),
        generation,
        provider_instance: "claude".to_owned(),
        native_id: native.to_owned(),
    }
}

fn ended(session: &str, grant_id: &str, generation: u32, reason: &str) -> Event {
    Event::CoordinatorEnded {
        binding_id: format!("{session}:{grant_id}"),
        generation,
        reason: reason.to_owned(),
    }
}

/// Folds the events of a station, numbered from `seq`.
async fn fold(w: &World, seq: &mut u64, session: &str, events: Vec<Event>) {
    let batch = events
        .into_iter()
        .map(|e| {
            *seq += 1;
            event(w, *seq, session, e)
        })
        .collect();
    w.s.apply_events(w.a.clone(), "inc".to_owned().into(), batch)
        .await
        .unwrap();
}

fn generation(g: &Grant) -> Option<u32> {
    g.binding.as_ref().map(|b| b.generation)
}

/// A snapshot whose coordinator projection holds one synthetic terminal entry
/// (no native id).
async fn project(
    w: &World,
    seq: u64,
    session: &str,
    grant_id: &str,
    state: CoordinatorBindingPhase,
) {
    let entry = CoordinatorBindingState {
        binding_id: format!("{session}:{grant_id}"),
        grant_id: grant_id.to_owned(),
        session_id: session.into(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: None,
        state,
    };
    w.s.apply_snapshot_with(
        w.a.clone(),
        "inc".to_owned().into(),
        seq,
        Vec::new(),
        Some(vec![entry]),
        None,
    )
    .await
    .unwrap();
}

/// MCP P4b test 11, the store part.
#[tokio::test]
async fn the_rebind_rule_accepts_a_higher_generation_only_for_a_grant_in_effect() {
    let w = world().await;
    let mut seq = 0;
    let g = give(&w, "owner-1", new_grant(&w, "owner-1")).await.unwrap();
    let id = g.grant_id.clone();
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![bound("owner-1", &id, 1, "n-1")],
    )
    .await;
    let first = grant(&w, &id).await;
    assert_eq!(generation(&first), Some(1));
    let t0 = first.last_used_at.unwrap();

    // A higher generation of the grant's station and session rebinds it with
    // no renewal. The idle clock and the counters stay.
    w.det.advance(Duration::from_millis(5 * MIN));
    let counters = id.clone();
    call(&w.s.inner.db, move |conn| {
        conn.execute(
            "UPDATE tokens SET spawn_used = 2 WHERE grant_id = ?1",
            [counters],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![bound("owner-1", &id, 2, "n-2")],
    )
    .await;
    let rebound = grant(&w, &id).await;
    assert_eq!(generation(&rebound), Some(2));
    assert_eq!(rebound.binding.as_ref().unwrap().native_id, "n-2");
    assert_eq!(rebound.last_used_at, Some(t0));
    assert_eq!((rebound.state, rebound.spawn_used), (GrantState::Active, 2));
    // The same or a lower generation changes nothing.
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![bound("owner-1", &id, 2, "n-9")],
    )
    .await;
    assert_eq!(grant(&w, &id).await.binding.unwrap().native_id, "n-2");
    // Another session never rebinds it.
    fold(
        &w,
        &mut seq,
        "owner-2",
        vec![bound("owner-1", &id, 3, "n-9")],
    )
    .await;
    assert_eq!(generation(&grant(&w, &id).await), Some(2));

    // A grant without a row keeps today's rule: a higher generation needs the
    // owner's matching renewal.
    let plain = w.s.create_grant(new_grant(&w, "owner-2")).await.unwrap();
    fold(
        &w,
        &mut seq,
        "owner-2",
        vec![bound("owner-2", &plain.grant_id, 1, "p-1")],
    )
    .await;
    fold(
        &w,
        &mut seq,
        "owner-2",
        vec![bound("owner-2", &plain.grant_id, 2, "p-2")],
    )
    .await;
    assert_eq!(generation(&grant(&w, &plain.grant_id).await), Some(1));

    // An idle-expired grant ignores a higher generation; the owner renews it,
    // and the renewal accepts a generation of at least the pending one.
    w.det.advance(Duration::from_millis(31 * MIN));
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![bound("owner-1", &id, 3, "n-3")],
    )
    .await;
    assert_eq!(generation(&grant(&w, &id).await), Some(2));
    w.s.sweep_grants().await.unwrap();
    let idle = grant(&w, &id).await;
    assert_eq!(idle.state, GrantState::Expired);
    assert_eq!(idle.revoked_reason.as_deref(), Some("idle"));
    assert_eq!(w.s.renew_grant(id.clone()).await.unwrap(), Some(3));
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![bound("owner-1", &id, 4, "n-4")],
    )
    .await;
    let renewed = grant(&w, &id).await;
    assert_eq!(
        (renewed.state, generation(&renewed)),
        (GrantState::Active, Some(4))
    );
    // The same rule for a grant without a row needs exactly the pending one.
    w.s.sweep_grants().await.unwrap();
    assert_eq!(grant(&w, &plain.grant_id).await.state, GrantState::Expired);
    assert_eq!(
        w.s.renew_grant(plain.grant_id.clone()).await.unwrap(),
        Some(2)
    );
    fold(
        &w,
        &mut seq,
        "owner-2",
        vec![bound("owner-2", &plain.grant_id, 3, "p-3")],
    )
    .await;
    assert_eq!(generation(&grant(&w, &plain.grant_id).await), Some(1));
}

/// `coordinator.ended` and the synthetic terminal entry of a projection end a
/// grant with a row by its binding id; the row stays.
#[tokio::test]
async fn a_terminal_event_ends_a_grant_with_a_row_by_its_binding_id() {
    let w = world().await;
    let mut seq = 0;
    let g = give(&w, "owner-1", new_grant(&w, "owner-1")).await.unwrap();
    let id = g.grant_id.clone();
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![bound("owner-1", &id, 1, "n-1")],
    )
    .await;
    call(&w.s.inner.db, {
        let id = id.clone();
        move |conn| {
            conn.execute(
                "INSERT INTO outbox (outbox_id, station_id, kind, origin, payload, state,
                     next_at, expires_at, created_at, updated_at, authority, required_classes)
                 SELECT 'pending', station_id, 'command', 'coordinator', '{}', 'queued', 1,
                     9999999999999, 1, 1, json_object('grant_id', grant_id), '[\"read\"]'
                 FROM tokens WHERE grant_id = ?1",
                params![id],
            )?;
            Ok(())
        }
    })
    .await
    .unwrap();
    // A generation below the bound one ends nothing.
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![bound("owner-1", &id, 3, "n-3")],
    )
    .await;
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![ended("owner-1", &id, 2, "identity_changed")],
    )
    .await;
    assert_eq!(grant(&w, &id).await.state, GrantState::Active);
    // A higher generation with a terminal reason revokes it, and its pending
    // fleet items fail.
    fold(
        &w,
        &mut seq,
        "owner-1",
        vec![ended("owner-1", &id, 4, "identity_changed")],
    )
    .await;
    let revoked = grant(&w, &id).await;
    assert_eq!(revoked.state, GrantState::Revoked);
    assert_eq!(revoked.revoked_reason.as_deref(), Some("identity_changed"));
    let reason: Option<String> = call(&w.s.inner.db, |conn| {
        Ok(conn.query_row(
            "SELECT reason FROM outbox WHERE outbox_id = 'pending'",
            [],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap();
    assert_eq!(reason.as_deref(), Some("authority_lost"));
    assert!(row_of(&w, "owner-1").await.is_some());

    // `expired` makes the grant `expired` with that reason; `replaced` and
    // `revoked` revoke it.
    for (session, reason, state) in [
        ("owner-2", "expired", GrantState::Expired),
        ("owner-3", "replaced", GrantState::Revoked),
        ("owner-4", "revoked", GrantState::Revoked),
    ] {
        let g = give(&w, session, new_grant(&w, session)).await.unwrap();
        fold(
            &w,
            &mut seq,
            session,
            vec![bound(session, &g.grant_id, 1, "n-1")],
        )
        .await;
        fold(
            &w,
            &mut seq,
            session,
            vec![ended(session, &g.grant_id, 1, reason)],
        )
        .await;
        let after = grant(&w, &g.grant_id).await;
        assert_eq!(
            (after.state, after.revoked_reason.as_deref()),
            (state, Some(reason))
        );
        assert!(row_of(&w, session).await.is_some());
    }

    // A grant without a row keeps the exact-generation rule.
    let plain = w.s.create_grant(new_grant(&w, "owner-5")).await.unwrap();
    fold(
        &w,
        &mut seq,
        "owner-5",
        vec![bound("owner-5", &plain.grant_id, 1, "p-1")],
    )
    .await;
    fold(
        &w,
        &mut seq,
        "owner-5",
        vec![ended("owner-5", &plain.grant_id, 2, "revoked")],
    )
    .await;
    assert_eq!(grant(&w, &plain.grant_id).await.state, GrantState::Active);
    fold(
        &w,
        &mut seq,
        "owner-5",
        vec![ended("owner-5", &plain.grant_id, 1, "revoked")],
    )
    .await;
    assert_eq!(grant(&w, &plain.grant_id).await.state, GrantState::Revoked);
}

/// A synthetic `Revoked` or `Expired` entry (no native id) of a snapshot
/// projection ends a grant with a row; without a row the entry keeps its
/// rule. A late `coordinator.bound` of a terminal grant with a row queues a
/// set with no spec.
#[tokio::test]
async fn a_projection_and_a_late_bound_event_end_a_grant_with_a_row() {
    let w = world().await;
    let mut seq = 0;
    let revoked = give(&w, "owner-3", new_grant(&w, "owner-3")).await.unwrap();
    let expired = give(&w, "owner-4", new_grant(&w, "owner-4")).await.unwrap();
    let plain = w.s.create_grant(new_grant(&w, "owner-5")).await.unwrap();
    for (session, g) in [
        ("owner-3", &revoked.grant_id),
        ("owner-4", &expired.grant_id),
        ("owner-5", &plain.grant_id),
    ] {
        fold(&w, &mut seq, session, vec![bound(session, g, 1, "n-1")]).await;
    }
    let mark = 100;
    project(
        &w,
        mark,
        "owner-3",
        &revoked.grant_id,
        CoordinatorBindingPhase::Revoked,
    )
    .await;
    let after = grant(&w, &revoked.grant_id).await;
    assert_eq!(after.state, GrantState::Revoked);
    assert_eq!(after.revoked_reason.as_deref(), Some("revoked"));
    project(
        &w,
        mark + 1,
        "owner-4",
        &expired.grant_id,
        CoordinatorBindingPhase::Expired,
    )
    .await;
    let after = grant(&w, &expired.grant_id).await;
    assert_eq!(after.state, GrantState::Expired);
    assert_eq!(after.revoked_reason.as_deref(), Some("expired"));
    project(
        &w,
        mark + 2,
        "owner-5",
        &plain.grant_id,
        CoordinatorBindingPhase::Revoked,
    )
    .await;
    let after = grant(&w, &plain.grant_id).await;
    assert_eq!(after.revoked_reason.as_deref(), Some("snapshot_revoked"));

    // The late `coordinator.bound` of the revoked grant with a row: a set with
    // no spec and the next sequence; the pending give is superseded.
    let before = row_of(&w, "owner-3").await.unwrap().seq;
    let mut seq = mark + 10;
    fold(
        &w,
        &mut seq,
        "owner-3",
        vec![bound("owner-3", &revoked.grant_id, 2, "n-2")],
    )
    .await;
    let row = row_of(&w, "owner-3").await.unwrap();
    assert_eq!(row.seq, before + 1);
    let queued = sets(&w, "owner-3").await;
    let last = queued.last().unwrap();
    assert_eq!((last.seq, last.state), (row.seq, DeckCommandState::Pending));
    assert!(last.spec.is_none());
    assert_eq!(last.grant_id, revoked.grant_id);
    assert_eq!(
        queued
            .iter()
            .filter(|c| c.state == DeckCommandState::Pending)
            .count(),
        1
    );
}
