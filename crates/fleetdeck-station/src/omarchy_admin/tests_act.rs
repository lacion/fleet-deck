//! Tests of the `omarchy.act` mapping: `integration_outdated`, the exit
//! codes, the cleaned message, the limits and the unavailable service.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::omarchy_admin::{ActOutcome, OmarchyAction};
use serde_json::Value;

use super::Holder;
use super::child::clean_message;
use super::tests::{Rig, STATUS_OUTDATED, act, result, status, until, wire_error};

const GUARDED: [&str; 4] = [
    "screensaver_on",
    "screensaver_off",
    "screensaver_rebaseline",
    "uninstall",
];

#[tokio::test]
async fn omarchy_an_outdated_integration_refuses_before_any_action_child() {
    let rig = Rig::new("exit 0");
    rig.set_status(STATUS_OUTDATED);
    for action in GUARDED {
        let got = result(act(&rig.admin, action, true).await);
        assert_eq!(got.outcome, ActOutcome::Failed, "{action}");
        assert_eq!(
            got.code.as_deref(),
            Some("integration_outdated"),
            "{action}"
        );
        assert_eq!(got.message, None, "{action}");
        assert!(got.status.install.unwrap().outdated);
        assert_eq!(got.status.running, None);
    }
    assert!(rig.action_runs().is_empty(), "{:?}", rig.action_runs());
    // Install stays allowed.
    let got = result(act(&rig.admin, "install", true).await);
    assert_eq!(got.outcome, ActOutcome::Done);
    assert_eq!(rig.action_runs().len(), 1);
}

#[tokio::test]
async fn omarchy_an_unreadable_status_does_not_block_a_guarded_action() {
    let rig = Rig::new("exit 0");
    rig.set_status("not json");
    let got = result(act(&rig.admin, "uninstall", true).await);
    assert_eq!(got.outcome, ActOutcome::Done);
    assert_eq!((got.status.install, got.status.screensaver), (None, None));
    assert_eq!(rig.action_runs().len(), 1);
}

#[tokio::test]
async fn omarchy_exit_codes_map_to_the_fixed_codes() {
    let table = [
        ("install", 1, "install_refused"),
        ("uninstall", 1, "uninstall_incomplete"),
        ("screensaver_on", 1, "screensaver_refused"),
        ("screensaver_off", 1, "screensaver_refused"),
        ("screensaver_rebaseline", 1, "screensaver_refused"),
    ];
    for (action, exit, want) in table {
        let rig = Rig::new(&format!("echo 'a conflict' >&2; exit {exit}"));
        let got = result(act(&rig.admin, action, true).await);
        assert_eq!(got.outcome, ActOutcome::Failed, "{action}");
        assert_eq!(got.code.as_deref(), Some(want), "{action}");
        assert_eq!(got.message.as_deref(), Some("a conflict\n"), "{action}");
    }
    for action in ["install", "uninstall", "screensaver_on"] {
        for body in [
            "echo secret >&2; exit 2",
            "echo secret >&2; exit 127",
            "kill -9 $$",
        ] {
            let rig = Rig::new(body);
            let got = result(act(&rig.admin, action, true).await);
            assert_eq!(got.outcome, ActOutcome::Failed, "{action} {body}");
            assert_eq!(got.code.as_deref(), Some("failed"), "{action} {body}");
            assert_eq!(got.message, None, "{action} {body}");
        }
    }
}

#[tokio::test]
async fn omarchy_a_program_that_does_not_start_is_failed_without_a_message() {
    let rig = Rig::with("exit 0", |cfg| {
        cfg.program = "/nonexistent/fleetdeck".into()
    });
    let got = result(act(&rig.admin, "install", true).await);
    assert_eq!(got.outcome, ActOutcome::Failed);
    assert_eq!(got.code.as_deref(), Some("failed"));
    assert_eq!(got.message, None);
    // The status child did not start either: no install facts.
    assert!(got.status.omarchy.present);
    assert_eq!((got.status.install, got.status.screensaver), (None, None));
}

#[tokio::test]
async fn omarchy_the_message_loses_control_characters_and_stops_at_2000_chars() {
    let body = r#"printf 'one\033[31mred\r\ttab\001\nline two\n' >&2
head -c 3000 /dev/zero | tr '\0' 'x' >&2
exit 1"#;
    let rig = Rig::new(body);
    let got = result(act(&rig.admin, "install", true).await);
    // 3,000 `x` fit in the 16 KiB stderr cap, so the message is cut at 2,000.
    let message = got.message.unwrap();
    assert_eq!(message.chars().count(), 2000);
    assert!(message.starts_with("one[31mredtab"), "{message:?}");
    assert!(message.contains("\nline two\n"));
    assert!(message.chars().all(|c| c == '\n' || !c.is_control()));
}

