//! R13 P5a task D1 (plan item 18): the screenshot runner through the real
//! `shim-exec` production path. A fake `grim` script on a temp `PATH` stands
//! in for the tool; the `fleetdeck` binary is the shim.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::capture::{code, text};
use fleetdeck_station::desktop::Fail;
use fleetdeck_station::desktop::capture::{Launch, capture_png};

fn launch() -> Launch {
    Launch::ShimExec {
        exe: PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
    }
}

/// A `PATH` folder with a fake `grim` that logs its argv to `<dir>/grim.log`,
/// one argument per line, then runs `action`.
fn fake_grim(dir: &Path, action: &str) -> ChildEnv {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("grim");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n{{ for a in \"$@\"; do echo \"$a\"; done; }} >> {}/grim.log\n{action}\n",
            dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut env = ChildEnv::empty();
    env.set("PATH", &bin).unwrap();
    env
}

fn ok_room(_: u64) -> Result<(), Fail> {
    Ok(())
}

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
fn capture_through_shim_exec_delivers_the_bytes_and_the_exact_argv() {
    let dir = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0..=255_u8).collect();
    std::fs::write(dir.path().join("out.bin"), &bytes).unwrap();
    let env = fake_grim(
        dir.path(),
        &format!("/bin/cat {}/out.bin", dir.path().display()),
    );
    let mut out = tempfile::tempfile().unwrap();
    let n = capture_png(
        &env,
        &launch(),
        "DP-1",
        &mut out,
        1000,
        Instant::now() + Duration::from_secs(10),
        &ok_room,
    )
    .unwrap();
    assert_eq!(n, 256);
    out.seek(SeekFrom::Start(0)).unwrap();
    let mut got = Vec::new();
    out.read_to_end(&mut got).unwrap();
    assert_eq!(got, bytes);
    // The `--` of the shim argv never reaches `exec`, and grim got its six
    // arguments exactly.
    let log = std::fs::read_to_string(dir.path().join("grim.log")).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["-t", "png", "-s", "1", "-l", "6", "-o", "DP-1", "-"]
    );
}

#[test]
fn capture_through_shim_exec_kills_a_stalled_group_at_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let env = fake_grim(
        dir.path(),
        &format!(
            "/bin/sleep 30 &\necho $! > {}/child.pid\nwait",
            dir.path().display()
        ),
    );
    let mut out = tempfile::tempfile().unwrap();
    let started = Instant::now();
    let result = capture_png(
        &env,
        &launch(),
        "DP-1",
        &mut out,
        1000,
        Instant::now() + Duration::from_millis(2500),
        &ok_room,
    );
    let e = result.unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (code::CAPTURE_FAILED, text::CAPTURE_FAILED)
    );
    assert!(started.elapsed() < Duration::from_secs(8));
    let pid = std::fs::read_to_string(dir.path().join("child.pid")).unwrap();
    assert!(
        gone(pid.trim()),
        "the sleeping child of grim is still alive"
    );
}

#[test]
fn capture_through_shim_exec_reports_a_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let env = fake_grim(dir.path(), "exit 3");
    let mut out: File = tempfile::tempfile().unwrap();
    let result = capture_png(
        &env,
        &launch(),
        "DP-1",
        &mut out,
        1000,
        Instant::now() + Duration::from_secs(10),
        &ok_room,
    );
    assert_eq!(result.unwrap_err().code, code::CAPTURE_FAILED);
}
