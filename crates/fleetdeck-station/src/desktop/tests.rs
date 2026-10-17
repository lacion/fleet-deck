//! Action tests with a fake desktop (R13 P3a): the exact argv of each
//! action, the gates and the window records.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_proto::desktop::{DesktopAction as A, DesktopKind, Urgency};

use super::authority::Admitted;
use super::fake::{Opts, Rig, call};
use super::{Fail, Gate};

pub(super) fn on() -> bool {
    true
}

pub(super) fn act_err(e: Fail) -> (&'static str, String) {
    (e.code, e.message)
}

pub(super) async fn run(rig: &Rig, action: A) -> Result<Option<String>, Fail> {
    rig.desktop
        .act(&on, action, Admitted::open())
        .await
        .map(|r| r.address)
}

pub(super) async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timeout: {what}");
}

pub(super) fn url() -> String {
    "http://127.0.0.1:3000/#/display-pair".to_owned()
}

#[tokio::test]
async fn info_of_an_omarchy_desktop() {
    let rig = Rig::new(Opts::default());
    let gate: Gate<'_> = &on;
    let info = rig.desktop.info(gate).await.unwrap();
    assert_eq!(info.kind, DesktopKind::Omarchy);
    assert_eq!(info.omarchy_version.as_deref(), Some("3.1.0"));
    assert_eq!(info.monitors.len(), 2);
    let m = &info.monitors[0];
    assert_eq!((m.name.as_str(), m.width, m.height), ("DP-1", 3440, 1440));
    assert_eq!((m.scale_pct, m.refresh_hz), (125, 144));
    assert!(m.focused);
    assert_eq!(m.dpms_on, Some(true));
    assert_eq!(info.monitors[1].dpms_on, Some(false));
    assert_eq!(info.locked, Some(false));
    assert_eq!(info.dnd, Some(false));
    assert_eq!(info.idle_inhibited, Some(false));
    assert!(info.walls.is_empty());
    assert!(info.allowed.desktop && !info.allowed.capture && !info.allowed.input);
    assert!(info.tools.chromium && info.tools.notify_send);
    assert!(!info.tools.wtype && !info.tools.grim && !info.tools.recorder && !info.tools.pointer);
    // The live parts follow the gate; the rest is cached.
    let off = || false;
    let info = rig.desktop.info(&off).await.unwrap();
    assert!(!info.allowed.desktop);
}

#[tokio::test]
async fn the_state_flags_are_read_and_unknown_is_none() {
    let rig = Rig::new(Opts::default());
    rig.flag("hyprlock", true);
    rig.flag("dnd", true);
    rig.flag("no_hypridle", true);
    let info = rig.desktop.info(&on).await.unwrap();
    assert_eq!(
        (info.locked, info.dnd, info.idle_inhibited),
        (Some(true), Some(true), Some(true))
    );
    let rig = Rig::without(Opts::default(), &["pgrep", "makoctl"]);
    let info = rig.desktop.info(&on).await.unwrap();
    assert_eq!(
        (info.locked, info.dnd, info.idle_inhibited),
        (None, None, None)
    );
}

#[tokio::test]
async fn a_hyprland_only_desktop_has_no_idle_state() {
    let rig = Rig::new(Opts {
        omarchy: false,
        ..Opts::default()
    });
    let info = rig.desktop.info(&on).await.unwrap();
    assert_eq!(info.kind, DesktopKind::Hyprland);
    assert_eq!(info.omarchy_version, None);
    assert_eq!(info.idle_inhibited, None);
    let e = run(&rig, A::ScreenIdleInhibit { on: true })
        .await
        .unwrap_err();
    assert_eq!(e.code, "unsupported");
    assert!(rig.calls().is_empty());
}

#[tokio::test]
async fn without_a_desktop_everything_is_unsupported() {
    let rig = Rig::new(Opts {
        instance: false,
        ..Opts::default()
    });
    let e = rig.desktop.info(&on).await.unwrap_err();
    assert_eq!(
        act_err(e),
        ("unsupported", "This station has no desktop.".into())
    );
    let e = run(&rig, A::ScreenLock).await.unwrap_err();
    assert_eq!(e.code, "unsupported");
    assert!(rig.calls().is_empty());
}

