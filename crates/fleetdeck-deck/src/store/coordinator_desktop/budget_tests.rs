//! The image reservation, the query rate and the cap change of the
//! coordinator desktop tools (R13 P6 §8.1, §8.2, §2.2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::code;

use super::admit_tests::{both, grant, new_op, rejected, rows, store, t0};
use super::*;
use crate::store::call;
use crate::store::grants::GrantClass;

const RESERVE: u64 = 716_800;
const MIB: u64 = 1_048_576;

#[tokio::test]
async fn each_reservation_charges_716800_bytes_and_the_cap_refuses_the_next() {
    let (det, s) = store().await;
    let (gid, _) = grant(&s, "coord", &[GrantClass::Capture], |g| {
        g.image_byte_cap = Some(2 * MIB);
    })
    .await;
    let now = t0(&det);
    for n in 1..=2u64 {
        assert_eq!(s.desktop_image_reserve(&gid, now).await.unwrap(), Ok(()));
        let g = s.grant(gid.clone()).await.unwrap().unwrap();
        assert_eq!(g.image_byte_used, n * RESERVE);
    }
    // 3 x 716,800 = 2,150,400 > 2,097,152.
    assert_eq!(
        s.desktop_image_reserve(&gid, now).await.unwrap(),
        Err(code::IMAGE_BUDGET_REACHED)
    );
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(g.image_byte_used, 2 * RESERVE);
}

#[tokio::test]
async fn a_cap_of_zero_refuses_the_first_reservation() {
    let (det, s) = store().await;
    let (gid, _) = grant(&s, "coord", &[GrantClass::Capture], |g| {
        g.image_byte_cap = Some(0);
    })
    .await;
    assert_eq!(
        s.desktop_image_reserve(&gid, t0(&det)).await.unwrap(),
        Err(code::IMAGE_BUDGET_REACHED)
    );
}

