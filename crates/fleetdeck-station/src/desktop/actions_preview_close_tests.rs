//! The log name of `preview_close` and the owner call without a deck: it
//! finds no preview and runs no program (R13 P6).

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
async fn an_owner_preview_close_without_a_deck_is_not_found_and_runs_no_program() {
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
        (
            "desktop_window_not_found",
            "No window opened by this grant matches that handle.".into()
        )
    );
    assert!(rig.calls().is_empty());
}
