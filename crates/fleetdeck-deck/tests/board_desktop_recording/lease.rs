//! The continuation lease of `board_desktop_recording` (R13 P5b-1 task K1):
//! `desktop.capture_lease` of the station with the fake station.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use crate::capture_support::*;
use crate::common::next_request;
use crate::desktop_support::*;
use crate::share_support::*;
use crate::support::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::recording::{CaptureLeaseRequest, LeaseDecision, LeasePhase, StopReason};
use serde_json::json;

fn failed() -> Outcome {
    err("recording_interrupted", SENTINEL)
}

#[tokio::test]
async fn the_first_record_lease_that_continues_sets_started_at_once_and_audits_it_once() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let (r, ()) = during(
        (&c, &h, &st),
        &mut station,
        ID,
        failed(),
        async |station, act| {
            let out = lease(station, act, LeasePhase::Start, Some("ok")).await;
            assert_eq!(answer_of(out), (LeaseDecision::Continue, None));
            assert_eq!(rec_row(&f, ID).await.unwrap().started_at, None);
            let out = lease(station, act, LeasePhase::Record, Some("ok")).await;
            assert_eq!(answer_of(out), (LeaseDecision::Continue, None));
            let first = rec_row(&f, ID).await.unwrap().started_at;
            assert!(first.is_some());
            f.det.advance(Duration::from_secs(3));
            for phase in [LeasePhase::Record, LeasePhase::Import] {
                let out = lease(station, act, phase, Some("ok")).await;
                assert_eq!(answer_of(out), (LeaseDecision::Continue, None));
            }
            assert_eq!(rec_row(&f, ID).await.unwrap().started_at, first);
        },
    )
    .await;
    expect(&r, 409, "recording_interrupted");
    let started: Vec<_> = c
        .audited("desktop.capture", AuditResult::Ok)
        .await
        .into_iter()
        .flatten()
        .filter(|d| d.contains("stage=started"))
        .collect();
    assert_eq!(
        started,
        vec![format!(
            "capture={ID} kind=recording monitor=DP-1 seconds=30 stage=started code=ok"
        )]
    );
    // The lease answers hold no station text, and no audit row per lease.
    assert!(!audit_text(&c).await.contains(SENTINEL));
    assert_eq!(c.audited("desktop.capture", AuditResult::Ok).await.len(), 2);
}

