//! Tests of the V25 foundation: the authentication generation, the binding,
//! the directive invalidation and the wake reservation (R13 §5E to §5G).

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::StationId;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_testkit::Det;
use rusqlite::params;

use super::displays::DisplayChange;
use super::displays::NewDisplay;
use super::displays_binding::BindError;
use super::displays_pairing::{Approval, Claim, ExpectedLaunch};
use super::stations::NewStation;
use super::wake_seen::{WAKE_SEEN_MAX, WakeReserve};
use super::{DeckStore, call};

async fn station(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([fleetdeck_proto::link::Scope::Read]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

struct Rig {
    s: DeckStore,
    det: Det,
    a: StationId,
    b: StationId,
}

async fn rig() -> Rig {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let a = station(&s, "alpha").await;
    let b = station(&s, "beta").await;
    Rig { s, det, a, b }
}

impl Rig {
    async fn display(&self, name: &str) -> String {
        self.s
            .create_display(NewDisplay {
                name: name.to_owned(),
                ceiling: vec![self.a.clone()],
            })
            .await
            .unwrap()
            .display_id
    }

    /// An approved pairing for an existing display; returns its id and secret.
    async fn approved(
        &self,
        display: &str,
        expected: Option<ExpectedLaunch>,
    ) -> (String, Secret<String>) {
        let (id, code, secret) = self
            .s
            .pairing_start("1.2.3.4".into(), String::new(), expected)
            .await
            .unwrap();
        let a = self
            .s
            .pairing_approve_existing(id.clone(), code, display.to_owned())
            .await
            .unwrap();
        assert!(matches!(a, Approval::Approved(_)), "{a:?}");
        (id, secret)
    }

    async fn sql(&self, sql: &'static str, args: Vec<String>) {
        call(&self.s.inner.db, move |conn| {
            conn.execute(sql, rusqlite::params_from_iter(args.iter()))?;
            Ok(())
        })
        .await
        .unwrap();
    }

    async fn directive_generation(&self, display: &str) -> i64 {
        let d = display.to_owned();
        call(&self.s.inner.db, move |conn| {
            Ok(conn.query_row(
                "SELECT directive_generation FROM displays WHERE display_id = ?1",
                [d],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
    }

    async fn put_directive(&self, display: &str) {
        self.sql(
            "INSERT INTO display_directives
             (display_id, generation, mode, sender_kind, sender_id, body, created_at)
             VALUES (?1, 1, 'mission', 'owner', '', '{\"mode\":\"mission\"}', 1)",
            vec![display.to_owned()],
        )
        .await;
    }

    async fn has_directive(&self, display: &str) -> bool {
        let d = display.to_owned();
        call(&self.s.inner.db, move |conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM display_directives WHERE display_id = ?1",
                [d],
                |r| r.get::<_, i64>(0),
            )? > 0)
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn a_claim_replaces_the_logins_and_the_second_of_two_approved_pairings_fails() {
    let r = rig().await;
    let d = r.display("Wall").await;
    let (old_cookie, _) = r.s.create_display_session(d.clone()).await.unwrap();
    let (link_display, link_token, _) =
        r.s.create_display_with_link(NewDisplay {
            name: "other".into(),
            ceiling: vec![r.a.clone()],
        })
        .await
        .map(|(rec, t, e)| (rec.display_id, t, e))
        .unwrap();
    // A link of the display that is replaced.
    r.sql(
        "INSERT INTO display_links (token_hash, display_id, created_at, expires_at, auth_generation)
         VALUES ('h1', ?1, 1, 9999999999999, 0)",
        vec![d.clone()],
    )
    .await;
    let (p1, s1) = r.approved(&d, None).await;
    let (p2, s2) = r.approved(&d, None).await;
    let Claim::Claimed(issued) = r.s.pairing_claim(p1, &s1).await.unwrap() else {
        panic!("the first claim wins");
    };
    assert_eq!(issued.display_id, d);
    // The competing approved pairing is gone.
    assert!(matches!(
        r.s.pairing_claim(p2, &s2).await.unwrap(),
        Claim::Unknown
    ));
    // The earlier session, the unused link and the new cookie.
    assert!(r.s.display_session(&old_cookie).await.unwrap().is_none());
    assert!(r.s.display_session(&issued.cookie).await.unwrap().is_some());
    let links: i64 = call(&r.s.inner.db, {
        let d = d.clone();
        move |conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM display_links WHERE display_id = ?1",
                [d],
                |r| r.get(0),
            )?)
        }
    })
    .await
    .unwrap();
    assert_eq!(links, 0);
    // A link of another display is not touched.
    assert!(
        r.s.redeem_display_link(&link_token)
            .await
            .unwrap()
            .is_some()
    );
    let _ = link_display;
}

#[tokio::test]
async fn a_delayed_redemption_cannot_issue_a_session_after_a_replacement() {
    let r = rig().await;
    let d = r.display("Wall").await;
    let (_, token, _) =
        r.s.create_display_with_link(NewDisplay {
            name: "w2".into(),
            ceiling: vec![r.a.clone()],
        })
        .await
        .map(|(rec, t, e)| (rec.display_id, t, e))
        .unwrap();
    // The check passed at generation 0: a replacement follows, then the issue.
    let (p, secret) = r.approved(&d, None).await;
    assert!(matches!(
        r.s.pairing_claim(p, &secret).await.unwrap(),
        Claim::Claimed(_)
    ));
    let issued = call(&r.s.inner.db, {
        let d = d.clone();
        move |conn| {
            let tx = conn.transaction()?;
            let knobs = crate::knobs::DeckKnobs::default();
            let late = super::displays_auth::issue_fenced(&tx, &d, 0, 5, &knobs, &|| knobs)?;
            let now = super::displays_auth::issue_fenced(&tx, &d, 1, 5, &knobs, &|| knobs)?;
            tx.commit()?;
            Ok((late.is_some(), now.is_some()))
        }
    })
    .await
    .unwrap();
    assert_eq!(issued, (false, true));
    // A link of a display that was not replaced still works once.
    assert!(r.s.redeem_display_link(&token).await.unwrap().is_some());
    assert!(r.s.redeem_display_link(&token).await.unwrap().is_none());
}

#[tokio::test]
async fn a_session_or_a_pairing_of_an_older_generation_is_dead() {
    let r = rig().await;
    let d = r.display("Wall").await;
    let (cookie, _) = r.s.create_display_session(d.clone()).await.unwrap();
    assert!(r.s.display_session(&cookie).await.unwrap().is_some());
    let (p, secret) = r.approved(&d, None).await;
    r.sql(
        "UPDATE displays SET auth_generation = auth_generation + 1 WHERE display_id = ?1",
        vec![d.clone()],
    )
    .await;
    assert!(r.s.display_session(&cookie).await.unwrap().is_none());
    assert!(matches!(
        r.s.pairing_claim(p, &secret).await.unwrap(),
        Claim::Stale
    ));
}

#[tokio::test]
async fn a_revoke_ends_every_login_moves_the_generation_and_clears_the_binding() {
    let r = rig().await;
    let d = r.display("Wall").await;
    let (cookie, _) = r.s.create_display_session(d.clone()).await.unwrap();
    let t =
        r.s.display_begin_open(d.clone(), r.a.clone(), "DP-1".into())
            .await
            .unwrap();
    r.s.display_commit_binding(d.clone(), t.op_revision, r.a.clone(), "DP-1".into())
        .await
        .unwrap();
    r.put_directive(&d).await;
    let (p, secret) = r.approved(&d, None).await;
    assert!(r.s.display_revoke(d.clone()).await.unwrap());
    assert!(r.s.display_session(&cookie).await.unwrap().is_none());
    assert!(!r.has_directive(&d).await);
    assert!(r.s.display_get(d.clone()).await.unwrap().is_none());
    assert!(matches!(
        r.s.pairing_claim(p, &secret).await.unwrap(),
        Claim::Unknown
    ));
    let gen_binding: (i64, Option<String>) = call(&r.s.inner.db, {
        let d = d.clone();
        move |conn| {
            Ok(conn.query_row(
                "SELECT auth_generation, binding_station FROM displays WHERE display_id = ?1",
                [d],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        }
    })
    .await
    .unwrap();
    assert_eq!(gen_binding, (1, None));
}

#[tokio::test]
async fn a_managed_launch_is_checked_by_its_expected_display_and_operation_revision() {
    let r = rig().await;
    let d1 = r.display("one").await;
    let d2 = r.display("two").await;
    let t =
        r.s.display_begin_open(d1.clone(), r.a.clone(), "DP-1".into())
            .await
            .unwrap();
    let expect = ExpectedLaunch {
        display_id: d1.clone(),
        operation_revision: t.op_revision,
    };
    // The approval refuses another display and a new display.
    let (id, code, secret) =
        r.s.pairing_start("ip".into(), "ua".into(), Some(expect.clone()))
            .await
            .unwrap();
    let pending = r.s.pairing_pending().await.unwrap();
    assert_eq!(pending[0].expected_display_id.as_deref(), Some(d1.as_str()));
    assert!(matches!(
        r.s.pairing_approve_existing(id.clone(), code.clone(), d2.clone())
            .await
            .unwrap(),
        Approval::DisplayMismatch
    ));
    assert!(matches!(
        r.s.pairing_approve_new(
            id.clone(),
            code.clone(),
            NewDisplay {
                name: "new".into(),
                ceiling: vec![r.a.clone()]
            }
        )
        .await
        .unwrap(),
        Approval::DisplayMismatch
    ));
    assert!(matches!(
        r.s.pairing_approve_existing(id.clone(), code.clone(), "nope".into())
            .await
            .unwrap(),
        Approval::DisplayMismatch
    ));
    assert!(matches!(
        r.s.pairing_approve_existing(id.clone(), code, d1.clone())
            .await
            .unwrap(),
        Approval::Approved(_)
    ));
    // A newer launch supersedes the claim.
    r.s.display_begin_open(d1.clone(), r.a.clone(), "DP-1".into())
        .await
        .unwrap();
    assert!(matches!(
        r.s.pairing_claim(id, &secret).await.unwrap(),
        Claim::Superseded
    ));
    // A matching launch claims.
    let (p, s) = {
        let t =
            r.s.display_begin_open(d1.clone(), r.a.clone(), "DP-1".into())
                .await
                .unwrap();
        r.approved(
            &d1,
            Some(ExpectedLaunch {
                display_id: d1.clone(),
                operation_revision: t.op_revision,
            }),
        )
        .await
    };
    assert!(matches!(
        r.s.pairing_claim(p, &s).await.unwrap(),
        Claim::Claimed(_)
    ));
    // An unknown display cannot be approved.
    let (id, code, _) =
        r.s.pairing_start("ip".into(), "ua".into(), None)
            .await
            .unwrap();
    assert!(matches!(
        r.s.pairing_approve_existing(id, code, "nope".into())
            .await
            .unwrap(),
        Approval::DisplayUnknown
    ));
}

#[tokio::test]
async fn the_binding_is_checked_committed_cleared_and_edited_in_one_transaction() {
    let r = rig().await;
    let d = r.display("Wall").await;
    let t1 =
        r.s.display_begin_open(d.clone(), r.a.clone(), "DP-1".into())
            .await
            .unwrap();
    // A stale revision does not commit.
    let t2 =
        r.s.display_begin_open(d.clone(), r.a.clone(), "DP-1".into())
            .await
            .unwrap();
    assert!(t2.op_revision > t1.op_revision);
    assert!(matches!(
        r.s.display_commit_binding(d.clone(), t1.op_revision, r.a.clone(), "DP-1".into())
            .await,
        Err(BindError::Superseded)
    ));
    r.put_directive(&d).await;
    r.s.display_commit_binding(d.clone(), t2.op_revision, r.a.clone(), "DP-1".into())
        .await
        .unwrap();
    let got = r.s.display_get(d.clone()).await.unwrap().unwrap();
    assert_eq!(
        got.binding,
        Some(DisplayBinding {
            station_id: r.a.clone(),
            monitor: "DP-1".into()
        })
    );
    // The binding change deleted the directive and moved the generation.
    assert!(!r.has_directive(&d).await);
    assert_eq!(r.directive_generation(&d).await, 1);
    // Another station is refused; so are bad inputs.
    assert!(matches!(
        r.s.display_begin_open(d.clone(), r.b.clone(), "DP-1".into())
            .await,
        Err(BindError::Bound)
    ));
    for bad in ["", "a b", "x;y", &"m".repeat(65)] {
        assert!(matches!(
            r.s.display_begin_open(d.clone(), r.a.clone(), bad.to_owned())
                .await,
            Err(BindError::Invalid(_))
        ));
    }
    assert!(matches!(
        r.s.display_begin_open(d.clone(), StationId::new("nope"), "DP-1".into())
            .await,
        Err(BindError::Invalid(_))
    ));
    assert!(matches!(
        r.s.display_begin_open("nope".into(), r.a.clone(), "DP-1".into())
            .await,
        Err(BindError::Unknown)
    ));
    // The owner moves it explicitly.
    r.put_directive(&d).await;
    let moved =
        r.s.display_set_binding(
            d.clone(),
            Some(DisplayBinding {
                station_id: r.b.clone(),
                monitor: "HDMI-A-1".into(),
            }),
        )
        .await
        .unwrap();
    assert_eq!(moved.binding.unwrap().station_id, r.b);
    assert!(!r.has_directive(&d).await);
    // A close clears it.
    assert!(r.s.display_clear_binding(d.clone()).await.unwrap());
    assert!(
        r.s.display_get(d.clone())
            .await
            .unwrap()
            .unwrap()
            .binding
            .is_none()
    );
    assert!(!r.s.display_clear_binding("nope".into()).await.unwrap());
}

#[tokio::test]
async fn a_ceiling_change_deletes_the_directive_and_another_change_keeps_it() {
    let r = rig().await;
    let d = r.display("Wall").await;
    r.put_directive(&d).await;
    r.s.display_update(
        d.clone(),
        DisplayChange {
            name: Some("Renamed".into()),
            ..DisplayChange::default()
        },
    )
    .await
    .unwrap();
    assert!(r.has_directive(&d).await);
    r.s.display_update(
        d.clone(),
        DisplayChange {
            ceiling: Some(vec![r.a.clone(), r.b.clone()]),
            ..DisplayChange::default()
        },
    )
    .await
    .unwrap();
    assert!(!r.has_directive(&d).await);
    assert_eq!(r.directive_generation(&d).await, 1);
    // A directive view reads `mission` with the generation.
    let view = r.s.display_directive(d.clone()).await.unwrap().unwrap();
    assert_eq!(view.generation, 1);
    assert_eq!(
        view.directive,
        fleetdeck_proto::display_directive::DirectiveBody::mission()
    );
    let scope = r.s.display_scope(d).await.unwrap().unwrap();
    assert_eq!(scope.stations.len(), 2);
}

#[tokio::test]
async fn a_wake_request_reserves_once_and_across_two_concurrent_observations() {
    let r = rig().await;
    assert_eq!(
        r.s.wake_reserve("s", "r1", 1_000).await,
        WakeReserve::Reserved
    );
    assert_eq!(r.s.wake_reserve("s", "r1", 2_000).await, WakeReserve::Seen);
    assert_eq!(
        r.s.wake_reserve("s", "r2", 2_000).await,
        WakeReserve::Reserved
    );
    let (a, b) = tokio::join!(
        r.s.wake_reserve("s", "r3", 3_000),
        r.s.wake_reserve("s", "r3", 3_000)
    );
    let reserved = [a, b]
        .iter()
        .filter(|x| **x == WakeReserve::Reserved)
        .count();
    assert_eq!(reserved, 1, "{a:?} {b:?}");
    // A key of another station is another key.
    assert_eq!(
        r.s.wake_reserve("t", "r1", 4_000).await,
        WakeReserve::Reserved
    );
    // Rows older than 24 hours are pruned.
    let day = 24 * 60 * 60 * 1000;
    assert_eq!(
        r.s.wake_reserve("s", "r1", 1_000 + day + 1).await,
        WakeReserve::Reserved
    );
}

#[tokio::test]
async fn a_wake_key_survives_a_restart_so_a_restored_request_does_not_wake_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let det = Det::new();
    {
        let s = DeckStore::open(&path, det.clock(), det.ids())
            .await
            .unwrap();
        assert_eq!(
            s.wake_reserve("s", "r1", 1_000).await,
            WakeReserve::Reserved
        );
        // The process stops after the commit and before the dispatch.
    }
    let s = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    assert_eq!(s.wake_reserve("s", "r1", 2_000).await, WakeReserve::Seen);
}

#[tokio::test]
async fn a_saturated_wake_table_suppresses_and_recovers_after_120_seconds() {
    let r = rig().await;
    // 10 000 fresh rows (younger than 120 s at `now`).
    call(&r.s.inner.db, |conn| {
        let tx = conn.transaction()?;
        for i in 0..WAKE_SEEN_MAX {
            tx.execute(
                "INSERT INTO wake_seen (station_id, request_id, first_seen_ms) VALUES ('s', ?1, ?2)",
                params![format!("b{i}"), 1_000_000 + i],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    let now = 1_010_000;
    assert_eq!(
        r.s.wake_reserve("s", "new", now).await,
        WakeReserve::Suppressed
    );
    // The suppression is persisted: also an unknown key is suppressed.
    assert_eq!(
        r.s.wake_reserve("s", "other", now + 60_000).await,
        WakeReserve::Suppressed
    );
    // After 120 s the state ends and the old rows can be evicted.
    assert_eq!(
        r.s.wake_reserve("s", "later", now + 120_000).await,
        WakeReserve::Reserved
    );
    let rows: i64 = call(&r.s.inner.db, |conn| {
        Ok(conn.query_row("SELECT count(*) FROM wake_seen", [], |r| r.get(0))?)
    })
    .await
    .unwrap();
    assert_eq!(rows, WAKE_SEEN_MAX);
}

#[tokio::test]
async fn a_snapshot_leaves_a_mark_for_the_wake_rule() {
    let r = rig().await;
    assert!(r.s.snapshot_mark(&r.a).is_none());
    r.s.apply_snapshot(
        r.a.clone(),
        fleetdeck_proto::IncarnationId::from("inc-1".to_owned()),
        7,
        Vec::new(),
    )
    .await
    .unwrap();
    let mark = r.s.snapshot_mark(&r.a).unwrap();
    assert_eq!(mark.watermark, 7);
    assert_eq!(mark.incarnation.as_str(), "inc-1");
    r.det.advance(Duration::from_millis(5));
}

#[test]
fn an_existing_display_login_and_link_read_generation_zero_after_v25() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let migrations = super::schema::migrations();
    migrations.to_version(&mut conn, 24).unwrap();
    conn.execute_batch(
        "INSERT INTO displays (display_id, name, ceiling, created_at) VALUES ('d', 'Wall', '[]', 1);
         INSERT INTO display_sessions (session_hash, display_id, created_at, last_seen, expires_at)
             VALUES ('h', 'd', 1, 1, 9);
         INSERT INTO display_links (token_hash, display_id, created_at, expires_at)
             VALUES ('t', 'd', 1, 9);",
    )
    .unwrap();
    migrations.to_latest(&mut conn).unwrap();
    let gens: (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT d.auth_generation, d.binding_revision, d.directive_generation,
                    (SELECT auth_generation FROM display_sessions),
                    (SELECT auth_generation FROM display_links)
             FROM displays d",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(gens, (0, 0, 0, 0, 0));
    // The wake state row exists, and a directive mode is checked.
    let suspended: i64 = conn
        .query_row(
            "SELECT suspended_until_ms FROM wake_state WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(suspended, 0);
    assert!(
        conn.execute(
            "INSERT INTO display_directives (display_id, generation, mode, sender_kind, body, created_at)
             VALUES ('d', 1, 'bogus', 'owner', '{}', 1)",
            [],
        )
        .is_err()
    );
}
