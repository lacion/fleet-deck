//! Why a grant is not in effect, and the refusal texts (G6, design
//! `grant-idle.md` §3.5).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::StationId;
use fleetdeck_proto::command::GrantKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_testkit::Det;

use super::*;
use crate::store::grants::{GrantClass, NewGrant};
use crate::store::stations::NewStation;

const IDLE_MS: i64 = 60_000;

/// A stored grant to edit in memory; it is bound, so it can idle.
async fn sample() -> Grant {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let st = pin(&s, "alpha").await;
    let mut g = NewGrant::new(st, "coord-1".into());
    g.classes = BTreeSet::from([GrantClass::Mail]);
    g.idle_ttl_ms = IDLE_MS;
    let mut grant = s.create_grant(g).await.unwrap();
    grant.last_used_at = Some(grant.created_at);
    grant
}

async fn pin(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([Scope::Admin]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

#[tokio::test]
async fn grant_end_orders_revoked_then_lifetime_then_idle() {
    let base = sample().await;
    let inside = base.created_at + 1_000;
    let past = base.expires_at + 1;

    let mut g = base.clone();
    g.state = GrantState::Revoked;
    assert_eq!(grant_end(&g, past), Some(GrantEnd::Revoked));

    let mut g = base.clone();
    g.state = GrantState::Expired;
    g.revoked_reason = Some("idle".to_owned());
    assert_eq!(
        grant_end(&g, inside),
        Some(GrantEnd::Idle {
            idle_ttl_ms: IDLE_MS
        })
    );
    assert_eq!(grant_end(&g, past), Some(GrantEnd::Lifetime));

    let mut g = base.clone();
    g.state = GrantState::Expired;
    g.revoked_reason = Some("expired".to_owned());
    assert_eq!(grant_end(&g, inside), Some(GrantEnd::Lifetime));

    assert_eq!(grant_end(&base, past), Some(GrantEnd::Lifetime));

    let idle_at = base.created_at + IDLE_MS + 1;
    assert_eq!(
        grant_end(&base, idle_at),
        Some(GrantEnd::Idle {
            idle_ttl_ms: IDLE_MS
        })
    );
    assert_eq!(grant_end(&base, base.created_at + IDLE_MS), None);

    let mut unbound = base.clone();
    unbound.kind = GrantKind::Coordinator;
    unbound.last_used_at = None;
    assert_eq!(grant_end(&unbound, inside + IDLE_MS * 10), None);
}

#[test]
fn end_text_says_why_and_what_the_owner_can_do() {
    assert_eq!(
        end_text(GrantEnd::Lifetime),
        "Your fleet grant expired (its lifetime ended). \
         The owner can give a new grant on the board."
    );
    assert_eq!(
        end_text(GrantEnd::Revoked),
        "Your fleet grant was revoked. The owner can give a new grant on the board."
    );
    let idle = |ms| end_text(GrantEnd::Idle { idle_ttl_ms: ms });
    let text = |d: &str| {
        format!("Your fleet grant expired (idle for {d}). The owner can renew it on the board.")
    };
    assert_eq!(idle(1_800_000), text("30 minutes"));
    assert_eq!(idle(60_000), text("1 minute"));
    assert_eq!(idle(90_000), text("90 seconds"));
    assert_eq!(idle(1_000), text("1 second"));
    assert_eq!(idle(1_500), text("2 seconds"));
}

#[tokio::test]
async fn station_grant_ends_keeps_the_newest_grant_of_each_session_of_the_station() {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let alpha = pin(&s, "alpha").await;
    let beta = pin(&s, "beta").await;
    let make = |station: &StationId, session: &str, idle: i64| {
        let mut g = NewGrant::new(station.clone(), session.into());
        g.classes = BTreeSet::from([GrantClass::Mail]);
        g.idle_ttl_ms = idle;
        g
    };

    // `coord-1`: an older revoked grant, then a newer grant that goes idle.
    let old = s
        .create_grant(make(&alpha, "coord-1", IDLE_MS))
        .await
        .unwrap();
    assert!(
        s.revoke_grant(old.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    det.advance(Duration::from_secs(1));
    let newer = s
        .create_grant(make(&alpha, "coord-1", 1_000))
        .await
        .unwrap();
    let binding = CoordinatorBinding {
        binding_id: "b-1".to_owned(),
        grant_id: newer.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-1".to_owned(),
    };
    assert!(s.bind_grant(binding).await.unwrap());
    // `coord-2` stays in effect; a grant of another station is not listed.
    s.create_grant(make(&alpha, "coord-2", IDLE_MS))
        .await
        .unwrap();
    s.create_grant(make(&beta, "coord-3", 1_000)).await.unwrap();
    det.advance(Duration::from_secs(2));
    s.sweep_grants().await.unwrap();

    let ends = s.grant_ends(alpha.clone()).await.unwrap();
    assert_eq!(ends.len(), 1, "{ends:?}");
    let facts = ends.get(&"coord-1".into()).expect("coord-1 ended");
    assert_eq!(facts.end, GrantEnd::Idle { idle_ttl_ms: 1_000 });
    assert_eq!(facts.expires_at, newer.expires_at);
    assert!(!ends.contains_key(&"coord-2".into()));
    assert!(!ends.contains_key(&"coord-3".into()));
}

#[test]
fn end_facts_classify_the_lifetime_at_the_refusal_time() {
    let at = 10_000;
    let idle = EndFacts {
        end: GrantEnd::Idle {
            idle_ttl_ms: IDLE_MS,
        },
        expires_at: at,
    };
    assert_eq!(idle.at(at), idle.end);
    assert_eq!(idle.at(at + 1), GrantEnd::Lifetime);
    let revoked = EndFacts {
        end: GrantEnd::Revoked,
        expires_at: at,
    };
    assert_eq!(revoked.at(at + 1), GrantEnd::Revoked);
    let lifetime = EndFacts {
        end: GrantEnd::Lifetime,
        expires_at: at,
    };
    assert_eq!(lifetime.at(at - 1), GrantEnd::Lifetime);
}