#[tokio::test]
async fn every_identity_failure_is_a_wire_forbidden_and_a_bad_request_is_a_protocol_error() {
    let dir = tempfile::tempdir().unwrap();
    let (f, mut station, c, h) = rec_setup_file(dir.path()).await;
    let st = f.station.0.clone();
    let conn = rusqlite::Connection::open(dir.path().join("deck.db")).unwrap();
    // A screenshot row of this station.
    f.store
        .capture_prepare(f.station.clone(), nth_id(2), "shot".into(), "DP-1".into())
        .await
        .unwrap();
    f.store
        .capture_attempted(f.station.clone(), nth_id(2))
        .await
        .unwrap();
    let (r, ()) = during(
        (&c, &h, &st),
        &mut station,
        ID,
        failed(),
        async |station, act| {
            let a = authority(act);
            let req = |id: &str, hash: &str, phase| CaptureLeaseRequest {
                capture_id: id.to_owned(),
                action_hash: hash.to_owned(),
                sequence: 7,
                phase,
            };
            for (what, request) in [
                (
                    "an unknown capture",
                    req(&nth_id(9), &a.action_hash, LeasePhase::Record),
                ),
                ("a screenshot", req(&nth_id(2), "shot", LeasePhase::Record)),
                ("another hash", req(ID, "other-hash", LeasePhase::Record)),
                ("a bad id", req("abc", &a.action_hash, LeasePhase::Record)),
            ] {
                let out = lease_req(station, &request, None).await;
                assert_eq!(wire_code(&out), "forbidden", "{what}");
            }
            // Another link epoch than the connection of the request.
            conn.execute(
                "UPDATE desktop_captures SET link_epoch = 'other' WHERE capture_id = ?1",
                [ID],
            )
            .unwrap();
            let out = lease(station, act, LeasePhase::Record, None).await;
            assert_eq!(wire_code(&out), "forbidden", "another epoch");
            let epoch = station
                .control
                .params()
                .local_desktop_epoch
                .clone()
                .unwrap();
            conn.execute(
                "UPDATE desktop_captures SET link_epoch = ?2 WHERE capture_id = ?1",
                [ID, epoch.as_str()],
            )
            .unwrap();
            let out = lease(station, act, LeasePhase::Record, Some("ok")).await;
            assert_eq!(answer_of(out).0, LeaseDecision::Continue);
            // `import` while the intent is `discard`.
            f.store
                .recording_stop_intent(
                    f.station.clone(),
                    ID.to_owned(),
                    false,
                    StopReason::Deadline,
                )
                .await
                .unwrap();
            let out = lease(station, act, LeasePhase::Import, None).await;
            assert_eq!(wire_code(&out), "forbidden", "import after a discard");
            // A request that does not parse.
            let bad = station
                .control
                .request("desktop.capture_lease", None, json!({ "capture_id": 1 }))
                .await
                .unwrap();
            assert_eq!(wire_code(&bad), "protocol_error");
        },
    )
    .await;
    expect(&r, 409, "recording_interrupted");
    // A row that is no more `attempted` allows no phase.
    assert_eq!(
        rec_row(&f, ID).await.unwrap().capture.state,
        CaptureState::Failed
    );
    let a = fleetdeck_proto::desktop::DesktopAction::ScreenRecord {
        monitor: "DP-1".into(),
        seconds: 30,
    };
    let hash = fleetdeck_link::desktop_authority::action_hash(
        &a,
        &f.station_node.identity.fingerprint().to_string(),
    );
    for phase in [LeasePhase::Start, LeasePhase::Record, LeasePhase::Import] {
        let request = CaptureLeaseRequest {
            capture_id: ID.into(),
            action_hash: hash.clone(),
            sequence: 7,
            phase,
        };
        assert_eq!(
            wire_code(&lease_req(&mut station, &request, None).await),
            "forbidden"
        );
    }
}

#[tokio::test]
async fn each_deck_local_loss_is_a_discard_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let (f, mut station, c, h) = rec_setup_file(dir.path()).await;
    let st = f.station.0.clone();
    let conn = rusqlite::Connection::open(dir.path().join("deck.db")).unwrap();
    let cases = [
        StopReason::DeckPolicy,
        StopReason::ArtifactDeleted,
        StopReason::Deadline,
        StopReason::CoordinatorAuthorityLost,
        StopReason::OwnerLoginEnded,
    ];
    for (n, reason) in cases.into_iter().enumerate() {
        let id = nth_id(u32::try_from(n).unwrap() + 10);
        let (r, ()) = during((&c, &h, &st), &mut station, &id, failed(), async |station, act| {
            match reason {
                StopReason::DeckPolicy => enable(&c, false),
                StopReason::ArtifactDeleted => {
                    let published = serde_json::to_string(&rec_published("V9", &id, None)).unwrap();
                    conn.execute("UPDATE desktop_captures SET published = ?2 WHERE capture_id = ?1", [id.as_str(), published.as_str()]).unwrap();
                    conn.execute(
                        "INSERT INTO artifact_tombstones
                             (station_id, artifact_id, state, reason, attempts, next_at, created_at, updated_at)
                         VALUES (?1, 'V9', 'pending', 'retry', 0, NULL, 1, 1)",
                        [st.as_str()],
                    )
                    .unwrap();
                }
                StopReason::Deadline => f.det.advance(Duration::from_secs(66)),
                StopReason::CoordinatorAuthorityLost => {
                    // A coordinator row takes the coordinator branch, which
                    // also needs fleet control on (R13 P5b-2): its context
                    // here does not parse, so the grant is lost.
                    f.store.set_fleet_control(true);
                    conn.execute("UPDATE desktop_captures SET coordinator = '{}' WHERE capture_id = ?1", [id.as_str()]).unwrap();
                }
                _ => {
                    let login = rec_row(&f, &id).await.unwrap().owner_login.unwrap();
                    assert!(f.store.end_login(login).await.unwrap());
                }
            }
            // The deck-local check comes first: the target is not asked.
            for phase in [LeasePhase::Start, LeasePhase::Record, LeasePhase::Import] {
                let out = lease(station, act, phase, None).await;
                assert_eq!(answer_of(out), (LeaseDecision::Discard, Some(reason)), "{reason:?}");
            }
            // Nothing is latched by a deck-local answer, and nothing started.
            assert_eq!(rec_row(&f, &id).await.unwrap().started_at, None);
            enable(&c, true);
        })
        .await;
        expect(&r, 409, "recording_interrupted");
    }
}

