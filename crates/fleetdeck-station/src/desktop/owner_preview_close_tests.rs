//! The owner closes a preview that an agent opened, and the list of previews
//! in the desktop info (R13 P6 follow-up). The fake desktop has no real
//! windows; the checks are the programs that ran and the preview records.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction as A, PreviewWindow};
use fleetdeck_proto::version::feature;

use super::Fail;
use super::authority::Admitted;
use super::authority_tests::Harness;
use super::coordinator_tests::{DECK, FakeCheck, coordinated, ctx, extras, negotiated, open_gates};
use super::fake::{Opts, Rig};
use super::tests::{act_err, eventually, on};

const NOT_FOUND_TEXT: &str = "No window opened by this grant matches that handle.";
const OTHER_DECK: &str = "cd00000000000000000000000000000000000000000000000000000000000002";

async fn open(rig: &Rig, h: &Harness, n: u64, monitor: Option<&str>) -> String {
    let action = A::BrowserOpen {
        url: "http://localhost:3000/".to_owned(),
        monitor: monitor.map(str::to_owned),
        session: None,
    };
    let admitted = coordinated(
        h,
        &action,
        n,
        &ctx(),
        FakeCheck::ok(),
        open_gates(),
        extras(),
    );
    rig.desktop
        .act(&on, action, admitted)
        .await
        .unwrap()
        .window_id
        .unwrap()
}

/// An owner `preview_close` on the link of `deck`.
async fn owner_close(
    rig: &Rig,
    deck: Option<&str>,
    handle: &str,
) -> Result<DesktopActResult, Fail> {
    let action = A::PreviewClose {
        window_id: handle.to_owned(),
    };
    rig.desktop
        .act_for_deck(&on, action, Admitted::open(), deck.map(str::to_owned))
        .await
}

fn closes(rig: &Rig) -> usize {
    rig.calls_of("hyprctl")
        .iter()
        .filter(|c| c.args.iter().any(|a| a == "closewindow"))
        .count()
}

fn not_found(e: Fail) -> bool {
    act_err(e) == ("desktop_window_not_found", NOT_FOUND_TEXT.to_owned())
}

#[tokio::test]
async fn an_owner_closes_a_preview_that_an_agent_opened_after_the_identity_check() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let handle = open(&rig, &h, 1, None).await;
    let (_, record) = rig.desktop.events.preview(&handle).unwrap();
    let result = owner_close(&rig, Some(DECK), &handle).await.unwrap();
    assert_eq!((result.address, result.window_id), (None, None));
    assert_eq!(closes(&rig), 1);
    let sent = rig.calls_of("hyprctl");
    assert_eq!(
        sent[sent.len() - 1].args,
        [
            "dispatch",
            "closewindow",
            &format!("address:{}", record.address)
        ]
    );
    eventually("the record goes", || {
        rig.desktop.events.preview(&handle).is_none()
    })
    .await;
    // The handle is gone: a second close finds nothing.
    let e = owner_close(&rig, Some(DECK), &handle).await.unwrap_err();
    assert!(not_found(e));
    assert_eq!(closes(&rig), 1);
}

#[tokio::test]
async fn a_stale_window_is_not_closed_by_the_owner() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let handle = open(&rig, &h, 1, None).await;
    let (_, record) = rig.desktop.events.preview(&handle).unwrap();
    // The compositor lists the address with another class now.
    std::fs::write(
        rig.dir.path().join("clients.json"),
        serde_json::to_string(&serde_json::json!([
            {"address": record.address, "class": "someone-else"}
        ]))
        .unwrap(),
    )
    .unwrap();
    let e = owner_close(&rig, Some(DECK), &handle).await.unwrap_err();
    assert_eq!(act_err(e), ("not_found", "The window is gone.".into()));
    assert_eq!(closes(&rig), 0);
}

#[tokio::test]
async fn an_unknown_handle_a_handle_of_another_deck_and_a_call_without_a_deck_are_not_found() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let handle = open(&rig, &h, 1, None).await;
    let e = owner_close(&rig, Some(DECK), &"0".repeat(32))
        .await
        .unwrap_err();
    assert!(not_found(e));
    let e = owner_close(&rig, Some(OTHER_DECK), &handle)
        .await
        .unwrap_err();
    assert!(not_found(e));
    let e = owner_close(&rig, None, &handle).await.unwrap_err();
    assert!(not_found(e));
    assert_eq!(closes(&rig), 0);
    assert!(rig.desktop.events.preview(&handle).is_some());
}

#[tokio::test]
async fn a_coordinator_close_still_needs_its_own_ownership() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let handle = open(&rig, &h, 1, None).await;
    let other = CoordinatorContext {
        grant_id: "grant-2".into(),
        ..ctx()
    };
    let action = A::PreviewClose {
        window_id: handle.clone(),
    };
    let admitted = coordinated(
        &h,
        &action,
        2,
        &other,
        FakeCheck::ok(),
        open_gates(),
        extras(),
    );
    let e = rig.desktop.act(&on, action, admitted).await.unwrap_err();
    assert!(not_found(e));
    assert_eq!(closes(&rig), 0);
}

#[tokio::test]
async fn the_list_holds_the_previews_of_this_deck_only_and_only_with_the_feature() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let first = open(&rig, &h, 1, None).await;
    let second = open(&rig, &h, 2, Some("HDMI-A-1")).await;
    let with = negotiated(&[feature::DESKTOP_MCP]);
    let without = negotiated(&[feature::DESKTOP]);
    let list = rig.desktop.previews_for_link(DECK, &with);
    assert_eq!(
        list,
        vec![
            PreviewWindow {
                window_id: first.clone(),
                monitor: None,
                caller_label: "bold-fox".into(),
            },
            PreviewWindow {
                window_id: second.clone(),
                monitor: Some("HDMI-A-1".into()),
                caller_label: "bold-fox".into(),
            },
        ]
    );
    // The wire form holds no address.
    let json = serde_json::to_string(&list).unwrap();
    assert!(!json.contains("0x"), "{json}");
    assert!(rig.desktop.previews_for_link(DECK, &without).is_empty());
    assert!(rig.desktop.previews_for_link(OTHER_DECK, &with).is_empty());
    // A closed preview leaves the list.
    owner_close(&rig, Some(DECK), &first).await.unwrap();
    eventually("the record goes", || {
        rig.desktop.previews_for_link(DECK, &with).len() == 1
    })
    .await;
    assert_eq!(
        rig.desktop.previews_for_link(DECK, &with)[0].window_id,
        second
    );
}