#[test]
fn omarchy_clean_message_rules() {
    assert_eq!(clean_message(b""), None);
    assert_eq!(clean_message(b"\x1b\x07\r\t"), None);
    assert_eq!(clean_message(b"a\x1bb\nc\rd").as_deref(), Some("ab\ncd"));
    // Invalid UTF-8 becomes the replacement character, which is kept.
    assert_eq!(clean_message(b"a\xffb").as_deref(), Some("a\u{fffd}b"));
    // The limit counts characters after the cleaning.
    let long = "é".repeat(2500);
    assert_eq!(
        clean_message(long.as_bytes()).unwrap().chars().count(),
        2000
    );
    let with_controls = format!("{}{}", "\x01".repeat(5000), "z".repeat(10));
    assert_eq!(
        clean_message(with_controls.as_bytes()).as_deref(),
        Some("zzzzzzzzzz")
    );
}

#[tokio::test]
async fn omarchy_a_child_past_its_limit_is_killed_with_its_group() {
    let body = r#"sleep 30 &
echo $! > "$REC/bgpid"
wait"#;
    let rig = Rig::with(body, |cfg| cfg.limits.install = Duration::from_millis(400));
    let started = std::time::Instant::now();
    let got = result(act(&rig.admin, "install", true).await);
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(got.outcome, ActOutcome::Failed);
    assert_eq!(got.code.as_deref(), Some("timeout"));
    assert_eq!(got.message, None);
    let pid: i32 = std::fs::read_to_string(rig.path("rec/bgpid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    until(|| {
        matches!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        )
    })
    .await;
    assert_eq!(rig.admin.running(), None);
}

#[tokio::test]
async fn omarchy_more_than_the_output_caps_is_output_too_large() {
    // 64 KiB of stdout is allowed; one byte more is not.
    let rig = Rig::new("head -c 65536 /dev/zero | tr '\\0' x");
    let got = result(act(&rig.admin, "install", true).await);
    assert_eq!(got.outcome, ActOutcome::Done);
    for (body, what) in [
        ("head -c 65537 /dev/zero | tr '\\0' x", "stdout"),
        ("head -c 16385 /dev/zero | tr '\\0' x >&2", "stderr"),
    ] {
        let rig = Rig::new(body);
        let got = result(act(&rig.admin, "install", true).await);
        assert_eq!(got.outcome, ActOutcome::Failed, "{what}");
        assert_eq!(got.code.as_deref(), Some("output_too_large"), "{what}");
        assert_eq!(got.message, None, "{what}");
    }
}

#[tokio::test]
async fn omarchy_status_json_is_read_whatever_the_exit_code() {
    let rig = Rig::new("exit 0");
    std::fs::write(rig.path("rec/status.exit"), "1").unwrap();
    let got = status(rig.admin.status_call(Value::Null).await);
    assert!(got.install.unwrap().installed);
}

#[tokio::test]
async fn omarchy_a_status_json_with_missing_keys_gives_no_install_facts() {
    let rig = Rig::new("exit 0");
    std::fs::write(
        rig.path("rec/status.json"),
        "{\"installed\":true,\"files\":[]}",
    )
    .unwrap();
    // A parse failure (missing keys) gives no install facts.
    let got = status(rig.admin.status_call(Value::Null).await);
    assert_eq!((got.install, got.screensaver), (None, None));
    assert!(got.omarchy.present);
}

#[tokio::test]
async fn omarchy_without_a_service_both_methods_are_unavailable() {
    for method in ["omarchy.status", "omarchy.act"] {
        let (code, text) = wire_error(super::call(method, Value::Null).await);
        assert_eq!(code, "unavailable", "{method}");
        assert_eq!(text, "Omarchy management is not available on this station.");
    }
}

#[tokio::test]
async fn omarchy_a_second_action_is_busy_and_status_reports_the_running_one() {
    let rig = Rig::new(r#"case "$*" in *install*) sleep 1;; esac"#);
    let admin = Arc::clone(&rig.admin);
    let first = tokio::spawn(async move { act(&admin, "install", true).await });
    until(|| rig.admin.running() == Some(OmarchyAction::Install)).await;
    let (code, text) = wire_error(act(&rig.admin, "uninstall", true).await);
    assert_eq!(code, "busy");
    assert_eq!(text, "Another Omarchy action is running on this station.");
    let during = status(rig.admin.status_call(Value::Null).await);
    assert_eq!(during.running, Some(OmarchyAction::Install));
    let done = result(first.await.unwrap());
    assert_eq!(done.outcome, ActOutcome::Done);
    assert_eq!(rig.admin.running(), None);
    // The slot is free again.
    assert_eq!(
        result(act(&rig.admin, "install", true).await).outcome,
        ActOutcome::Done
    );
}

#[tokio::test]
async fn omarchy_a_tick_blocks_an_action_but_status_shows_no_running_action() {
    let rig = Rig::new("exit 0");
    let permit = rig.admin.try_hold(Holder::Tick).unwrap();
    let (code, _) = wire_error(act(&rig.admin, "install", true).await);
    assert_eq!(code, "busy");
    let got = status(rig.admin.status_call(Value::Null).await);
    assert_eq!(got.running, None);
    drop(permit);
    assert_eq!(
        result(act(&rig.admin, "install", true).await).outcome,
        ActOutcome::Done
    );
}