#[tokio::test]
async fn the_target_policy_must_allow_everything_and_the_epoch_must_match() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let (r, ()) = during(
        (&c, &h, &st),
        &mut station,
        ID,
        failed(),
        async |station, act| {
            for how in [
                "valid",
                "allow_desktop",
                "allow_desktop_capture",
                "monitor_allowed",
                "monitor_present",
                "unlocked",
                "desktop_epoch",
                "error",
                "junk",
            ] {
                let out = lease(station, act, LeasePhase::Record, Some(how)).await;
                assert_eq!(wire_code(&out), "forbidden", "{how}");
            }
            assert_eq!(rec_row(&f, ID).await.unwrap().started_at, None);
            // A policy answer that never comes is `forbidden` after 3 s at most.
            let out = {
                let control = station.control.clone();
                let a = authority(act);
                let req = CaptureLeaseRequest {
                    capture_id: a.action_id,
                    action_hash: a.action_hash,
                    sequence: 7,
                    phase: LeasePhase::Record,
                };
                let ask = async {
                    control
                        .request("desktop.capture_lease", None, json!(req))
                        .await
                        .unwrap()
                };
                let hold = async { next_request(station).await };
                let (out, held) = tokio::join!(ask, hold);
                assert_eq!(held.method, "desktop.capture_policy_now");
                out
            };
            assert_eq!(wire_code(&out), "forbidden");
        },
    )
    .await;
    expect(&r, 409, "recording_interrupted");
}

#[tokio::test]
async fn nine_parallel_lease_requests_get_one_busy_and_eight_answers() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let (r, ()) = during(
        (&c, &h, &st),
        &mut station,
        ID,
        failed(),
        async |station, act| {
            let a = authority(act);
            let control = station.control.clone();
            let requests = (0..9).map(|n| {
                let control = control.clone();
                let req = CaptureLeaseRequest {
                    capture_id: a.action_id.clone(),
                    action_hash: a.action_hash.clone(),
                    sequence: 100 + n,
                    phase: LeasePhase::Record,
                };
                async move {
                    control
                        .request("desktop.capture_lease", None, json!(req))
                        .await
                        .unwrap()
                }
            });
            // The eight that got a slot ask the target; the ninth is refused at once.
            let drive = async {
                let mut asked = Vec::new();
                for _ in 0..8 {
                    let got = next_request(station).await;
                    assert_eq!(got.method, "desktop.capture_policy_now");
                    asked.push(got);
                }
                for got in asked {
                    let reply = policy_reply(station, "ok");
                    station.control.respond(got.id, reply).await.unwrap();
                }
            };
            let (answers, ()) = tokio::join!(futures_util::future::join_all(requests), drive);
            let busy = answers
                .iter()
                .filter(|o| matches!(o, Outcome::Err(e) if e.code == "busy"))
                .count();
            let ok = answers
                .iter()
                .filter(|o| matches!(o, Outcome::Ok(_)))
                .count();
            assert_eq!((busy, ok), (1, 8), "{answers:?}");
        },
    )
    .await;
    expect(&r, 409, "recording_interrupted");
}
