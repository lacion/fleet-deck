//! The agent desktop rates are the owner knobs (K2): the admission refuses at
//! the configured count of actions and of screenshots in a window.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;

use super::super::admit_tests::{both, grant, new_op, store, t0};
use super::super::{Admission, OpTool};
use crate::knobs::DeckKnobs;
use crate::store::DeckStore;
use crate::store::grants::GrantClass;

async fn run(
    s: &DeckStore,
    gid: &str,
    ctx: &CoordinatorContext,
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
async fn the_action_rate_is_the_configured_count_per_window() {
    let (det, s) = store().await;
    s.set_knobs(DeckKnobs {
        mutations_per_window: 3,
        ..DeckKnobs::default()
    });
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(100);
    })
    .await;
    let now = t0(&det);
    for i in 0..3 {
        let got = run(&s, &gid, &ctx, &format!("r{i}"), OpTool::WallShow, now).await;
        assert!(matches!(got, Admission::Accepted(_)), "{i}");
    }
    assert_eq!(
        run(&s, &gid, &ctx, "r3", OpTool::WallShow, now).await,
        limited()
    );
    // A raised rate takes effect for the next operation.
    s.set_knobs(DeckKnobs {
        mutations_per_window: 4,
        ..DeckKnobs::default()
    });
    let got = run(&s, &gid, &ctx, "r3", OpTool::WallShow, now).await;
    assert!(matches!(got, Admission::Accepted(_)));
}

#[tokio::test]
async fn the_screenshot_rate_is_the_configured_count_per_window() {
    let (det, s) = store().await;
    s.set_knobs(DeckKnobs {
        screenshots_per_window: 1,
        ..DeckKnobs::default()
    });
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    let got = run(&s, &gid, &ctx, "s0", OpTool::Screenshot, now).await;
    assert!(matches!(got, Admission::Accepted(_)));
    assert_eq!(
        run(&s, &gid, &ctx, "s1", OpTool::Screenshot, now).await,
        limited()
    );
    // The default allows a second screenshot.
    s.set_knobs(DeckKnobs::default());
    let got = run(&s, &gid, &ctx, "s1", OpTool::Screenshot, now).await;
    assert!(matches!(got, Admission::Accepted(_)));
}
