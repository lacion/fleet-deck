//! The admission of `record_monitor` (R13 P5b-2 §13.2, task Q-K): the four
//! charges of one acceptance, the two caps with their stored refusals, the
//! shared screenshot rate, and no refund.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;

use super::admit_tests::{grant, new_op, rejected, rows, store, t0};
use super::*;
use crate::store::call;
use crate::store::grants::GrantClass;

/// A grant of both classes that allows `cap` recordings of `seconds` seconds
/// in all.
async fn recording_grant(
    s: &crate::store::DeckStore,
    cap: u32,
    seconds: u32,
) -> (String, CoordinatorContext) {
    grant(
        s,
        "coord",
        &[GrantClass::Desktop, GrantClass::Capture],
        |g| {
            g.recording_cap = cap;
            g.recording_second_cap = seconds;
        },
    )
    .await
}

fn record_op(grant_id: &str, ctx: &CoordinatorContext, name: &str, seconds: u32) -> NewOp {
    let mut op = new_op(grant_id, ctx, name, OpTool::RecordMonitor);
    op.class = OpClass::Capture;
    op.seconds = Some(seconds);
    op
}

async fn counters(s: &crate::store::DeckStore, gid: &str) -> (u32, u32, u32, u32, u32) {
    let g = s.grant(gid.to_owned()).await.unwrap().unwrap();
    (
        g.desktop_used,
        g.capture_used,
        g.recording_used,
        g.recording_second_used,
        g.desktop_attempts,
    )
}

async fn charged_text(s: &crate::store::DeckStore, id: &str) -> String {
    let id = id.to_owned();
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(
            "SELECT charged FROM coordinator_desktop_ops WHERE operation_id = ?1",
            [id],
            |r| r.get::<_, String>(0),
        )?)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn an_accepted_recording_charges_the_four_counters_and_stores_them() {
    let (det, s) = store().await;
    let (gid, ctx) = recording_grant(&s, 5, 600).await;
    let Admission::Accepted(row) = s
        .desktop_admit(record_op(&gid, &ctx, "r1", 45), t0(&det))
        .await
        .unwrap()
    else {
        panic!("expected Accepted");
    };
    assert_eq!(row.class, OpClass::Capture);
    assert_eq!(counters(&s, &gid).await, (1, 1, 1, 45, 1));
    let stored: serde_json::Value =
        serde_json::from_str(&charged_text(&s, &row.operation_id).await).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({"desktop": 1, "capture": 1, "recording": 1, "recording_seconds": 45})
    );
    // The row reads back through the two-field `Charges`.
    let back = s.desktop_op(&row.operation_id).await.unwrap().unwrap();
    assert_eq!(
        back.charged,
        Charges {
            desktop: 1,
            capture: 1
        }
    );
    // Another tool keeps the JSON that it always had.
    let Admission::Accepted(other) = s
        .desktop_admit(new_op(&gid, &ctx, "n1", OpTool::Notify), t0(&det))
        .await
        .unwrap()
    else {
        panic!("expected Accepted");
    };
    assert_eq!(
        charged_text(&s, &other.operation_id).await,
        r#"{"desktop":1,"capture":0}"#
    );
}

#[tokio::test]
async fn the_recording_count_cap_refuses_with_a_stored_code_that_replays() {
    let (det, s) = store().await;
    let (gid, ctx) = recording_grant(&s, 1, 600).await;
    let now = t0(&det);
    let Admission::Accepted(_) = s
        .desktop_admit(record_op(&gid, &ctx, "r1", 10), now)
        .await
        .unwrap()
    else {
        panic!("expected Accepted");
    };
    // The screenshot rate is the shared one: move the window on.
    let later = now + 61_000;
    let op = record_op(&gid, &ctx, "r2", 10);
    assert_eq!(
        rejected(s.desktop_admit(op.clone(), later).await.unwrap()),
        (code::RECORDING_CAP_REACHED, true)
    );
    // Stored: a replay answers the same code and counts nothing more.
    assert_eq!(
        rejected(s.desktop_admit(op, later + 1).await.unwrap()),
        (code::RECORDING_CAP_REACHED, true)
    );
    assert_eq!(counters(&s, &gid).await, (1, 1, 1, 10, 2));
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 1);
}

