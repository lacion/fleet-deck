//! The `preview_close` stub of R13 P6 task F1: the action has its log name
//! and refuses with the same failure as a `window_close` of a window that
//! Fleet Deck did not open, without a program. Task T2 replaces the stub.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::desktop::DesktopAction as A;

use super::name;
use crate::desktop::fake::{Opts, Rig};
use crate::desktop::tests::{act_err, run};

#[test]
fn the_action_has_its_log_name() {
    let a = A::PreviewClose {
        window_id: "0123456789abcdef0123456789abcdef".into(),
    };
    assert_eq!(name(&a), "preview_close");
}

#[tokio::test]
async fn preview_close_is_refused_and_runs_no_program() {
    let rig = Rig::new(Opts::default());
    let e = run(
        &rig,
        A::PreviewClose {
            window_id: "0123456789abcdef0123456789abcdef".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        act_err(e),
        ("forbidden", "Fleet Deck did not open this window.".into())
    );
    assert!(rig.calls().is_empty());
}
