//! The binary runner with fake `grim` scripts (design §5.5): argv, binary
//! bytes, no fallback, the caps, the timeout with a child group, a full
//! stderr pipe, a nonzero exit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::capture::{code, text};

use super::{Launch, capture_png};
use crate::desktop::fake::{Grim, grim};
use crate::desktop::{Fail, code as desktop_code};

fn ok_room(_: u64) -> Result<(), Fail> {
    Ok(())
}

fn in_secs(secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(secs)
}

fn run(env: &ChildEnv, limit: u64, deadline: Instant) -> (Result<u64, Fail>, File) {
    let mut out = tempfile::tempfile().unwrap();
    let result = capture_png(
        env,
        &Launch::Direct,
        "DP-1",
        &mut out,
        limit,
        deadline,
        &ok_room,
    );
    (result, out)
}

fn contents(file: &mut File) -> Vec<u8> {
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

fn fail(result: Result<u64, Fail>) -> (&'static str, String) {
    let e = result.unwrap_err();
    (e.code, e.message)
}

/// True when the process is gone or a zombie.
fn gone(pid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| s.rsplit(')').next().map(|t| t.trim_start().chars().next()))
            .flatten()
            .is_some_and(|state| state != 'Z');
        if !alive {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_argv_is_the_six_grim_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let env = grim(dir.path(), &Grim::Bytes(b"x"));
    let (result, _) = run(&env, 100, in_secs(5));
    assert_eq!(result.unwrap(), 1);
    let log = std::fs::read_to_string(dir.path().join("grim.log")).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["-t", "png", "-s", "1", "-l", "6", "-o", "DP-1", "-"]
    );
}

#[test]
fn binary_bytes_round_trip_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0..=255_u8).chain((0..=255_u8).rev()).collect();
    let env = grim(dir.path(), &Grim::Bytes(&bytes));
    let (result, mut out) = run(&env, 1000, in_secs(5));
    assert_eq!(result.unwrap(), 512);
    assert_eq!(contents(&mut out), bytes);
}

#[test]
fn a_missing_grim_is_unsupported_and_nothing_runs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("bin")).unwrap();
    let mut env = ChildEnv::empty();
    env.set("PATH", dir.path().join("bin")).unwrap();
    let (result, _) = run(&env, 100, in_secs(5));
    assert_eq!(
        fail(result),
        (desktop_code::UNSUPPORTED, text::NO_GRIM.to_owned())
    );
    assert!(!dir.path().join("grim.log").exists());
    // A PATH with no `grim` at all gives the same answer.
    let (result, _) = run(&ChildEnv::empty(), 100, in_secs(5));
    assert_eq!(result.unwrap_err().message, text::NO_GRIM);
}

#[test]
fn a_bad_monitor_name_is_invalid_params_and_nothing_runs() {
    let dir = tempfile::tempdir().unwrap();
    let env = grim(dir.path(), &Grim::Bytes(b"x"));
    for name in ["", "DP 1", "a;b", "../x", "é", &"a".repeat(65)] {
        let mut out = tempfile::tempfile().unwrap();
        let result = capture_png(
            &env,
            &Launch::Direct,
            name,
            &mut out,
            100,
            in_secs(5),
            &ok_room,
        );
        assert_eq!(result.unwrap_err().code, desktop_code::INVALID_PARAMS);
    }
    assert!(!dir.path().join("grim.log").exists());
    let mut out = tempfile::tempfile().unwrap();
    for name in ["DP-1", "HDMI-A-1", "eDP_1.a", &"a".repeat(64)] {
        let r = capture_png(
            &env,
            &Launch::Direct,
            name,
            &mut out,
            100,
            in_secs(5),
            &ok_room,
        );
        assert!(r.is_ok(), "{name}");
    }
}

#[test]
fn the_exact_cap_passes_and_one_byte_more_is_too_large() {
    for cap in [1000_u64, 70_000] {
        let dir = tempfile::tempdir().unwrap();
        let env = grim(dir.path(), &Grim::Zeros(cap));
        let (result, mut out) = run(&env, cap, in_secs(5));
        assert_eq!(result.unwrap(), cap);
        assert_eq!(contents(&mut out).len() as u64, cap);

        let dir = tempfile::tempdir().unwrap();
        let env = grim(dir.path(), &Grim::Zeros(cap + 1));
        let (result, mut out) = run(&env, cap, in_secs(5));
        assert_eq!(
            fail(result),
            (code::CAPTURE_TOO_LARGE, text::CAPTURE_TOO_LARGE.to_owned())
        );
        // The byte past the cap is never written.
        assert!(contents(&mut out).len() as u64 <= cap);
    }
}

#[test]
fn a_room_refusal_in_the_middle_stops_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let env = grim(dir.path(), &Grim::Zeros(300_000));
    let calls = Cell::new(0_u32);
    let room = |need: u64| {
        assert!(need > 0);
        calls.set(calls.get() + 1);
        if calls.get() >= 2 {
            Err(Fail::new(code::DISK_FULL, text::DISK_FULL))
        } else {
            Ok(())
        }
    };
    let mut out = tempfile::tempfile().unwrap();
    let result = capture_png(
        &env,
        &Launch::Direct,
        "DP-1",
        &mut out,
        1_000_000,
        in_secs(5),
        &room,
    );
    assert_eq!(fail(result), (code::DISK_FULL, text::DISK_FULL.to_owned()));
    assert_eq!(calls.get(), 2);
    assert!(contents(&mut out).len() < 300_000);
}

#[test]
fn a_write_error_enospc_is_disk_full() {
    let dir = tempfile::tempdir().unwrap();
    let env = grim(dir.path(), &Grim::Bytes(b"abc"));
    let mut full = File::options().write(true).open("/dev/full").unwrap();
    let result = capture_png(
        &env,
        &Launch::Direct,
        "DP-1",
        &mut full,
        100,
        in_secs(5),
        &ok_room,
    );
    assert_eq!(fail(result), (code::DISK_FULL, text::DISK_FULL.to_owned()));
}

#[test]
fn a_stalled_stdout_hits_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let env = grim(dir.path(), &Grim::Stall);
    let started = Instant::now();
    let (result, _) = run(&env, 100, Instant::now() + Duration::from_millis(400));
    assert_eq!(
        fail(result),
        (code::CAPTURE_FAILED, text::CAPTURE_FAILED.to_owned())
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn the_timeout_kills_the_whole_group() {
    let dir = tempfile::tempdir().unwrap();
    let env = grim(dir.path(), &Grim::StallWithChild);
    let (result, _) = run(&env, 100, Instant::now() + Duration::from_millis(1500));
    assert_eq!(result.unwrap_err().code, code::CAPTURE_FAILED);
    let pid = std::fs::read_to_string(dir.path().join("child.pid")).unwrap();
    assert!(
        gone(pid.trim()),
        "the sleeping child of grim is still alive"
    );
}

#[test]
fn a_full_stderr_pipe_does_not_stall_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let env = grim(dir.path(), &Grim::Flood(b"png"));
    let started = Instant::now();
    let (result, mut out) = run(&env, 100, in_secs(8));
    assert_eq!(result.unwrap(), 3);
    assert_eq!(contents(&mut out), b"png");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_nonzero_exit_is_capture_failed() {
    let dir = tempfile::tempdir().unwrap();
    let env = grim(dir.path(), &Grim::Exit(3));
    let (result, _) = run(&env, 100, in_secs(5));
    assert_eq!(
        fail(result),
        (code::CAPTURE_FAILED, text::CAPTURE_FAILED.to_owned())
    );
}
