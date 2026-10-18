//! The owner screenshot result lookup, the recovery task and the features
//! (R13 P5a task E): `GET .../desktop/captures/{id}`, the reconcile of an
//! `attempted` capture, the 24 hour end of an unknown one, the deck crash
//! between `attempted` and the answer, an indexing failure after the station
//! commit, a tombstone, `GET /api/features` and `DesktopInfo.features`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod common;
mod desktop_support;
mod share_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use capture_support::*;
use common::next_request;
use desktop_support::*;
use fleetdeck_deck::desktop_capture::{self, CaptureOptions, Reply};
use fleetdeck_deck::link::desktop_authority::Waits;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::version::feature;
use serde_json::json;
use share_support::*;
use tokio::sync::watch;

/// An `attempted` row, as a deck leaves it after a crash between the record
/// and the answer: no send record, no answer.
async fn crashed(f: &common::Fixture, id: &str) {
    let hash = "crash-hash".to_owned();
    f.store
        .capture_prepare(f.station.clone(), id.to_owned(), hash, "DP-1".to_owned())
        .await
        .unwrap();
    f.store
        .capture_attempted(f.station.clone(), id.to_owned())
        .await
        .unwrap();
}

fn short_opts() -> CaptureOptions {
    CaptureOptions {
        waits: Waits {
            long: Duration::from_millis(200),
            short: Duration::from_millis(200),
        },
        ..CaptureOptions::default()
    }
}

#[tokio::test]
async fn the_lookup_reconciles_a_timed_out_capture_to_committed() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let ctx = relay_ctx(&f, &c);
    let ask = desktop_capture::act(
        &ctx,
        f.station.clone(),
        ID.into(),
        "DP-1".into(),
        short_opts(),
    );
    let seen = async { next_request(&mut station).await };
    let (result, _) = tokio::join!(ask, seen);
    assert!(result.is_err());
    assert_eq!(row(&f, ID).await.unwrap().state, CaptureState::Attempted);
    // The station still has no result: pending.
    let path = lookup_path(&st, ID);
    let look = get(&c, &h, &path);
    let answer = async {
        let req = next_request(&mut station).await;
        assert_eq!(req.method, "desktop.capture_status");
        assert_eq!(req.params, json!({ "capture_id": ID }));
        station
            .control
            .respond(req.id, Outcome::Ok(state("pending", ID)))
            .await
            .unwrap();
    };
    let (r, ()) = tokio::join!(look, answer);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, state("pending", ID));
    assert_eq!(row(&f, ID).await.unwrap().state, CaptureState::Attempted);
    // The station committed it: the lookup indexes the artifact.
    let path = lookup_path(&st, ID);
    let look = get(&c, &h, &path);
    let answer = async {
        let req = next_request(&mut station).await;
        station
            .control
            .respond(req.id, Outcome::Ok(committed(ID, "A1")))
            .await
            .unwrap();
    };
    let (r, ()) = tokio::join!(look, answer);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "committed");
    assert_eq!(r.body["capture_id"], ID);
    assert_eq!(r.body["published"]["artifact"]["station_id"], st);
    assert_eq!(r.body["published"]["version"]["capture"]["monitor"], "DP-1");
    let stored = row(&f, ID).await.unwrap();
    assert_eq!(
        (stored.state, stored.indexed),
        (CaptureState::Committed, true)
    );
    assert_eq!(f.store.artifacts(None, None, None).await.unwrap().len(), 1);
    // A resolved row is answered from the store: no station call.
    let r = get(&c, &h, &lookup_path(&st, ID)).await;
    assert_eq!(r.body["state"], "committed");
    quiet(&mut station).await;
}

