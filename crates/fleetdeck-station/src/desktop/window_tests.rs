//! Window tests with a fake desktop (R13 P3a): `browser.open`, `window.close`
//! and the wall actions, with the records of the Hyprland event socket.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::AtomicBool;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::desktop::{DesktopAction as A, Urgency};

use super::fake::{Opts, Rig, SIGNATURE, call};
use super::tests::{act_err, eventually, on, run, url};

fn flag_value<'a>(args: &'a [String], name: &str) -> &'a str {
    args.iter()
        .find_map(|a| a.strip_prefix(name))
        .unwrap_or_else(|| panic!("no {name} in {args:?}"))
}

#[tokio::test]
async fn browser_open_starts_a_unique_window_and_moves_it() {
    let rig = Rig::new(Opts::default());
    let session = SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ");
    let address = run(
        &rig,
        A::BrowserOpen {
            url: "https://example.com/p?q=1".into(),
            monitor: Some("HDMI-A-1".into()),
            session: Some(session.clone()),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let chromium = &rig.calls_of("chromium")[0];
    let a = &chromium.args;
    assert_eq!(a[0], "--app=https://example.com/p?q=1");
    let id = flag_value(a, "--class=fleetdeck-preview-");
    assert_eq!(id.len(), 16);
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(
        a[1],
        format!(
            "--user-data-dir={}/fleetdeck/browser-{id}",
            rig.runtime.display()
        )
    );
    assert_eq!(a[2], format!("--class=fleetdeck-preview-{id}"));
    assert_eq!(a[3], "--no-first-run");
    assert_eq!(a.len(), 4);
    assert!(address.starts_with("0x"));
    // The window goes to the active workspace of the monitor (5).
    assert_eq!(
        rig.calls_of("hyprctl"),
        vec![call(
            "hyprctl",
            &[
                "dispatch",
                "movetoworkspacesilent",
                &format!("5,address:{address}")
            ]
        )]
    );
    assert!(rig.calls_of("uwsm-app").is_empty());
    // A second preview gets another class and another profile.
    run(
        &rig,
        A::BrowserOpen {
            url: "https://example.com".into(),
            monitor: None,
            session: None,
        },
    )
    .await
    .unwrap();
    let second = &rig.calls_of("chromium")[1].args;
    assert_ne!(flag_value(second, "--class="), flag_value(a, "--class="));
    assert_eq!(rig.calls_of("hyprctl").len(), 1, "no monitor: no move");
}

#[tokio::test]
async fn browser_open_goes_through_uwsm_app_when_it_is_installed() {
    let rig = Rig::new(Opts {
        uwsm: true,
        ..Opts::default()
    });
    run(
        &rig,
        A::BrowserOpen {
            url: "https://example.com".into(),
            monitor: None,
            session: None,
        },
    )
    .await
    .unwrap();
    assert!(rig.calls_of("chromium").is_empty());
    let uwsm = &rig.calls_of("uwsm-app")[0].args;
    assert_eq!(uwsm[..3], ["--", "chromium", "--app=https://example.com"]);
}

#[tokio::test]
async fn a_launch_that_stops_early_is_reported() {
    let rig = Rig::new(Opts::default());
    rig.chromium_fails();
    let e = run(
        &rig,
        A::BrowserOpen {
            url: "https://example.com".into(),
            monitor: None,
            session: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        act_err(e),
        ("failed", "`chromium` stopped with exit code 3".into())
    );
    let rig = Rig::without(Opts::default(), &["chromium"]);
    let e = run(
        &rig,
        A::BrowserOpen {
            url: "https://example.com".into(),
            monitor: None,
            session: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "unsupported");
}

#[tokio::test]
async fn window_close_closes_only_a_window_that_the_station_opened() {
    let rig = Rig::new(Opts::default());
    let e = run(
        &rig,
        A::WindowClose {
            address: "0x1234".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        act_err(e),
        ("forbidden", "Fleet Deck did not open this window.".into())
    );
    let e = run(
        &rig,
        A::WindowClose {
            address: "1234; reboot".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "invalid_params");
    assert!(rig.calls().is_empty());
    let address = run(
        &rig,
        A::BrowserOpen {
            url: "https://example.com".into(),
            monitor: None,
            session: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let profile = rig.calls_of("chromium")[0].args[1]["--user-data-dir=".len()..].to_owned();
    assert!(
        std::path::Path::new(&profile).is_dir(),
        "the fake chromium made its profile"
    );
    run(
        &rig,
        A::WindowClose {
            address: address.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        rig.calls_of("hyprctl").last().unwrap(),
        &call(
            "hyprctl",
            &["dispatch", "closewindow", &format!("address:{address}")]
        )
    );
    // The `closewindow` event deletes the record and the profile.
    eventually("the preview profile is deleted", || {
        !std::path::Path::new(&profile).exists()
    })
    .await;
    let e = run(&rig, A::WindowClose { address }).await.unwrap_err();
    assert_eq!(e.code, "forbidden");
}

#[tokio::test]
async fn a_window_that_is_gone_is_not_closed() {
    let rig = Rig::new(Opts::default());
    let address = run(
        &rig,
        A::BrowserOpen {
            url: "https://example.com".into(),
            monitor: None,
            session: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    rig.forget_client(&address);
    let e = run(
        &rig,
        A::WindowClose {
            address: address.clone(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(act_err(e), ("not_found", "The window is gone.".into()));
    assert!(
        rig.calls_of("hyprctl")
            .iter()
            .all(|c| c.args[1] != "closewindow")
    );
    // The record is deleted: a second close is refused as unknown.
    let e = run(&rig, A::WindowClose { address }).await.unwrap_err();
    assert_eq!(e.code, "forbidden");
}

#[tokio::test]
async fn a_window_with_another_class_is_not_closed() {
    let rig = Rig::new(Opts::default());
    let address = run(
        &rig,
        A::BrowserOpen {
            url: "https://example.com".into(),
            monitor: None,
            session: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    // Another window now uses the address.
    rig.forget_client(&address);
    std::fs::write(
        rig.dir.path().join("clients.json"),
        format!(r#"[{{"address":"{address}","class":"firefox"}}]"#),
    )
    .unwrap();
    let e = run(&rig, A::WindowClose { address }).await.unwrap_err();
    assert_eq!(e.code, "not_found");
    assert!(
        rig.calls_of("hyprctl")
            .iter()
            .all(|c| c.args[1] != "closewindow")
    );
}

#[tokio::test]
async fn wall_open_runs_a_kiosk_and_a_second_open_is_a_conflict() {
    let rig = Rig::new(Opts::default());
    let address = run(
        &rig,
        A::WallOpen {
            display_id: "wall-1".into(),
            monitor: "HDMI-A-1".into(),
            workspace: None,
            url: url(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let a = &rig.calls_of("chromium")[0].args;
    let id = flag_value(a, "--class=fleetdeck-wall-");
    assert_eq!(a[0], format!("--app={}", url()));
    assert_eq!(
        a[1],
        format!(
            "--user-data-dir={}/fleetdeck/wall-wall-1",
            rig.data.display()
        )
    );
    assert_eq!(a[2], format!("--class=fleetdeck-wall-{id}"));
    assert_eq!(a[3..], ["--no-first-run", "--kiosk"]);
    assert_eq!(
        rig.calls_of("hyprctl"),
        vec![
            call(
                "hyprctl",
                &[
                    "dispatch",
                    "movetoworkspacesilent",
                    &format!("5,address:{address}")
                ]
            ),
            call(
                "hyprctl",
                &["dispatch", "focuswindow", &format!("address:{address}")]
            ),
            call("hyprctl", &["dispatch", "fullscreen", "0"]),
        ]
    );
    let info = rig.desktop.info(&on).await.unwrap();
    assert_eq!(info.walls.len(), 1);
    assert_eq!(info.walls[0].display_id, "wall-1");
    assert_eq!(info.walls[0].monitor.as_deref(), Some("HDMI-A-1"));
    assert_eq!(info.walls[0].address, address);
    let e = run(
        &rig,
        A::WallOpen {
            display_id: "wall-1".into(),
            monitor: "DP-1".into(),
            workspace: Some(3),
            url: url(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        act_err(e),
        (
            "conflict",
            "This display already has a wall window; close it first.".into()
        )
    );
    assert_eq!(rig.calls_of("chromium").len(), 1);
    // Another display is fine, with an explicit workspace.
    run(
        &rig,
        A::WallOpen {
            display_id: "wall-2".into(),
            monitor: "DP-1".into(),
            workspace: Some(3),
            url: url(),
        },
    )
    .await
    .unwrap();
    assert!(
        rig.calls_of("hyprctl")
            .iter()
            .any(|c| c.args[1] == "movetoworkspacesilent" && c.args[2].starts_with("3,address:"))
    );
}

#[tokio::test]
async fn wall_open_checks_every_argument() {
    let rig = Rig::new(Opts::default());
    for (id, monitor, ws) in [
        ("a b", "DP-1", None),
        ("a/b", "DP-1", None),
        ("", "DP-1", None),
        ("ok", "nope", None),
        ("ok", "DP-1", Some(0)),
        ("ok", "DP-1", Some(100)),
    ] {
        let e = run(
            &rig,
            A::WallOpen {
                display_id: id.into(),
                monitor: monitor.into(),
                workspace: ws,
                url: url(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "invalid_params", "{id} {monitor} {ws:?}");
    }
    assert!(rig.calls().is_empty());
}

#[tokio::test]
async fn wall_close_closes_the_window_and_deletes_the_profile() {
    let rig = Rig::new(Opts::default());
    let e = run(
        &rig,
        A::WallClose {
            display_id: "wall-1".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "not_found");
    let address = run(
        &rig,
        A::WallOpen {
            display_id: "wall-1".into(),
            monitor: "DP-1".into(),
            workspace: None,
            url: url(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let profile = rig.data.join("fleetdeck/wall-wall-1");
    std::fs::write(profile.join("Cookies"), b"x").unwrap();
    run(
        &rig,
        A::WallClose {
            display_id: "wall-1".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        rig.calls_of("hyprctl").last().unwrap(),
        &call(
            "hyprctl",
            &["dispatch", "closewindow", &format!("address:{address}")]
        )
    );
    assert!(!profile.exists(), "the wall profile is deleted");
    assert!(rig.desktop.info(&on).await.unwrap().walls.is_empty());
}

#[tokio::test]
async fn wall_close_refuses_a_link_in_place_of_the_profile() {
    let rig = Rig::new(Opts::default());
    run(
        &rig,
        A::WallOpen {
            display_id: "wall-1".into(),
            monitor: "DP-1".into(),
            workspace: None,
            url: url(),
        },
    )
    .await
    .unwrap();
    let target = rig.dir.path().join("precious");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), b"x").unwrap();
    let profile = rig.data.join("fleetdeck/wall-wall-1");
    std::fs::remove_dir_all(&profile).unwrap();
    std::os::unix::fs::symlink(&target, &profile).unwrap();
    let e = run(
        &rig,
        A::WallClose {
            display_id: "wall-1".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        act_err(e),
        (
            "failed",
            "The wall profile is not a plain folder; it was not removed.".into()
        )
    );
    assert!(target.join("keep").exists());
    assert!(profile.is_symlink());
    // The window was closed all the same.
    assert!(
        rig.calls_of("hyprctl")
            .iter()
            .any(|c| c.args[1] == "closewindow")
    );
    // A wall must not open over a link either.
    let e = run(
        &rig,
        A::WallOpen {
            display_id: "wall-1".into(),
            monitor: "DP-1".into(),
            workspace: None,
            url: url(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "failed");
}

#[tokio::test]
async fn wall_reload_closes_and_opens_again_on_the_same_monitor() {
    let rig = Rig::new(Opts::default());
    let first = run(
        &rig,
        A::WallOpen {
            display_id: "wall-1".into(),
            monitor: "HDMI-A-1".into(),
            workspace: Some(9),
            url: url(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let second = run(
        &rig,
        A::WallReload {
            display_id: "wall-1".into(),
            url: url(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(first, second);
    assert_eq!(rig.calls_of("chromium").len(), 2);
    let moves: Vec<_> = rig
        .calls_of("hyprctl")
        .into_iter()
        .filter(|c| c.args[1] == "movetoworkspacesilent")
        .collect();
    // The reload uses the active workspace of the recorded monitor (5).
    assert_eq!(moves[1].args[2], format!("5,address:{second}"));
    let e = run(
        &rig,
        A::WallReload {
            display_id: "nope".into(),
            url: url(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "not_found");
}

#[tokio::test]
async fn the_records_are_cleared_when_the_event_stream_ends() {
    let rig = Rig::new(Opts::default());
    run(
        &rig,
        A::WallOpen {
            display_id: "wall-1".into(),
            monitor: "DP-1".into(),
            workspace: None,
            url: url(),
        },
    )
    .await
    .unwrap();
    assert_eq!(rig.desktop.info(&on).await.unwrap().walls.len(), 1);
    rig.drop_events_connection();
    eventually("the walls are forgotten", || {
        rig.desktop.events.walls().is_empty()
    })
    .await;
    // The station owns no window now: it cannot close the old one.
    let e = run(
        &rig,
        A::WallClose {
            display_id: "wall-1".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "not_found");
    assert_eq!(SIGNATURE, "sig_a_1");
}

#[test]
fn the_log_of_an_action_never_holds_the_arguments() {
    let notify = A::Notify {
        title: "T".into(),
        body: "B".into(),
        urgency: Urgency::Low,
    };
    assert_eq!(super::actions::name(&notify), "notify");
    assert_eq!(
        super::actions::name(&A::BrowserOpen {
            url: "https://x.example".into(),
            monitor: None,
            session: None
        }),
        "browser_open"
    );
    let _ = AtomicBool::new(true);
}

fn open_wall(display_id: &str) -> A {
    A::WallOpen {
        display_id: display_id.into(),
        monitor: "DP-1".into(),
        workspace: None,
        url: url(),
    }
}

fn close_requests(rig: &Rig) -> usize {
    rig.calls_of("hyprctl")
        .iter()
        .filter(|c| c.args.get(1).is_some_and(|a| a == "closewindow"))
        .count()
}

#[tokio::test]
async fn a_wall_close_that_is_not_confirmed_fails_and_keeps_the_profile_and_the_record() {
    let rig = Rig::new(Opts::default());
    run(&rig, open_wall("wall-1")).await.unwrap();
    let profile = rig.data.join("fleetdeck/wall-wall-1");
    std::fs::write(profile.join("Cookies"), b"x").unwrap();
    // The window ignores the close request.
    rig.flag("ignore_close", true);
    let e = run(
        &rig,
        A::WallClose {
            display_id: "wall-1".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        act_err(e),
        (
            "failed",
            "The wall window did not close; its profile stays. Try again.".into()
        )
    );
    assert!(profile.join("Cookies").exists(), "the profile stays");
    let walls = rig.desktop.info(&on).await.unwrap().walls;
    assert_eq!(walls.len(), 1, "the record stays");
}

#[tokio::test]
async fn a_wall_open_waits_for_a_close_and_its_profile_removal_of_the_same_display() {
    let rig = Rig::new(Opts::default());
    run(&rig, open_wall("wall-1")).await.unwrap();
    let profile = rig.data.join("fleetdeck/wall-wall-1");
    std::fs::write(profile.join("Cookies"), b"x").unwrap();
    // The close request is held: the close waits for the window.
    rig.flag("hold_close", true);
    let desktop = std::sync::Arc::clone(&rig.desktop);
    let close = tokio::spawn(async move {
        desktop
            .act(
                &on,
                A::WallClose {
                    display_id: "wall-1".into(),
                },
            )
            .await
    });
    eventually("the close request", || close_requests(&rig) == 1).await;
    let desktop = std::sync::Arc::clone(&rig.desktop);
    let open = tokio::spawn(async move { desktop.act(&on, open_wall("wall-1")).await });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(!open.is_finished() && !close.is_finished());
    assert_eq!(rig.calls_of("chromium").len(), 1, "no launch while closing");
    assert!(profile.join("Cookies").exists());
    rig.flag("hold_close", false);
    close.await.unwrap().unwrap();
    open.await.unwrap().unwrap();
    assert_eq!(rig.calls_of("chromium").len(), 2);
    assert!(!profile.join("Cookies").exists(), "the old profile is gone");
}

#[tokio::test]
async fn a_reload_with_an_invalid_url_leaves_the_window_the_record_and_the_profile() {
    let rig = Rig::new(Opts::default());
    run(&rig, open_wall("wall-1")).await.unwrap();
    let profile = rig.data.join("fleetdeck/wall-wall-1");
    std::fs::write(profile.join("Cookies"), b"x").unwrap();
    let e = run(
        &rig,
        A::WallReload {
            display_id: "wall-1".into(),
            url: "http://exa mple.test/".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "invalid_params");
    assert_eq!(close_requests(&rig), 0, "no close request");
    assert_eq!(rig.calls_of("chromium").len(), 1);
    assert!(profile.join("Cookies").exists());
    assert_eq!(rig.desktop.info(&on).await.unwrap().walls.len(), 1);
}
