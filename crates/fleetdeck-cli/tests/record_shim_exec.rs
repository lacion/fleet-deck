//! R13 P5b task E3: the `shim-exec` options of the recorder (`--stdin-fd3`,
//! `--fsize`) through the real binary, and the recorder supervisor started
//! through the real `shim-exec` production path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_station::desktop::capture::Launch;
use fleetdeck_station::media::clock::{SystemClocks, boot_now};
use fleetdeck_station::media::recorder::{Deadlines, StopCause, argv, spawn_with};

const EXE: &str = env!("CARGO_BIN_EXE_fleetdeck");

/// `fleetdeck shim-exec --parent <this process> <options> -- sh -c <script>`
/// with `stdin` as its stdin.
fn shim_sh(options: &[&str], script: &str, stdin: File) -> Output {
    Command::new(EXE)
        .arg("shim-exec")
        .arg("--parent")
        .arg(std::process::id().to_string())
        .args(options)
        .args(["--", "/bin/sh", "-c", script])
        .stdin(Stdio::from(stdin))
        .output()
        .unwrap()
}

fn stdin_with(bytes: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file
}

fn read_all(file: &mut File) -> Vec<u8> {
    let mut got = Vec::new();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.read_to_end(&mut got).unwrap();
    got
}

#[test]
fn stdin_fd3_gives_the_child_the_file_on_fd_3_and_null_on_stdin() {
    let mut file = tempfile::tempfile().unwrap();
    let out = shim_sh(
        &["--stdin-fd3"],
        "echo from-fd3 >&3; echo \"stdin=$(readlink /proc/self/fd/0)\"",
        file.try_clone().unwrap(),
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "stdin=/dev/null\n");
    assert_eq!(read_all(&mut file), b"from-fd3\n");
}

#[test]
fn fsize_ends_a_child_that_writes_past_the_limit_with_sigxfsz() {
    let mut file = tempfile::tempfile().unwrap();
    let out = shim_sh(
        &["--stdin-fd3", "--fsize", "4096"],
        "exec head -c 8192 /dev/zero >&3",
        file.try_clone().unwrap(),
    );
    assert_eq!(out.status.signal(), Some(25), "{out:?}");
    assert!(read_all(&mut file).len() <= 4096);
}

#[test]
fn without_the_options_the_launch_is_unchanged() {
    let out = shim_sh(
        &[],
        "read line; echo \"line=$line\"; \
         [ ! -e /proc/self/fd/3 ] && echo no-fd3; echo \"fsize=$(ulimit -f)\"",
        stdin_with(b"hello\n"),
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "line=hello\nno-fd3\nfsize=unlimited\n"
    );
}

#[test]
fn a_bad_fsize_is_refused_by_the_parser() {
    let out = Command::new(EXE)
        .args([
            "shim-exec",
            "--parent",
            "1",
            "--fsize",
            "many",
            "--",
            "/bin/true",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// A fake recorder that logs its argv, its stdin and its file-size limit
/// into the retained file (FD 3), and finishes on `SIGINT`.
fn fake_recorder(dir: &Path) -> PathBuf {
    let path = dir.join("wf-recorder");
    std::fs::write(
        &path,
        "#!/bin/sh\n\
         for a in \"$@\"; do echo \"arg=$a\" >&3; done\n\
         echo \"stdin=$(readlink /proc/self/fd/0)\" >&3\n\
         echo \"fsize=$(ulimit -f)\" >&3\n\
         trap 'echo finished >&3; exit 0' INT\n\
         while :; do sleep 0.05; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn the_supervisor_starts_the_recorder_through_the_real_shim_exec() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_recorder(dir.path());
    let mut env = ChildEnv::empty();
    env.set("PATH", "/usr/bin:/bin").unwrap();
    let mut output = tempfile::tempfile().unwrap();
    let deadlines = Deadlines {
        duration: Duration::from_millis(600),
        total: Duration::from_secs(30),
        lease_until: boot_now().plus(Duration::from_secs(60)),
        grace: Duration::from_secs(3),
    };
    let recorder = spawn_with(
        &Launch::ShimExec {
            exe: PathBuf::from(EXE),
        },
        &env,
        &program,
        "DP-1",
        1,
        &output,
        deadlines,
        Arc::new(SystemClocks),
        Box::new(|| Ok(())),
    )
    .unwrap();
    let exit = recorder.exited().await;
    assert_eq!(exit.cause, Some(StopCause::Duration), "{exit:?}");
    assert!(exit.status.unwrap().success(), "{exit:?}");
    assert!(!exit.escalated && !exit.xfsz);
    let text = String::from_utf8(read_all(&mut output)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let expected: Vec<String> = argv("DP-1", 1)
        .unwrap()
        .into_iter()
        .map(|a| format!("arg={a}"))
        .collect();
    assert_eq!(lines[..expected.len()], expected[..]);
    assert_eq!(lines[expected.len()], "stdin=/dev/null");
    assert!(lines[expected.len() + 1].starts_with("fsize="));
    assert_ne!(lines[expected.len() + 1], "fsize=unlimited");
    assert_eq!(lines.last(), Some(&"finished"));
}