#[tokio::test]
async fn the_lookup_answers_from_the_row_without_a_link_and_checks_its_input() {
    let (f, station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    crashed(&f, ID).await;
    f.drop_link(station).await;
    // No live link: the row says pending.
    let r = get(&c, &h, &lookup_path(&st, ID)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, state("pending", ID));
    expect(
        &get(&c, &h, &lookup_path(&st, "01ARZ3NDEKTSV4RRFFQ69G5FAW")).await,
        404,
        "unknown_capture",
    );
    expect(
        &get(&c, &h, &lookup_path(&st, "not-an-id")).await,
        400,
        "invalid_capture_id",
    );
    expect(
        &get(&c, &h, &lookup_path("no-such-station", ID)).await,
        404,
        "unknown_station",
    );
    // A token with Admin is not the owner.
    let (_, token) = f
        .store
        .create_token(
            "reader".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c
        .get(&lookup_path(&st, ID), &[("Authorization", auth.as_str())])
        .await;
    expect(&r, 403, "owner_only");
    // The lookup does not need `desktop_control`.
    enable(&c, false);
    assert_eq!(get(&c, &h, &lookup_path(&st, ID)).await.status, 200);
    // The lookup is not an action: no audit row.
    assert!(
        c.audited("desktop.screen_capture", AuditResult::Ok)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn the_recovery_indexes_a_crashed_capture_without_a_lookup_and_never_resends() {
    let (f, station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    crashed(&f, ID).await;
    let status: Status = Arc::new(|id| committed(id, "A1"));
    let log = serve_status(station, status);
    let (stop, rx) = watch::channel(false);
    let task = desktop_capture::spawn_with(
        f.store.clone(),
        Arc::clone(&f.ctx),
        rx,
        Duration::from_millis(100),
        Duration::from_secs(1),
    );
    f.wait("the capture indexed", |f| {
        let store = f.store.clone();
        let station = f.station.clone();
        async move {
            store
                .capture_get(station, ID.to_owned())
                .await
                .unwrap()
                .is_some_and(|r| r.indexed)
        }
    })
    .await;
    assert!(log.count("desktop.capture_status") >= 1);
    // A repeated POST of the id answers from the row; the station is asked
    // for `desktop.info` only. It never gets another `desktop.act`.
    let dummy = capture_with(&c, &h, &st, Some(ID), cap_body("DP-1")).await;
    // The hash of the crashed row is not the hash of this body.
    expect(&dummy, 409, "capture_id_conflict");
    assert_eq!(log.count("desktop.act"), 0);
    let r = get(&c, &h, &lookup_path(&st, ID)).await;
    assert_eq!(r.body["state"], "committed");
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn a_post_of_an_attempted_capture_of_this_deck_answers_pending_and_the_recovery_commits_it() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let ctx = relay_ctx(&f, &c);
    let ask = desktop_capture::act(
        &ctx,
        f.station.clone(),
        ID.into(),
        "DP-1".into(),
        short_opts(),
    );
    let seen = async { next_request(&mut station).await };
    let (result, _) = tokio::join!(ask, seen);
    assert!(result.is_err());
    // The same request again: pending, nothing sent.
    let r = capture_with(&c, &h, &st, Some(ID), cap_body("DP-1")).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body, state("pending", ID));
    quiet(&mut station).await;
    let status: Status = Arc::new(|id| committed(id, "A1"));
    let log = serve_status(station, status);
    let (stop, rx) = watch::channel(false);
    let task = desktop_capture::spawn_with(
        f.store.clone(),
        Arc::clone(&f.ctx),
        rx,
        Duration::from_millis(100),
        Duration::from_secs(1),
    );
    f.wait("the capture indexed", |f| {
        let store = f.store.clone();
        let station = f.station.clone();
        async move {
            store
                .capture_get(station, ID.to_owned())
                .await
                .unwrap()
                .is_some_and(|r| r.indexed)
        }
    })
    .await;
    assert_eq!(log.count("desktop.act"), 0);
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn an_unknown_capture_turns_terminal_after_24_hours_and_polling_stops() {
    let (f, station, _c, _h) = setup_default().await;
    crashed(&f, ID).await;
    let status: Status = Arc::new(|id| state("unknown", id));
    let log = serve_status(station, status);
    let (stop, rx) = watch::channel(false);
    let task = desktop_capture::spawn_with(
        f.store.clone(),
        Arc::clone(&f.ctx),
        rx,
        Duration::from_millis(100),
        Duration::from_secs(1),
    );
    f.wait("the first status call", |_| {
        let log = Arc::clone(&log);
        async move { log.count("desktop.capture_status") >= 2 }
    })
    .await;
    assert_eq!(row(&f, ID).await.unwrap().state, CaptureState::Attempted);
    f.det.advance(Duration::from_secs(24 * 3600 + 1));
    f.wait("the row unknown", |f| {
        let store = f.store.clone();
        let station = f.station.clone();
        async move {
            store
                .capture_get(station, ID.to_owned())
                .await
                .unwrap()
                .is_some_and(|r| r.state == CaptureState::Unknown)
        }
    })
    .await;
    // Polling stops: the count does not grow any more.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = log.count("desktop.capture_status");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(log.count("desktop.capture_status"), after);
    assert_eq!(log.count("desktop.act"), 0);
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn an_unknown_row_answers_unknown_to_a_lookup_and_a_repeated_post() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let ctx = relay_ctx(&f, &c);
    let ask = desktop_capture::act(
        &ctx,
        f.station.clone(),
        ID.into(),
        "DP-1".into(),
        short_opts(),
    );
    let seen = async { next_request(&mut station).await };
    let (result, _) = tokio::join!(ask, seen);
    assert!(result.is_err());
    f.store
        .capture_unknown(f.station.clone(), ID.to_owned())
        .await
        .unwrap();
    let r = get(&c, &h, &lookup_path(&st, ID)).await;
    assert_eq!(r.body, state("unknown", ID));
    let r = capture_with(&c, &h, &st, Some(ID), cap_body("DP-1")).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body, state("unknown", ID));
    quiet(&mut station).await;
}

#[tokio::test]
async fn an_indexing_failure_after_the_station_commit_is_pending_and_the_recovery_indexes_it() {
    let dir = tempfile::tempdir().unwrap();
    let (f, mut station, c, h) = setup_file(dir.path()).await;
    let st = f.station.0.clone();
    // A second connection makes the version insert fail.
    let conn = rusqlite::Connection::open(dir.path().join("deck.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER boom BEFORE INSERT ON artifact_versions
         BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    )
    .unwrap();
    let (r, _) = capture_exchange(
        &c,
        &h,
        &mut station,
        &st,
        ID,
        cap_body("DP-1"),
        capture_ok(ID, "A1"),
    )
    .await;
    // The screenshot did not fail: 202 with the pending state.
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body, json!({ "capture_id": ID, "state": "pending" }));
    let stored = row(&f, ID).await.unwrap();
    assert_eq!(
        (stored.state, stored.indexed),
        (CaptureState::Committed, false)
    );
    assert!(stored.published.is_some());
    assert!(
        f.store
            .artifacts(None, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        c.audited("desktop.screen_capture", AuditResult::Ok).await,
        vec![Some(format!("capture={ID} code=pending"))]
    );
    // The lookup answers pending while the row is not indexed.
    let status: Status = Arc::new(|id| committed(id, "A1"));
    conn.execute_batch("DROP TRIGGER boom;").unwrap();
    let log = serve_status(station, status);
    let (stop, rx) = watch::channel(false);
    let task = desktop_capture::spawn_with(
        f.store.clone(),
        Arc::clone(&f.ctx),
        rx,
        Duration::from_millis(100),
        Duration::from_secs(1),
    );
    f.wait("the capture indexed", |f| {
        let store = f.store.clone();
        let station = f.station.clone();
        async move {
            store
                .capture_get(station, ID.to_owned())
                .await
                .unwrap()
                .is_some_and(|r| r.indexed)
        }
    })
    .await;
    assert_eq!(f.store.artifacts(None, None, None).await.unwrap().len(), 1);
    assert_eq!(log.count("desktop.act"), 0);
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn a_tombstone_of_the_captured_artifact_wins_in_the_lookup_and_the_repeated_post() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let (r, _) = capture_exchange(
        &c,
        &h,
        &mut station,
        &st,
        ID,
        cap_body("DP-1"),
        capture_ok(ID, "A1"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    f.store
        .artifact_delete(f.station.clone(), "A1".to_owned())
        .await
        .unwrap();
    let r = get(&c, &h, &lookup_path(&st, ID)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, state("deleted", ID));
    let r = capture_with(&c, &h, &st, Some(ID), cap_body("DP-1")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, state("deleted", ID));
    quiet(&mut station).await;
    // A capture whose artifact was deleted before the deck indexed it: the
    // result is returned as the station sent it, and nothing is indexed.
    let id2 = nth_id(2);
    let (r, _) = capture_exchange(
        &c,
        &h,
        &mut station,
        &st,
        &id2,
        cap_body("DP-1"),
        capture_ok(&id2, "A1"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["capture"]["published"]["artifact"]["station_id"],
        "station-side-name"
    );
    assert!(
        f.store
            .artifacts(None, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    let stored = row(&f, &id2).await.unwrap();
    assert_eq!((stored.deleted, stored.indexed), (true, false));
}

#[tokio::test]
async fn the_features_route_and_the_info_features_follow_the_negotiation() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let r = get(&c, &h, "/api/features").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["features"], json!(feature::ALL));
    let features = r.body["features"].as_array().unwrap();
    assert!(features.iter().any(|v| v == feature::DESKTOP_CAPTURE));
    assert!(features.iter().any(|v| v == feature::ARTIFACTS_DELETE));
    // Owner only.
    let (_, token) = f
        .store
        .create_token(
            "reader".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    expect(
        &c.get("/api/features", &[("Authorization", auth.as_str())])
            .await,
        403,
        "owner_only",
    );
    // The info of the station: the station sends none, the deck stamps both.
    let path = info_path(&st);
    let ask = get(&c, &h, &path);
    let answer = async {
        let req = next_request(&mut station).await;
        station
            .control
            .respond(req.id, Outcome::Ok(serde_json::to_value(info()).unwrap()))
            .await
            .unwrap();
    };
    let (r, ()) = tokio::join!(ask, answer);
    assert_eq!(
        r.body["features"],
        json!([feature::DESKTOP_CAPTURE, feature::ARTIFACTS_DELETE])
    );
    // A station that leaves out `desktop_capture.v1`.
    let (f2, mut station2, c2, h2) = setup_without(&[feature::DESKTOP_CAPTURE]).await;
    let path2 = info_path(&f2.station.0);
    let ask = get(&c2, &h2, &path2);
    let answer = async {
        let req = next_request(&mut station2).await;
        station2
            .control
            .respond(req.id, Outcome::Ok(serde_json::to_value(info()).unwrap()))
            .await
            .unwrap();
    };
    let (r, ()) = tokio::join!(ask, answer);
    assert_eq!(r.body["features"], json!([feature::ARTIFACTS_DELETE]));
    // And one that leaves out both.
    let (f3, mut station3, c3, h3) =
        setup_without(&[feature::DESKTOP_CAPTURE, feature::ARTIFACTS_DELETE]).await;
    let path3 = info_path(&f3.station.0);
    let ask = get(&c3, &h3, &path3);
    let answer = async {
        let req = next_request(&mut station3).await;
        station3
            .control
            .respond(req.id, Outcome::Ok(serde_json::to_value(info()).unwrap()))
            .await
            .unwrap();
    };
    let (r, ()) = tokio::join!(ask, answer);
    assert_eq!(r.body["features"], json!([]));
    // The kept station info is stamped too.
    let kept = f2.store.station_info(&f2.station);
    assert!(kept.is_none_or(|(i, _)| {
        i.desktop
            .is_none_or(|d| d.features == [feature::ARTIFACTS_DELETE])
    }));
}

#[tokio::test]
async fn a_capture_is_never_a_mutation_outbox_entry() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let (r, _) = capture_exchange(
        &c,
        &h,
        &mut station,
        &st,
        ID,
        cap_body("DP-1"),
        capture_ok(ID, "A1"),
    )
    .await;
    assert_eq!(r.status, 200);
    let ctx = relay_ctx(&f, &c);
    let reply = desktop_capture::act(
        &ctx,
        f.station.clone(),
        ID.into(),
        "DP-1".into(),
        short_opts(),
    );
    let answer = async {
        let req = next_request(&mut station).await;
        assert_eq!(req.method, "desktop.info");
        station
            .control
            .respond(req.id, Outcome::Ok(serde_json::to_value(info()).unwrap()))
            .await
            .unwrap();
    };
    let (again, ()) = tokio::join!(reply, answer);
    assert!(matches!(again.unwrap(), Reply::Done(_)));
    let held = f.store.outbox().await.map(|i| i.len());
    assert_eq!(held.unwrap_or(0), 0);
}
