use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::{Duration, Instant};

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::launch_dir::LaunchDir;
use fleetdeck_proto::command::RejectCode;
use fleetdeck_proto::shell::{ShellChange, ShellState};
use rustix::process::Signal;
use tokio::sync::mpsc::Receiver;

use super::{Launch, ShellConfig, Shells, handover};
use crate::manager::policy::Policy;
use crate::store::OfflineApprovalPolicy;

pub(super) fn policy(root: &std::path::Path) -> Policy {
    policy_with(root, true, 2)
}

/// A policy with `allow_shell` and `max_shells` set.
pub(super) fn policy_with(root: &std::path::Path, allow: bool, max_shells: u32) -> Policy {
    Policy::new(
        vec![root.to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )
    .with_shell(allow, max_shells)
}

pub(super) fn env() -> ChildEnv {
    ChildEnv::scrubbed(
        [
            ("PATH", "/usr/bin:/bin"),
            ("KEEP_ME", "kept"),
            ("ANTHROPIC_API_KEY", "secret-a"),
            ("FLEETDECK_DATA_DIR", "secret-b"),
            ("CLAUDECODE", "1"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    )
}

fn shells() -> Shells {
    let config = ShellConfig {
        program: Some(("/bin/sh".into(), Vec::new())),
        instances: BTreeMap::new(),
    };
    Shells::with_env(config, env())
}

pub(super) fn open(id: &str, cwd: &std::path::Path) -> ShellChange {
    ShellChange::Open {
        shell_id: id.to_owned(),
        cwd: cwd.to_string_lossy().into_owned(),
        cols: 80,
        rows: 24,
    }
}

/// Reads `rx` (after `seen`) until the text holds `needle`.
pub(super) fn read_until(seen: &mut String, rx: &mut Receiver<Vec<u8>>, needle: &str) {
    let t0 = Instant::now();
    while !seen.contains(needle) {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "no `{needle}` in {seen:?}"
        );
        match rx.try_recv() {
            Ok(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

#[test]
fn shell_policy_checks_allow_shell_the_roots_and_the_shell_cap() {
    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    let p = policy(root.path());

    let off = shells();
    let p_off = policy_with(root.path(), false, 2);
    let e = off
        .apply(&open("a", root.path()), &p_off, 1)
        .expect_err("off");
    assert_eq!(e.code, RejectCode::Unsupported);

    let on = shells();
    let e = on
        .apply(&open("a", outside.path()), &p, 1)
        .expect_err("outside");
    assert_eq!(e.code, RejectCode::InvalidCommand);
    assert!(e.message.contains("outside the allowed roots"), "{e:?}");
    let e = on
        .apply(&open("../x", root.path()), &p, 1)
        .expect_err("bad id");
    assert_eq!(e.code, RejectCode::InvalidCommand);

    on.apply(&open("a", root.path()), &p, 1).expect("first");
    let e = on
        .apply(&open("a", root.path()), &p, 2)
        .expect_err("same id");
    assert_eq!(e.code, RejectCode::InvalidState);
    on.apply(&open("b", root.path()), &p, 2).expect("second");
    let e = on.apply(&open("c", root.path()), &p, 3).expect_err("cap");
    assert_eq!(e.code, RejectCode::InvalidState);
    assert!(e.message.contains("max_shells"), "{e:?}");
    assert_eq!(on.list().len(), 2);

    let close = |id: &str| ShellChange::Close {
        shell_id: id.to_owned(),
    };
    on.apply(&close("a"), &p, 4).expect("close");
    let e = on.apply(&close("a"), &p, 5).expect_err("closed twice");
    assert_eq!(e.code, RejectCode::InvalidState);
    on.apply(&open("c", root.path()), &p, 6)
        .expect("a slot is free again");
    on.close_all();
    assert!(on.list().is_empty());
}

/// The shell's real working directory is the checked folder (B9).
#[cfg(target_os = "linux")]
#[test]
fn a_shell_starts_in_the_checked_folder() {
    let root = tempfile::tempdir().expect("root");
    let proj = root.path().join("proj");
    std::fs::create_dir(&proj).expect("proj");
    let p = policy(root.path());
    let s = shells();
    s.apply(&open("real", &proj), &p, 1).expect("open");
    let (_, mut rx) = s.attach("real").expect("attach");
    assert!(s.input("real", b"echo \"HERE=$(pwd -P)\"; echo END-OF-$((40+2))\n"));
    let mut seen = String::new();
    read_until(&mut seen, &mut rx, "END-OF-42");
    let canonical = proj.canonicalize().expect("canonical");
    assert!(
        seen.contains(&format!("HERE={}", canonical.display())),
        "{seen}"
    );
    s.close_all();
}

#[test]
fn a_shell_runs_in_its_cwd_with_the_scrubbed_environment() {
    let root = tempfile::tempdir().expect("root");
    let p = policy(root.path());
    let s = shells();
    s.apply(&open("env", root.path()), &p, 1).expect("open");
    let (_, mut rx) = s.attach("env").expect("attach");
    assert!(s.input("env", b"pwd; env; echo END-OF-$((40+2))\n"));
    let mut seen = String::new();
    read_until(&mut seen, &mut rx, "END-OF-42");
    let canonical = root.path().canonicalize().expect("canonical");
    assert!(seen.contains(&*canonical.to_string_lossy()), "{seen}");
    assert!(seen.contains("KEEP_ME=kept"), "{seen}");
    assert!(seen.contains("TERM=xterm-256color"), "{seen}");
    for secret in [
        "ANTHROPIC_API_KEY",
        "FLEETDECK_DATA_DIR",
        "CLAUDECODE",
        "secret-a",
    ] {
        assert!(!seen.contains(secret), "{secret} leaked: {seen}");
    }
    s.close_all();
}

#[test]
fn a_reattached_board_gets_the_scrollback_and_the_shell_survives_a_detach() {
    let root = tempfile::tempdir().expect("root");
    let p = policy(root.path());
    let s = shells();
    s.apply(&open("sh", root.path()), &p, 1).expect("open");
    let resize = ShellChange::Resize {
        shell_id: "sh".to_owned(),
        cols: 120,
        rows: 40,
    };
    s.apply(&resize, &p, 2).expect("resize");
    assert_eq!((s.list()[0].cols, s.list()[0].rows), (120, 40));

    let (_, mut first) = s.attach("sh").expect("attach");
    assert!(s.input("sh", b"echo one-$((40+2))\n"));
    let mut seen = String::new();
    read_until(&mut seen, &mut first, "one-42");
    // The link (or the deck) goes away: the stream's queue ends.
    drop(first);
    assert!(s.input("sh", b"echo two-$((40+3))\n"));
    let t0 = Instant::now();
    loop {
        let (backlog, _) = s.attach("sh").expect("reattach");
        let text = String::from_utf8_lossy(&backlog).into_owned();
        if text.contains("two-43") {
            assert!(text.contains("one-42"), "{text}");
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "{text}");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(s.list()[0].state, ShellState::Running);

    // The shell ends by itself: the state says so and the queue ends.
    let (_, mut live) = s.attach("sh").expect("attach");
    assert!(s.input("sh", b"exit 3\n"));
    let t0 = Instant::now();
    while s.list()[0].state == ShellState::Running {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "the shell did not end"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(s.list()[0].exit_code, Some(3));
    while let Ok(_) | Err(tokio::sync::mpsc::error::TryRecvError::Empty) = live.try_recv() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "the queue did not end"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_handover_is_refused_when_shells_are_not_allowed() {
    let root = tempfile::tempdir().expect("root");
    let p = policy_with(root.path(), false, 2);
    let s = shells();
    let handover = ShellChange::Handover {
        shell_id: "tui".to_owned(),
        session_id: "s1".into(),
        cols: 80,
        rows: 24,
    };
    let e = s.apply(&handover, &p, 1).expect_err("handover");
    assert_eq!(e.code, RejectCode::Unsupported);
    assert!(s.list().is_empty());
}

#[test]
fn a_handover_and_a_release_are_not_pty_effects_of_their_own() {
    let root = tempfile::tempdir().expect("root");
    let p = policy(root.path());
    let s = shells();
    let handover = ShellChange::Handover {
        shell_id: "tui".to_owned(),
        session_id: "s1".into(),
        cols: 80,
        rows: 24,
    };
    let release = ShellChange::Release {
        shell_id: "tui".to_owned(),
    };
    for change in [handover, release] {
        let e = s.apply(&change, &p, 1).expect_err("no PTY effect");
        assert_eq!(e.code, RejectCode::InvalidState);
    }
    assert!(s.list().is_empty());
}

#[test]
fn closing_an_exited_tui_sends_no_signal() {
    let root = tempfile::tempdir().expect("root");
    let s = shells();
    let launch = Launch {
        program: ("/bin/sh".into(), vec!["-c".into(), "exit 0".into()]),
        env: env(),
        session: Some("s1".into()),
        max_shells: 2,
    };
    let canonical = root.path().canonicalize().expect("canonical");
    let dir = LaunchDir::open(&canonical, std::path::Path::new("")).expect("the folder opens");
    let pid = s
        .spawn("tui", &dir, (80, 24), 1, launch, (5000, 8 << 20))
        .expect("spawn");
    assert!(pid.is_some());
    let shell = s.get("tui").expect("the shell is listed");
    let t0 = Instant::now();
    while shell.info.lock().expect("info").state != ShellState::Exited {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "the TUI did not exit"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // Its pid may be reused by now: nothing may be sent to it.
    assert!(!handover::signal_group(&shell, Signal::HUP));
    assert!(!handover::signal_group(&shell, Signal::KILL));
}

#[test]
fn a_new_terminal_keeps_the_scrollback_lines_of_the_policy() {
    use crate::manager::policy_hub::test_config;
    let root = tempfile::tempdir().expect("root");
    let extra = "allow_shell = true\n[limits]\nscrollback_lines = 500\n";
    let p = Policy::from_config(&test_config(root.path(), extra));
    assert_eq!(p.scrollback_limits().0, 500);
    let s = shells();
    s.apply(&open("sh", root.path()), &p, 1).expect("open");
    assert!(s.input("sh", b"seq 1 800; echo done-$((40+2))\n"));
    let t0 = Instant::now();
    loop {
        let (backlog, _) = s.attach("sh").expect("attach");
        let text = String::from_utf8_lossy(&backlog).into_owned();
        if text.contains("done-42") {
            let lines = backlog.iter().filter(|b| **b == b'\n').count();
            assert!(lines < 500, "{lines} lines");
            assert!(text.contains("\n800\r\n"), "the newest lines stay");
            assert!(!text.contains("\n3\r\n"), "the oldest lines are gone");
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "{text}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