#[tokio::test]
async fn the_seventh_reservation_in_a_window_is_rate_limited_and_charges_nothing() {
    let (det, s) = store().await;
    let (gid, _) = both(&s).await;
    let now = t0(&det);
    for _ in 0..6 {
        assert_eq!(s.desktop_image_reserve(&gid, now).await.unwrap(), Ok(()));
    }
    assert_eq!(
        s.desktop_image_reserve(&gid, now).await.unwrap(),
        Err(code::RATE_LIMITED)
    );
    let g = s.grant(gid.clone()).await.unwrap().unwrap();
    assert_eq!(g.image_byte_used, 6 * RESERVE);
    // A new window accepts again.
    assert_eq!(
        s.desktop_image_reserve(&gid, now + 60_000).await.unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn a_reservation_needs_an_active_unexpired_grant_with_the_capture_class() {
    let (det, s) = store().await;
    let now = t0(&det);
    assert_eq!(
        s.desktop_image_reserve("no-such-grant", now).await.unwrap(),
        Err(code::NOT_GRANTED)
    );
    let (desktop_only, _) = grant(&s, "coord", &[GrantClass::Desktop], |_| {}).await;
    assert_eq!(
        s.desktop_image_reserve(&desktop_only, now).await.unwrap(),
        Err(code::NOT_GRANTED)
    );
    let (gid, _) = grant(&s, "coord-2", &[GrantClass::Capture], |_| {}).await;
    assert_eq!(
        s.desktop_image_reserve(&gid, now + 13 * 3_600_000)
            .await
            .unwrap(),
        Err(code::NOT_GRANTED)
    );
    s.revoke_grant(gid.clone(), "test".to_owned())
        .await
        .unwrap();
    assert_eq!(
        s.desktop_image_reserve(&gid, now).await.unwrap(),
        Err(code::NOT_GRANTED)
    );
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(g.image_byte_used, 0);
}

#[tokio::test]
async fn the_sixty_first_query_in_a_window_is_rate_limited() {
    let (det, s) = store().await;
    let (gid, _) = both(&s).await;
    let now = t0(&det);
    for i in 0..60 {
        assert_eq!(
            s.desktop_query_admit(&gid, now).await.unwrap(),
            Ok(()),
            "{i}"
        );
    }
    assert_eq!(
        s.desktop_query_admit(&gid, now).await.unwrap(),
        Err(code::RATE_LIMITED)
    );
    assert_eq!(
        s.desktop_query_admit(&gid, now + 60_000).await.unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn a_decrease_needs_no_elevation_and_keeps_the_values_that_are_none() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    s.desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Screenshot), t0(&det))
        .await
        .unwrap();
    s.desktop_image_reserve(&gid, t0(&det))
        .await
        .unwrap()
        .unwrap();
    let before = s.grant(gid.clone()).await.unwrap().unwrap();
    let got = s
        .desktop_caps_update(
            &gid,
            CapsChange {
                desktop_cap: Some(100),
                image_byte_cap: Some(MIB),
                ..CapsChange::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(got, Ok(CapsChanged { increased: false }));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(
        (g.desktop_cap, g.capture_cap, g.image_byte_cap),
        (Some(100), before.capture_cap, Some(MIB))
    );
    // The counters do not change.
    assert_eq!(
        (
            g.desktop_used,
            g.capture_used,
            g.image_byte_used,
            g.desktop_attempts
        ),
        (
            before.desktop_used,
            before.capture_used,
            before.image_byte_used,
            before.desktop_attempts
        )
    );
}

#[tokio::test]
async fn an_increase_without_elevation_is_refused_and_changes_nothing() {
    let (_det, s) = store().await;
    let (gid, _) = both(&s).await;
    let before = s.grant(gid.clone()).await.unwrap().unwrap();
    // One cap grows while another shrinks: still an increase.
    let got = s
        .desktop_caps_update(
            &gid,
            CapsChange {
                desktop_cap: Some(400),
                capture_cap: Some(1),
                ..CapsChange::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(got, Err(CapsError::ElevationRequired));
    let got = s
        .desktop_caps_update(
            &gid,
            CapsChange {
                image_byte_cap: Some(65 * MIB),
                ..CapsChange::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(got, Err(CapsError::ElevationRequired));
    assert_eq!(s.grant(gid).await.unwrap().unwrap(), before);
}

#[tokio::test]
async fn an_increase_with_elevation_succeeds() {
    let (_det, s) = store().await;
    let (gid, _) = both(&s).await;
    let got = s
        .desktop_caps_update(
            &gid,
            CapsChange {
                desktop_cap: Some(400),
                capture_cap: Some(30),
                image_byte_cap: Some(128 * MIB),
                ..CapsChange::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(got, Ok(CapsChanged { increased: true }));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(
        (g.desktop_cap, g.capture_cap, g.image_byte_cap),
        (Some(400), Some(30), Some(128 * MIB))
    );
}

#[tokio::test]
async fn a_cap_without_its_class_is_invalid_with_the_fixed_text() {
    let (_det, s) = store().await;
    let (gid, _) = grant(&s, "coord", &[GrantClass::Desktop], |_| {}).await;
    let got = s
        .desktop_caps_update(
            &gid,
            CapsChange {
                capture_cap: Some(5),
                ..CapsChange::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        got,
        Err(CapsError::Invalid(
            "capture_cap and image_byte_cap need the capture class"
        ))
    );
    let got = s
        .desktop_caps_update(
            &gid,
            CapsChange {
                desktop_cap: Some(5001),
                ..CapsChange::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(got, Err(CapsError::Invalid("desktop_cap is 0 to 5000")));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(g.desktop_cap, Some(300));
}

#[tokio::test]
async fn a_missing_or_revoked_grant_is_not_found() {
    let (_det, s) = store().await;
    let got = s
        .desktop_caps_update(
            "no-such-grant",
            CapsChange {
                desktop_cap: Some(1),
                ..CapsChange::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(got, Err(CapsError::NotFound));
    let (gid, _) = both(&s).await;
    s.revoke_grant(gid.clone(), "test".to_owned())
        .await
        .unwrap();
    let got = s
        .desktop_caps_update(
            &gid,
            CapsChange {
                desktop_cap: Some(1),
                ..CapsChange::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(got, Err(CapsError::NotFound));
}

#[tokio::test]
async fn a_cap_of_zero_refuses_the_first_operation_and_stores_the_refusal() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(0);
    })
    .await;
    let got = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Notify), t0(&det))
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::DESKTOP_CAP_REACHED, true));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (0, 1));
    assert_eq!(rows(&s, "coordinator_desktop_rejects").await, 1);
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 0);
    assert_eq!(rows(&s, "coordinator_desktop_rate").await, 0);
}

#[tokio::test]
async fn a_cap_of_two_accepts_two_and_refuses_the_third_with_a_stored_refusal() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(2);
    })
    .await;
    let now = t0(&det);
    for name in ["r1", "r2"] {
        let got = s
            .desktop_admit(new_op(&gid, &ctx, name, OpTool::Notify), now)
            .await
            .unwrap();
        assert!(matches!(got, Admission::Accepted(_)));
    }
    let got = s
        .desktop_admit(new_op(&gid, &ctx, "r3", OpTool::Notify), now)
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::DESKTOP_CAP_REACHED, true));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (2, 3));
}

#[tokio::test]
async fn the_screenshot_cap_refuses_with_its_own_stored_code() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Capture], |g| {
        g.capture_cap = Some(1);
    })
    .await;
    // A 60 s clock step keeps the screenshot rate (2 per window) out of the way.
    let now = t0(&det);
    let first = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Screenshot), now)
        .await
        .unwrap();
    assert!(matches!(first, Admission::Accepted(_)));
    let second = s
        .desktop_admit(new_op(&gid, &ctx, "r2", OpTool::Screenshot), now + 60_000)
        .await
        .unwrap();
    assert_eq!(rejected(second), (code::CAPTURE_CAP_REACHED, true));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(
        (g.desktop_used, g.capture_used, g.desktop_attempts),
        (1, 1, 2)
    );
}

