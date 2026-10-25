//! Helpers of the maintenance tests: a spec, a private folder, a runner that
//! runs `/bin/sh -c <script>` and a client that does the handshake.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::desktop_tools::ResolvedPackage;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::files::{self, Spec};
use super::frame::{Frame, read_frame, write_frame};
use super::runner::{ChildPlan, serve};

pub(super) const ID: &str = "abcdef0123456789abcdef0123456789";
pub(super) const STATION_TOKEN: &str = "11111111111111111111111111111111";
pub(super) const RUNNER_TOKEN: &str = "22222222222222222222222222222222";
/// The output of a post-check that agrees with [`spec`].
pub(super) const POST_OK: &str = "echo 'wtype 1.0-1'";
const WAIT: Duration = Duration::from_secs(30);

pub(super) fn package(name: &str, version: &str) -> ResolvedPackage {
    ResolvedPackage {
        repo: "extra".to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
    }
}

pub(super) fn spec(id: &str) -> Spec {
    Spec {
        v: 1,
        maintenance_id: id.to_owned(),
        request_id: "33333333333333333333333333333333".to_owned(),
        deck: "deck-fingerprint".to_owned(),
        plan_id: "44444444444444444444444444444444".to_owned(),
        tools: vec!["wtype".to_owned()],
        transaction: vec![package("wtype", "1.0-1")],
        conf_sha256: "a".repeat(64),
        cols: 100,
        rows: 30,
        station_token: STATION_TOKEN.to_owned(),
        runner_token: RUNNER_TOKEN.to_owned(),
        created_at: 1_700_000_000_000,
    }
}

/// A new folder with the mode 0700.
pub(super) fn private_dir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Writes an executable post-check script that runs `body`.
pub(super) fn postcheck_script(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("postcheck.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A runner in a task.
pub(super) struct Run {
    pub dir: TempDir,
    handle: JoinHandle<i32>,
}

impl Run {
    /// Waits for the end of the runner and returns its exit code.
    pub(super) async fn finished(self) -> (TempDir, i32) {
        let code = timeout(WAIT, self.handle).await.unwrap().unwrap();
        (self.dir, code)
    }

    pub(super) fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Starts the runner of [`ID`] with the child `/bin/sh -c <script>` and a
/// post-check that runs `post`.
pub(super) fn start(script: &str, post: &str) -> Run {
    let dir = private_dir();
    let spec = spec(ID);
    files::write_spec(dir.path(), &spec).unwrap();
    let plan = ChildPlan {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        postcheck_program: postcheck_script(dir.path(), post),
    };
    let path = dir.path().to_owned();
    let handle = tokio::spawn(async move { serve(&path, spec, plan).await });
    Run { dir, handle }
}

/// Connects to the socket of the run, as soon as it listens.
pub(super) async fn connect(dir: &Path) -> UnixStream {
    let sock = files::socket_path(dir, ID);
    for _ in 0..500 {
        if let Ok(stream) = UnixStream::connect(&sock).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the socket did not listen");
}

/// Connects and does the handshake with the right token.
pub(super) async fn open(dir: &Path) -> UnixStream {
    let mut stream = connect(dir).await;
    write_frame(&mut stream, &Frame::Hello(STATION_TOKEN.into()))
        .await
        .unwrap();
    let answer = timeout(WAIT, read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer, Frame::HelloOk(RUNNER_TOKEN.into()));
    stream
}

pub(super) async fn send_input(stream: &mut UnixStream, text: &str) {
    write_frame(stream, &Frame::Input(text.into()))
        .await
        .unwrap();
}

/// Reads frames until the output holds `needle`. Returns the output so far.
pub(super) async fn read_until(stream: &mut UnixStream, needle: &str) -> Vec<u8> {
    let mut out = Vec::new();
    timeout(WAIT, async {
        while !contains(&out, needle.as_bytes()) {
            match read_frame(stream).await.unwrap() {
                Frame::Output(bytes) => out.extend(bytes),
                Frame::SnapshotEnd => {}
                other => panic!("unexpected frame {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    out
}

/// Reads frames until `Exit`. Returns the output and the exit code.
pub(super) async fn read_to_exit(stream: &mut UnixStream) -> (Vec<u8>, i32) {
    let mut out = Vec::new();
    let code = timeout(WAIT, async {
        loop {
            match read_frame(stream).await.unwrap() {
                Frame::Output(bytes) => out.extend(bytes),
                Frame::Exit(code) => return code,
                Frame::SnapshotEnd => {}
                other => panic!("unexpected frame {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    (out, code)
}

pub(super) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}
