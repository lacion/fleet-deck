//! The admission stubs of R13 P7b task F: `pointer_click` under any grant and
//! every input tool under an `any_window` grant are refused with
//! `desktop_unsupported` before anything is stored or charged; the P6 tools
//! under an `any_window` grant, and the P7 tools under `own_windows`, are
//! admitted as before.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::coordinator_spec::InputScope;

use super::super::admit_tests::{grant, new_op, rejected, rows, store, t0};
use super::super::{Admission, OpClass, OpTool};
use crate::store::DeckStore;
use crate::store::grants::GrantClass;

const ALL: [GrantClass; 3] = [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];

async fn input_grant(
    s: &DeckStore,
    scope: InputScope,
    click: bool,
) -> (String, CoordinatorContext) {
    grant(s, "coord", &ALL, |g| {
        g.allow_input = true;
        g.input_scope = Some(scope);
        g.allow_click = click;
        g.input_cap = 10;
        g.input_character_cap = 100;
    })
    .await
}

fn input_op(gid: &str, ctx: &CoordinatorContext, name: &str, tool: OpTool) -> super::super::NewOp {
    let mut op = new_op(gid, ctx, name, tool);
    op.class = OpClass::Input;
    op.input_length = Some(u32::from(tool == OpTool::KeyPress));
    op
}

/// Nothing was stored and nothing of the grant was charged.
async fn assert_untouched(s: &DeckStore, gid: &str) {
    let g = s.grant(gid.to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (
            g.desktop_used,
            g.capture_used,
            g.input_used,
            g.input_character_used,
            g.desktop_attempts
        ),
        (0, 0, 0, 0, 0)
    );
    for table in [
        "coordinator_desktop_ops",
        "coordinator_desktop_rejects",
        "coordinator_desktop_rate",
    ] {
        assert_eq!(rows(s, table).await, 0, "{table}");
    }
}

#[tokio::test]
async fn a_pointer_click_is_refused_as_unsupported_under_any_input_grant_and_stores_nothing() {
    let (det, s) = store().await;
    for (session, scope, click) in [
        ("own", InputScope::OwnWindows, false),
        ("own-click", InputScope::OwnWindows, true),
        ("any", InputScope::AnyWindow, true),
    ] {
        let (gid, ctx) = grant(&s, session, &ALL, |g| {
            g.allow_input = true;
            g.input_scope = Some(scope);
            g.allow_click = click;
            g.input_cap = 10;
            g.input_character_cap = 100;
        })
        .await;
        let op = input_op(&gid, &ctx, "c1", OpTool::PointerClick);
        let got = s.desktop_admit(op, t0(&det)).await.unwrap();
        assert_eq!(
            rejected(got),
            (code::DESKTOP_UNSUPPORTED, false),
            "{session}"
        );
        assert_untouched(&s, &gid).await;
    }
}

#[tokio::test]
async fn an_input_grant_without_the_allowance_also_gets_the_unsupported_click() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &ALL, |_| {}).await;
    let op = input_op(&gid, &ctx, "c1", OpTool::PointerClick);
    let got = s.desktop_admit(op, t0(&det)).await.unwrap();
    assert_eq!(rejected(got), (code::DESKTOP_UNSUPPORTED, false));
    assert_untouched(&s, &gid).await;
}

#[tokio::test]
async fn every_input_tool_under_an_any_window_grant_is_refused_as_unsupported() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, InputScope::AnyWindow, false).await;
    for (name, tool) in [
        ("k1", OpTool::KeyPress),
        ("t1", OpTool::TypeText),
        ("m1", OpTool::PointerMove),
    ] {
        let op = input_op(&gid, &ctx, name, tool);
        let got = s.desktop_admit(op, t0(&det)).await.unwrap();
        assert_eq!(rejected(got), (code::DESKTOP_UNSUPPORTED, false), "{name}");
    }
    assert_untouched(&s, &gid).await;
    // A replay of the same request is refused the same way: nothing was kept.
    let op = input_op(&gid, &ctx, "k1", OpTool::KeyPress);
    let got = s.desktop_admit(op, t0(&det)).await.unwrap();
    assert_eq!(rejected(got), (code::DESKTOP_UNSUPPORTED, false));
    assert_untouched(&s, &gid).await;
}

#[tokio::test]
async fn the_p6_tools_under_an_any_window_grant_and_the_p7_tools_under_own_windows_still_work() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, InputScope::AnyWindow, true).await;
    for (name, tool) in [("n1", OpTool::Notify), ("s1", OpTool::Screenshot)] {
        let got = s
            .desktop_admit(new_op(&gid, &ctx, name, tool), t0(&det))
            .await
            .unwrap();
        assert!(matches!(got, Admission::Accepted(_)), "{name}: {got:?}");
    }
    let (own, own_ctx) = grant(&s, "other", &ALL, |g| {
        g.allow_input = true;
        g.input_scope = Some(InputScope::OwnWindows);
        g.input_cap = 10;
        g.input_character_cap = 100;
    })
    .await;
    let got = s
        .desktop_admit(input_op(&own, &own_ctx, "k1", OpTool::KeyPress), t0(&det))
        .await
        .unwrap();
    assert!(matches!(got, Admission::Accepted(_)), "{got:?}");
}

#[test]
fn the_new_tool_and_bucket_have_their_stored_names() {
    assert_eq!(OpTool::PointerClick.as_str(), "pointer_click");
    assert_eq!(super::super::rate::WINDOW_LIST, "window_list");
}