#[tokio::test]
async fn ten_thousand_attempts_refuse_with_request_budget_reached_and_store_nothing() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let id = gid.clone();
    call(&s.inner.db, move |conn| {
        conn.execute(
            "UPDATE tokens SET desktop_attempts = 10000 WHERE grant_id = ?1",
            [id],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let got = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Notify), t0(&det))
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::REQUEST_BUDGET_REACHED, false));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (0, 10_000));
    assert_eq!(rows(&s, "coordinator_desktop_rejects").await, 0);
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 0);
}

#[tokio::test]
async fn two_parallel_admissions_for_the_last_slot_accept_exactly_one() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(1);
    })
    .await;
    let now = t0(&det);
    let (a, b) = tokio::join!(
        s.desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Notify), now),
        s.desktop_admit(new_op(&gid, &ctx, "r2", OpTool::WallShow), now),
    );
    let results = [a.unwrap(), b.unwrap()];
    let accepted = results
        .iter()
        .filter(|a| matches!(a, Admission::Accepted(_)))
        .count();
    let refused = results
        .iter()
        .filter(|a| {
            matches!(
                a,
                Admission::Rejected {
                    code: code::DESKTOP_CAP_REACHED,
                    stored: true
                }
            )
        })
        .count();
    assert_eq!((accepted, refused), (1, 1));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(g.desktop_used, g.desktop_cap.unwrap());
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 1);
}