#[tokio::test]
async fn the_recording_seconds_cap_refuses_and_the_seconds_that_are_left_fit() {
    let (det, s) = store().await;
    let (gid, ctx) = recording_grant(&s, 10, 50).await;
    let now = t0(&det);
    s.desktop_admit(record_op(&gid, &ctx, "r1", 30), now)
        .await
        .unwrap();
    let later = now + 61_000;
    assert_eq!(
        rejected(
            s.desktop_admit(record_op(&gid, &ctx, "r2", 21), later)
                .await
                .unwrap()
        ),
        (code::RECORDING_SECONDS_REACHED, true)
    );
    // 30 + 20 = 50 is not above the cap.
    let Admission::Accepted(_) = s
        .desktop_admit(record_op(&gid, &ctx, "r3", 20), later)
        .await
        .unwrap()
    else {
        panic!("expected Accepted");
    };
    assert_eq!(counters(&s, &gid).await, (2, 2, 2, 50, 3));
}

#[tokio::test]
async fn a_grant_without_the_allowance_reaches_the_recording_cap_at_once() {
    let (det, s) = store().await;
    let (gid, ctx) = recording_grant(&s, 0, 0).await;
    assert_eq!(
        rejected(
            s.desktop_admit(record_op(&gid, &ctx, "r1", 5), t0(&det))
                .await
                .unwrap()
        ),
        (code::RECORDING_CAP_REACHED, true)
    );
    assert_eq!(counters(&s, &gid).await, (0, 0, 0, 0, 1));
}

#[tokio::test]
async fn the_capture_cap_and_the_screenshot_rate_are_shared_with_screenshots() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(
        &s,
        "coord",
        &[GrantClass::Desktop, GrantClass::Capture],
        |g| {
            g.capture_cap = Some(2);
            g.recording_cap = 10;
            g.recording_second_cap = 600;
        },
    )
    .await;
    let now = t0(&det);
    // A screenshot and a recording fill the capture cap and the rate.
    s.desktop_admit(new_op(&gid, &ctx, "s1", OpTool::Screenshot), now)
        .await
        .unwrap();
    s.desktop_admit(record_op(&gid, &ctx, "r1", 5), now)
        .await
        .unwrap();
    // The rate comes first: a recording and a screenshot are both limited.
    assert_eq!(
        rejected(
            s.desktop_admit(record_op(&gid, &ctx, "r2", 5), now)
                .await
                .unwrap()
        ),
        (code::RATE_LIMITED, false)
    );
    assert_eq!(
        rejected(
            s.desktop_admit(new_op(&gid, &ctx, "s2", OpTool::Screenshot), now)
                .await
                .unwrap()
        ),
        (code::RATE_LIMITED, false)
    );
    // Later the capture cap (2) is the limit, for a recording too.
    let later = now + 61_000;
    assert_eq!(
        rejected(
            s.desktop_admit(record_op(&gid, &ctx, "r3", 5), later)
                .await
                .unwrap()
        ),
        (code::CAPTURE_CAP_REACHED, true)
    );
    assert_eq!(counters(&s, &gid).await, (2, 2, 1, 5, 3));
}

#[tokio::test]
async fn a_recording_without_seconds_is_a_store_error_and_charges_nothing() {
    let (det, s) = store().await;
    let (gid, ctx) = recording_grant(&s, 5, 600).await;
    let mut op = record_op(&gid, &ctx, "r1", 5);
    op.seconds = None;
    assert!(s.desktop_admit(op, t0(&det)).await.is_err());
    assert_eq!(counters(&s, &gid).await, (0, 0, 0, 0, 0));
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 0);
}
