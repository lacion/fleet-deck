//! The rolling rate windows of the coordinator desktop tools (R13 P6 §8.2)
//! as the admission transaction applies them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::code;

use super::admit_tests::{grant, new_op, rows, store, t0};
use super::*;
use crate::store::grants::GrantClass;

async fn run(
    s: &crate::store::DeckStore,
    gid: &str,
    ctx: &fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext,
    name: &str,
    tool: OpTool,
    now: i64,
) -> Admission {
    s.desktop_admit(new_op(gid, ctx, name, tool), now)
        .await
        .unwrap()
}

fn limited() -> Admission {
    Admission::Rejected {
        code: code::RATE_LIMITED,
        stored: false,
    }
}

#[tokio::test]
async fn the_thirty_first_mutation_in_a_window_is_rate_limited_and_not_charged() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(100);
    })
    .await;
    let now = t0(&det);
    for i in 0..30 {
        let got = run(&s, &gid, &ctx, &format!("r{i}"), OpTool::WallShow, now).await;
        assert!(matches!(got, Admission::Accepted(_)), "{i}");
    }
    assert_eq!(
        run(&s, &gid, &ctx, "r30", OpTool::WallShow, now).await,
        limited()
    );
    let g = s.grant(gid.clone()).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (30, 30));
    assert_eq!(rows(&s, "coordinator_desktop_rejects").await, 0);
    // The refusal is not stored, so the same request is accepted later, after
    // the window of the first rows has passed.
    let got = run(&s, &gid, &ctx, "r30", OpTool::WallShow, now + 60_000).await;
    assert!(matches!(got, Admission::Accepted(_)));
}

#[tokio::test]
async fn a_clock_moved_back_ten_minutes_opens_no_capacity() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(100);
    })
    .await;
    let now = t0(&det);
    for i in 0..30 {
        run(&s, &gid, &ctx, &format!("r{i}"), OpTool::WallShow, now).await;
    }
    let back = now - 600_000;
    assert_eq!(
        run(&s, &gid, &ctx, "r30", OpTool::WallShow, back).await,
        limited()
    );
    // The window time stays at the newest row: a new row would not be older.
    assert_eq!(
        run(&s, &gid, &ctx, "r31", OpTool::WallShow, back + 30_000).await,
        limited()
    );
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(g.desktop_used, 30);
}

#[tokio::test]
async fn six_notifies_of_one_caller_session_across_two_grants_then_the_seventh_is_refused() {
    let (det, s) = store().await;
    let (gid_a, ctx_a) = grant(&s, "coord", &[GrantClass::Desktop], |_| {}).await;
    let (gid_b, ctx_b) = grant(&s, "coord", &[GrantClass::Desktop], |_| {}).await;
    assert_ne!(gid_a, gid_b);
    let now = t0(&det);
    for i in 0..3 {
        let a = run(&s, &gid_a, &ctx_a, &format!("a{i}"), OpTool::Notify, now).await;
        let b = run(&s, &gid_b, &ctx_b, &format!("b{i}"), OpTool::Notify, now).await;
        assert!(matches!(a, Admission::Accepted(_)));
        assert!(matches!(b, Admission::Accepted(_)));
    }
    assert_eq!(
        run(&s, &gid_a, &ctx_a, "a3", OpTool::Notify, now).await,
        limited()
    );
    assert_eq!(
        run(&s, &gid_b, &ctx_b, "b3", OpTool::Notify, now).await,
        limited()
    );
    // Another tool of the same grants is not a notify.
    let other = run(&s, &gid_a, &ctx_a, "a4", OpTool::WallShow, now).await;
    assert!(matches!(other, Admission::Accepted(_)));
    let g = s.grant(gid_a).await.unwrap().unwrap();
    assert_eq!(g.desktop_used, 4);
}

#[tokio::test]
async fn two_screenshots_per_window_then_one_more_after_sixty_seconds() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Capture], |_| {}).await;
    let now = t0(&det);
    for i in 0..2 {
        let got = run(&s, &gid, &ctx, &format!("s{i}"), OpTool::Screenshot, now).await;
        assert!(matches!(got, Admission::Accepted(_)));
    }
    assert_eq!(
        run(&s, &gid, &ctx, "s2", OpTool::Screenshot, now).await,
        limited()
    );
    let g = s.grant(gid.clone()).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.capture_used), (2, 2));
    let later = run(&s, &gid, &ctx, "s2", OpTool::Screenshot, now + 60_000).await;
    assert!(matches!(later, Admission::Accepted(_)));
}

#[tokio::test]
async fn rate_rows_older_than_one_hundred_twenty_seconds_are_pruned() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |_| {}).await;
    let now = t0(&det);
    for i in 0..3 {
        run(&s, &gid, &ctx, &format!("r{i}"), OpTool::WallShow, now).await;
    }
    assert_eq!(rows(&s, "coordinator_desktop_rate").await, 3);
    // 100 s later the old rows stay; 121 s later they go.
    run(&s, &gid, &ctx, "r3", OpTool::WallShow, now + 100_000).await;
    assert_eq!(rows(&s, "coordinator_desktop_rate").await, 4);
    run(&s, &gid, &ctx, "r4", OpTool::WallShow, now + 121_000).await;
    assert_eq!(rows(&s, "coordinator_desktop_rate").await, 2);
}