#[tokio::test]
async fn info_is_cached_for_ten_seconds_and_an_action_refreshes_it() {
    let rig = Rig::new(Opts::default());
    rig.desktop.info(&on).await.unwrap();
    let monitor_reads = |rig: &Rig| {
        rig.reads()
            .iter()
            .filter(|c| c.program == "hyprctl" && c.args == ["-j", "monitors"])
            .count()
    };
    assert_eq!(monitor_reads(&rig), 1);
    rig.desktop.info(&on).await.unwrap();
    assert_eq!(monitor_reads(&rig), 1);
    run(&rig, A::ScreenDnd { on: true }).await.unwrap();
    assert_eq!(monitor_reads(&rig), 2);
}

#[tokio::test]
async fn screen_power_runs_dpms_with_an_exact_monitor() {
    let rig = Rig::new(Opts::default());
    run(
        &rig,
        A::ScreenPower {
            monitor: Some("DP-1".into()),
            on: false,
        },
    )
    .await
    .unwrap();
    run(
        &rig,
        A::ScreenPower {
            monitor: None,
            on: true,
        },
    )
    .await
    .unwrap();
    let dispatch = rig.calls_of("hyprctl");
    assert_eq!(
        dispatch[0],
        call("hyprctl", &["dispatch", "dpms", "off", "DP-1"])
    );
    assert_eq!(dispatch[1], call("hyprctl", &["dispatch", "dpms", "on"]));
    let n = rig.calls().len();
    for bad in ["dp-1", "DP-1;exit", "$(id)", "-x", ""] {
        let e = run(
            &rig,
            A::ScreenPower {
                monitor: Some(bad.into()),
                on: true,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "invalid_params", "{bad}");
    }
    assert_eq!(rig.calls().len(), n, "a refused monitor runs nothing");
}

#[tokio::test]
async fn screen_lock_uses_the_omarchy_script_or_loginctl() {
    let rig = Rig::new(Opts::default());
    run(&rig, A::ScreenLock).await.unwrap();
    assert_eq!(rig.calls(), vec![call("omarchy-lock-screen", &[])]);
    let rig = Rig::new(Opts {
        omarchy: false,
        ..Opts::default()
    });
    run(&rig, A::ScreenLock).await.unwrap();
    assert_eq!(rig.calls(), vec![call("loginctl", &["lock-session"])]);
}

#[tokio::test]
async fn screen_dnd_sets_the_mode() {
    let rig = Rig::new(Opts::default());
    run(&rig, A::ScreenDnd { on: true }).await.unwrap();
    run(&rig, A::ScreenDnd { on: false }).await.unwrap();
    assert_eq!(
        rig.calls(),
        vec![
            call("makoctl", &["mode", "-a", "do-not-disturb"]),
            call("makoctl", &["mode", "-r", "do-not-disturb"]),
        ]
    );
    let rig = Rig::without(Opts::default(), &["makoctl"]);
    let e = run(&rig, A::ScreenDnd { on: true }).await.unwrap_err();
    assert_eq!(
        act_err(e),
        (
            "unsupported",
            "`makoctl` is not installed on this desktop".into()
        )
    );
}

#[tokio::test]
async fn idle_inhibit_toggles_only_when_the_state_differs() {
    let rig = Rig::new(Opts::default());
    // `hypridle` runs: idle is not inhibited.
    run(&rig, A::ScreenIdleInhibit { on: false }).await.unwrap();
    assert!(rig.calls().is_empty());
    run(&rig, A::ScreenIdleInhibit { on: true }).await.unwrap();
    assert_eq!(rig.calls(), vec![call("omarchy-toggle-idle", &[])]);
    let rig = Rig::without(Opts::default(), &["pgrep"]);
    let e = run(&rig, A::ScreenIdleInhibit { on: true })
        .await
        .unwrap_err();
    assert_eq!(e.code, "failed");
    assert!(rig.calls().is_empty());
}

#[tokio::test]
async fn workspace_show_focuses_the_monitor_then_the_workspace() {
    let rig = Rig::new(Opts::default());
    run(
        &rig,
        A::WorkspaceShow {
            monitor: "HDMI-A-1".into(),
            workspace: 7,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        rig.calls(),
        vec![
            call("hyprctl", &["dispatch", "focusmonitor", "HDMI-A-1"]),
            call("hyprctl", &["dispatch", "workspace", "7"]),
        ]
    );
    let n = rig.calls().len();
    for ws in [0, 100] {
        let e = run(
            &rig,
            A::WorkspaceShow {
                monitor: "DP-1".into(),
                workspace: ws,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "invalid_params");
    }
    let e = run(
        &rig,
        A::WorkspaceShow {
            monitor: "nope".into(),
            workspace: 1,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "invalid_params");
    assert_eq!(rig.calls().len(), n);
}

#[tokio::test]
async fn notify_puts_the_option_terminator_before_the_text() {
    let rig = Rig::new(Opts::default());
    let send = |title: &str, body: &str, urgency| A::Notify {
        title: title.into(),
        body: body.into(),
        urgency,
    };
    run(
        &rig,
        send("Build done", "All <b>green</b> &amp; fast", Urgency::Normal),
    )
    .await
    .unwrap();
    run(&rig, send("-rf", "--urgency=critical", Urgency::Low))
        .await
        .unwrap();
    run(&rig, send("$(id); `x`", "a\nb\u{7}", Urgency::Critical))
        .await
        .unwrap();
    let calls = rig.calls_of("notify-send");
    assert_eq!(
        calls[0],
        call(
            "notify-send",
            &[
                "--app-name=Fleet Deck",
                "--urgency=normal",
                "--",
                "Build done",
                "All green & fast"
            ]
        )
    );
    // A title that starts with `-` stays after `--`.
    assert_eq!(
        calls[1],
        call(
            "notify-send",
            &[
                "--app-name=Fleet Deck",
                "--urgency=low",
                "--",
                "-rf",
                "--urgency=critical"
            ]
        )
    );
    assert_eq!(calls[2].args[3..], ["$(id); `x`", "ab"]);
    assert_eq!(calls[2].args[1], "--urgency=critical");
    let e = run(&rig, send("<i></i>", "x", Urgency::Low))
        .await
        .unwrap_err();
    assert_eq!(e.code, "invalid_params");
    assert_eq!(rig.calls_of("notify-send").len(), 3);
}

#[tokio::test]
async fn a_failed_program_gives_a_redacted_message() {
    let rig = Rig::new(Opts::default());
    rig.fail("notify-send");
    let e = run(
        &rig,
        A::Notify {
            title: "SECRET-TITLE".into(),
            body: "SECRET-BODY".into(),
            urgency: Urgency::Low,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        act_err(e.clone()),
        ("failed", "`notify-send` failed with exit code 2".into())
    );
    assert!(!e.message.contains("SECRET"));
}

#[tokio::test]
async fn allow_desktop_off_refuses_before_any_program_runs() {
    let rig = Rig::new(Opts::default());
    let off = || false;
    let e = rig
        .desktop
        .act(&off, A::ScreenLock, Admitted::open())
        .await
        .unwrap_err();
    assert_eq!(
        act_err(e),
        (
            "forbidden",
            "Desktop control is off on this station (allow_desktop = false).".into()
        )
    );
    assert!(rig.calls().is_empty());
    // `desktop.info` still works with the setting off.
    assert!(rig.desktop.info(&off).await.is_ok());
}

#[tokio::test]
async fn the_policy_is_read_again_before_every_program_run() {
    let rig = Rig::new(Opts::default());
    // The gate is true for the intake check, the monitor list and the first
    // dispatch, then the owner turns the setting off.
    let seen = AtomicUsize::new(0);
    let gate = || seen.fetch_add(1, Ordering::SeqCst) < 3;
    let e = rig
        .desktop
        .act(
            &gate,
            A::WorkspaceShow {
                monitor: "DP-1".into(),
                workspace: 2,
            },
            Admitted::open(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "forbidden");
    assert_eq!(
        rig.calls(),
        vec![call("hyprctl", &["dispatch", "focusmonitor", "DP-1"])]
    );
}

#[tokio::test]
async fn a_url_with_a_space_is_refused_before_any_program_runs() {
    let rig = Rig::new(Opts::default());
    for bad in [
        "http://example.com/a b",
        "ftp://example.com",
        "http://u@example.com",
        "",
    ] {
        let e = run(
            &rig,
            A::BrowserOpen {
                url: bad.into(),
                monitor: Some("DP-1".into()),
                session: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "invalid_params", "{bad}");
        let e = run(
            &rig,
            A::WallOpen {
                display_id: "w".into(),
                monitor: "DP-1".into(),
                workspace: None,
                url: bad.into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "invalid_params", "{bad}");
    }
    assert!(rig.calls().is_empty());
    assert!(rig.reads().iter().all(|c| c.args != ["-j", "monitors"]));
}
