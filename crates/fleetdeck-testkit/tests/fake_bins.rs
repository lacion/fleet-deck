//! The fake agent binaries start, report a version, and follow the skeleton
//! scenarios. `FakeAgent::locate` finds the same binaries that Cargo built.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::fake::SCENARIO_ENV;

fn built(agent: FakeAgent) -> PathBuf {
    PathBuf::from(match agent {
        FakeAgent::Claude => env!("CARGO_BIN_EXE_fake-claude"),
        FakeAgent::Codex => env!("CARGO_BIN_EXE_fake-codex"),
    })
}

#[test]
fn locate_finds_the_binaries_that_cargo_built() {
    for agent in FakeAgent::ALL {
        let found = agent.locate().unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            built(agent).canonicalize().unwrap()
        );
    }
}

#[test]
fn version_lines_look_like_the_real_clis() {
    for agent in FakeAgent::ALL {
        let out = Command::new(built(agent))
            .arg("--version")
            .output()
            .unwrap();
        assert!(out.status.success());
        let line = String::from_utf8(out.stdout).unwrap();
        assert_eq!(line.trim(), agent.version_line());
    }
}

#[test]
fn exit_scenarios_set_the_exit_code() {
    for agent in FakeAgent::ALL {
        let status = agent.command("exit:7").unwrap().status().unwrap();
        assert_eq!(status.code(), Some(7));
    }
}

#[test]
fn hang_waits_for_stdin_to_close() {
    let mut child = FakeAgent::Claude
        .command("hang")
        .unwrap()
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(
        child.try_wait().unwrap().is_none(),
        "exited before stdin closed"
    );
    child.stdin.take().unwrap().write_all(b"bye").unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn unknown_scenarios_fail() {
    let out = Command::new(built(FakeAgent::Codex))
        .env(SCENARIO_ENV, "no-such-scenario")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("unknown scenario `no-such-scenario`"), "{err}");
}
