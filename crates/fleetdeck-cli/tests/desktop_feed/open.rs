//! `fleetdeck desktop open` (P4 §3.3), with the rig of `desktop_feed.rs`.

use super::*;

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn opened(rig: &Rig) -> Vec<Vec<String>> {
    wait_until("xdg-open", || !rig.calls("xdg-open").is_empty());
    rig.calls("xdg-open")
}

#[test]
fn desktop_open_without_an_argument_opens_the_board() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![answer(1, "up", 0, &plain("t1"))]);
    let out = rig.run(&["desktop", "open"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert_eq!(opened(&rig), [[format!("{BOARD}/")]]);
    assert_eq!(rig.requests(), [r#"{"op":"fleet_summary","after":0}"#]);
}

#[test]
fn desktop_open_a_session_and_a_request_go_into_the_fragment() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![
        answer(1, "up", 0, &plain("t1")),
        answer(1, "up", 0, &plain("t1")),
    ]);
    let out = rig.run(&["desktop", "open", "st1/s2"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let out = rig.run(&["desktop", "open", "st1/s2", "--request", "r3"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    wait_until("two xdg-open calls", || rig.calls("xdg-open").len() >= 2);
    assert_eq!(
        rig.calls("xdg-open"),
        [
            [format!("{BOARD}/#/s/st1/s2")],
            [format!("{BOARD}/#/s/st1/s2?request=r3")]
        ]
    );
}

#[test]
fn desktop_open_refuses_a_bad_argument() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    for args in [
        vec!["desktop", "open", "st1"],
        vec!["desktop", "open", "st1/s 1"],
        vec!["desktop", "open", "--request", "r1"],
        vec!["desktop", "open", "st1/s1", "--request", "r?1"],
    ] {
        let out = rig.run(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            stderr_of(&out),
            "fleetdeck: Use <station_id>/<session>.\n",
            "{args:?}"
        );
    }
    assert!(rig.requests().is_empty(), "no call for a bad argument");
    assert!(rig.calls("xdg-open").is_empty());
}

#[test]
fn desktop_open_without_a_summary_or_with_the_setting_off_says_to_run_the_feed() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![answer(1, "down", 0, &Value::Null), off()]);
    for _ in 0..2 {
        let out = rig.run(&["desktop", "open"]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            stderr_of(&out),
            "fleetdeck: No board address is known. Run the feed first.\n"
        );
    }
    assert!(rig.calls("xdg-open").is_empty());
}

#[test]
fn desktop_open_gives_up_after_5_seconds_when_the_station_holds_the_call() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![]);
    let started = Instant::now();
    let out = rig.run(&["desktop", "open"]);
    let took = started.elapsed();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: No board address is known. Run the feed first.\n"
    );
    assert!(took >= Duration::from_millis(4500), "{took:?}");
    assert!(took < Duration::from_secs(12), "{took:?}");
}

#[test]
fn desktop_open_a_null_board_has_its_own_text() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![answer(
        1,
        "up",
        0,
        &summary("t1", vec![], Value::Null),
    )]);
    let out = rig.run(&["desktop", "open"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: The deck did not tell its board address.\n"
    );
}

#[test]
fn desktop_open_without_xdg_open_says_so() {
    let rig = Rig::new();
    rig.station(vec![answer(1, "up", 0, &plain("t1"))]);
    let out = rig.run(&["desktop", "open"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr_of(&out), "fleetdeck: xdg-open is not installed.\n");
}

#[test]
fn desktop_open_without_a_station_fails_with_exit_1() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    let out = rig.run(&["desktop", "open"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).starts_with("fleetdeck: "),
        "{}",
        stderr_of(&out)
    );
    assert!(rig.calls("xdg-open").is_empty());
}
