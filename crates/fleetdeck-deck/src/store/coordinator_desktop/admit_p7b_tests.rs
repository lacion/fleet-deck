//! The admission of `pointer_click` and of the input tools under both scopes
//! (R13 P7b §3.1, §9.2, §11.3, task K1), and the rate of the `window_list`
//! read (R13 P7b §6.1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::{code, limits};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::coordinator_spec::InputScope;

use super::super::admit_tests::{grant, new_op, rejected, rows, store, t0};
use super::super::{Admission, OpClass, OpTool};
use crate::store::grants::GrantClass;
use crate::store::{DeckStore, call};

const ALL: [GrantClass; 3] = [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];

async fn input_grant(
    s: &DeckStore,
    session: &str,
    scope: InputScope,
    click: bool,
) -> (String, CoordinatorContext) {
    grant(s, session, &ALL, |g| {
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

/// The rows of one rate bucket.
async fn bucket(s: &DeckStore, name: &'static str) -> i64 {
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM coordinator_desktop_rate WHERE bucket = ?1",
            [name],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_click_charges_one_mutation_one_input_action_no_character_and_one_input_rate_entry() {
    let (det, s) = store().await;
    for (session, scope) in [
        ("own", InputScope::OwnWindows),
        ("any", InputScope::AnyWindow),
    ] {
        let (gid, ctx) = input_grant(&s, session, scope, true).await;
        let mut op = input_op(&gid, &ctx, session, OpTool::PointerClick);
        op.input_length = Some(0);
        let Admission::Accepted(row) = s.desktop_admit(op, t0(&det)).await.unwrap() else {
            panic!("expected Accepted: {session}");
        };
        assert_eq!(row.tool, OpTool::PointerClick, "{session}");
        assert_eq!(row.input_length, Some(0));
        let g = s.grant(gid.clone()).await.unwrap().unwrap();
        assert_eq!(
            (
                g.desktop_used,
                g.capture_used,
                g.input_used,
                g.input_character_used,
                g.desktop_attempts
            ),
            (1, 0, 1, 0, 1),
            "{session}"
        );
        // The stored charges: one desktop action, one input action and no
        // typed character.
        let id = row.operation_id.clone();
        let charged: String = call(&s.inner.db, move |conn| {
            Ok(conn.query_row(
                "SELECT charged FROM coordinator_desktop_ops WHERE operation_id = ?1",
                [id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
        let charged: serde_json::Value = serde_json::from_str(&charged).unwrap();
        assert_eq!(
            charged,
            serde_json::json!({"desktop": 1, "capture": 0, "input": 1, "input_characters": 0}),
            "{session}"
        );
    }
    // One mutation entry for each grant, and one input entry for the grant
    // and one for the caller session of each.
    assert_eq!(bucket(&s, "mutation").await, 2);
    assert_eq!(bucket(&s, "input").await, 4);
}

#[tokio::test]
async fn a_click_without_the_click_allowance_is_refused_and_stores_nothing() {
    for (session, scope) in [
        ("own", InputScope::OwnWindows),
        ("any", InputScope::AnyWindow),
    ] {
        let (det, s) = store().await;
        let (gid, ctx) = input_grant(&s, session, scope, false).await;
        let op = input_op(&gid, &ctx, "c1", OpTool::PointerClick);
        let got = s.desktop_admit(op, t0(&det)).await.unwrap();
        assert_eq!(
            rejected(got),
            (code::INPUT_CLICK_NOT_ALLOWED, false),
            "{session}"
        );
        assert_untouched(&s, &gid).await;
        // The other input tools of the same grant are admitted.
        let got = s
            .desktop_admit(input_op(&gid, &ctx, "k1", OpTool::KeyPress), t0(&det))
            .await
            .unwrap();
        assert!(matches!(got, Admission::Accepted(_)), "{session}: {got:?}");
    }
}

#[tokio::test]
async fn the_classes_and_the_input_allowance_come_before_the_click_allowance() {
    let (det, s) = store().await;
    // A missing class.
    let (gid, ctx) = grant(
        &s,
        "no-class",
        &[GrantClass::Desktop, GrantClass::Capture],
        |_| {},
    )
    .await;
    let got = s
        .desktop_admit(input_op(&gid, &ctx, "c1", OpTool::PointerClick), t0(&det))
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::NOT_GRANTED, false));
    assert_untouched(&s, &gid).await;
    // The classes without the input allowance.
    let (gid, ctx) = grant(&s, "no-allowance", &ALL, |_| {}).await;
    let got = s
        .desktop_admit(input_op(&gid, &ctx, "c1", OpTool::PointerClick), t0(&det))
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::INPUT_NOT_ALLOWED, false));
    assert_untouched(&s, &gid).await;
}

#[tokio::test]
async fn both_scopes_admit_every_input_tool() {
    let (det, s) = store().await;
    for (session, scope) in [
        ("own", InputScope::OwnWindows),
        ("any", InputScope::AnyWindow),
    ] {
        let (gid, ctx) = input_grant(&s, session, scope, true).await;
        for (name, tool) in [
            ("k1", OpTool::KeyPress),
            ("t1", OpTool::TypeText),
            ("m1", OpTool::PointerMove),
            ("c1", OpTool::PointerClick),
        ] {
            let mut op = input_op(&gid, &ctx, &format!("{session}-{name}"), tool);
            if tool == OpTool::TypeText {
                op.input_length = Some(3);
            }
            let got = s.desktop_admit(op, t0(&det)).await.unwrap();
            assert!(
                matches!(got, Admission::Accepted(_)),
                "{session} {name}: {got:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_p6_tools_under_an_any_window_grant_are_admitted_as_before() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, "any", InputScope::AnyWindow, true).await;
    for (name, tool) in [("n1", OpTool::Notify), ("s1", OpTool::Screenshot)] {
        let got = s
            .desktop_admit(new_op(&gid, &ctx, name, tool), t0(&det))
            .await
            .unwrap();
        assert!(matches!(got, Admission::Accepted(_)), "{name}: {got:?}");
    }
}

#[tokio::test]
async fn the_window_list_rate_is_ten_for_the_grant_and_ten_for_the_caller_session() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, "any", InputScope::AnyWindow, false).await;
    let now = t0(&det);
    for i in 0..limits::WINDOW_LIST_PER_WINDOW {
        let got = s.desktop_window_list_admit(&ctx, now + i64::from(i)).await;
        assert_eq!(got.unwrap(), Ok(()), "{i}");
    }
    // The 11th read of the window is refused and charges nothing.
    let at = now + 20;
    assert_eq!(
        s.desktop_window_list_admit(&ctx, at).await.unwrap(),
        Err(code::RATE_LIMITED)
    );
    assert_eq!(bucket(&s, "window_list").await, 20);
    assert_eq!(bucket(&s, "query").await, 10);
    // The list read uses no budget of the grant.
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(
        (
            g.desktop_used,
            g.input_used,
            g.input_character_used,
            g.desktop_attempts
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(bucket(&s, "mutation").await, 0);
    // The window moves on: a minute later the reads are admitted again.
    let later = now + 61_000;
    assert_eq!(
        s.desktop_window_list_admit(&ctx, later).await.unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn the_session_bucket_alone_limits_the_window_list_read() {
    let (det, s) = store().await;
    let (_g1, ctx1) = input_grant(&s, "any", InputScope::AnyWindow, false).await;
    let now = t0(&det);
    // The session bucket alone: ten entries for the session key of the
    // caller, none for the grant.
    call(&s.inner.db, {
        let key = format!("session:{}/{}", ctx1.caller_station, ctx1.caller_session);
        move |conn| {
            for i in 0..10 {
                conn.execute(
                    "INSERT INTO coordinator_desktop_rate (scope_key, bucket, at)
                     VALUES (?1, 'window_list', ?2)",
                    rusqlite::params![key, now + i],
                )?;
            }
            Ok(())
        }
    })
    .await
    .unwrap();
    assert_eq!(
        s.desktop_window_list_admit(&ctx1, now + 50).await.unwrap(),
        Err(code::RATE_LIMITED)
    );
    // The query bucket of the refused read was not charged.
    assert_eq!(bucket(&s, "query").await, 0);
}

#[test]
fn the_new_tool_and_bucket_have_their_stored_names() {
    assert_eq!(OpTool::PointerClick.as_str(), "pointer_click");
    assert_eq!(super::super::rate::WINDOW_LIST, "window_list");
}