/// A grant of both classes with the recording caps 5 and 600, and a used
/// count of 2 recordings and 90 seconds.
async fn used_recording_grant(s: &crate::store::DeckStore) -> String {
    let (gid, _) = grant(
        s,
        "coord",
        &[GrantClass::Desktop, GrantClass::Capture],
        |g| {
            g.recording_cap = 5;
            g.recording_second_cap = 600;
        },
    )
    .await;
    let id = gid.clone();
    call(&s.inner.db, move |conn| {
        conn.execute(
            "UPDATE tokens SET recording_used = 2, recording_second_used = 90 WHERE grant_id = ?1",
            [id],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    gid
}

fn recording_change(cap: Option<u32>, seconds: Option<u32>) -> CapsChange {
    CapsChange {
        recording_cap: cap,
        recording_second_cap: seconds,
        ..CapsChange::default()
    }
}

#[tokio::test]
async fn a_recording_raise_needs_the_elevation_and_keeps_the_counters() {
    let (_det, s) = store().await;
    let gid = used_recording_grant(&s).await;
    let before = s.grant(gid.clone()).await.unwrap().unwrap();
    let got = s
        .desktop_caps_update(&gid, recording_change(Some(6), None), false)
        .await
        .unwrap();
    assert_eq!(got, Err(CapsError::ElevationRequired));
    let got = s
        .desktop_caps_update(&gid, recording_change(None, Some(601)), false)
        .await
        .unwrap();
    assert_eq!(got, Err(CapsError::ElevationRequired));
    assert_eq!(s.grant(gid.clone()).await.unwrap().unwrap(), before);
    let got = s
        .desktop_caps_update(&gid, recording_change(Some(50), Some(6000)), true)
        .await
        .unwrap();
    assert_eq!(got, Ok(CapsChanged { increased: true }));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.recording_cap, g.recording_second_cap), (50, 6000));
    assert_eq!(
        (
            g.desktop_used,
            g.capture_used,
            g.recording_used,
            g.recording_second_used
        ),
        (
            before.desktop_used,
            before.capture_used,
            before.recording_used,
            before.recording_second_used
        )
    );
    assert_eq!(
        (g.desktop_cap, g.capture_cap, g.image_byte_cap),
        (
            before.desktop_cap,
            before.capture_cap,
            before.image_byte_cap
        )
    );
}

#[tokio::test]
async fn a_recording_decrease_needs_no_elevation_and_may_go_below_the_use() {
    let (_det, s) = store().await;
    let gid = used_recording_grant(&s).await;
    let got = s
        .desktop_caps_update(&gid, recording_change(Some(1), Some(30)), false)
        .await
        .unwrap();
    assert_eq!(got, Ok(CapsChanged { increased: false }));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.recording_cap, g.recording_second_cap), (1, 30));
    assert_eq!((g.recording_used, g.recording_second_used), (2, 90));
}

#[tokio::test]
async fn a_recording_key_on_a_grant_without_the_allowance_is_invalid() {
    let (_det, s) = store().await;
    let (gid, _) = both(&s).await;
    for change in [
        recording_change(Some(1), None),
        recording_change(None, Some(1)),
        recording_change(Some(0), Some(0)),
    ] {
        let got = s.desktop_caps_update(&gid, change, true).await.unwrap();
        assert_eq!(
            got,
            Err(CapsError::Invalid(
                "recording limits need a grant that allows recordings"
            ))
        );
    }
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.recording_cap, g.recording_second_cap), (0, 0));
}

#[tokio::test]
async fn a_recording_cap_out_of_range_is_invalid_with_the_fixed_text() {
    let (_det, s) = store().await;
    let gid = used_recording_grant(&s).await;
    let got = s
        .desktop_caps_update(&gid, recording_change(Some(5001), None), true)
        .await
        .unwrap();
    assert_eq!(got, Err(CapsError::Invalid("recording_cap is 0 to 5000")));
    let got = s
        .desktop_caps_update(&gid, recording_change(None, Some(600_001)), true)
        .await
        .unwrap();
    assert_eq!(
        got,
        Err(CapsError::Invalid("recording_second_cap is 0 to 600000"))
    );
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.recording_cap, g.recording_second_cap), (5, 600));
}
