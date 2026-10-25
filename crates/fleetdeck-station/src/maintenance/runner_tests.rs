//! The runner with real PTYs: the child is `/bin/sh -c <script>`.

use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use fleetdeck_proto::desktop_tools::PostCheck;
use rustix::fs::{FlockOperation, Mode, OFlags, flock};
use tokio::io::AsyncReadExt as _;
use tokio::time::timeout;

use super::files::{self, RunStateKind};
use super::frame::{Frame, read_frame, write_frame};
use super::runner::{ChildPlan, serve};
use super::test_support::{
    ID, POST_OK, STATION_TOKEN, connect, contains, open, postcheck_script, private_dir,
    read_to_exit, read_until, send_input, spec, start,
};

const WAIT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn maintenance_echo_is_off_and_the_typed_secret_is_in_no_output() {
    let run = start("read -r x; echo got-$#", POST_OK);
    let mut client = open(run.path()).await;
    send_input(&mut client, "secret-pass\n").await;
    let (output, code) = read_to_exit(&mut client).await;
    assert_eq!(code, 0);
    assert!(contains(&output, b"got-0"), "{output:?}");
    assert!(!contains(&output, b"secret-pass"));
    let (dir, exit) = run.finished().await;
    assert_eq!(exit, 0);
    let stored = files::read_output(dir.path(), ID).unwrap().unwrap();
    assert!(contains(&stored, b"got-0"));
    assert!(!contains(&stored, b"secret-pass"));
}

#[tokio::test]
async fn maintenance_terminal_baseline_has_no_echo_and_no_echonl() {
    let run = start("stty -a; read -r gate", POST_OK);
    let mut client = open(run.path()).await;
    send_input(&mut client, "\n").await;
    let (output, _) = read_to_exit(&mut client).await;
    assert!(
        contains(&output, b"-echo "),
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert!(contains(&output, b"-echonl"));
    run.finished().await;
}

#[tokio::test]
async fn maintenance_child_gets_the_fixed_environment_and_directory() {
    let run = start(
        "pwd; echo PATH=$PATH TERM=$TERM LANG=$LANG; read -r gate",
        POST_OK,
    );
    let mut client = open(run.path()).await;
    send_input(&mut client, "\n").await;
    let (output, _) = read_to_exit(&mut client).await;
    let text = String::from_utf8_lossy(&output).replace('\r', "");
    assert!(text.starts_with("/\n"), "{text:?}");
    assert!(text.contains("PATH=/usr/bin:/bin TERM=xterm-256color LANG=C.UTF-8\n"));
    run.finished().await;
}

#[tokio::test]
async fn maintenance_wrong_token_is_closed_without_an_answer() {
    let run = start("read -r x; echo end-$x", POST_OK);
    let mut bad = connect(run.path()).await;
    write_frame(&mut bad, &Frame::Hello(b"wrong-token".to_vec()))
        .await
        .unwrap();
    let got = timeout(WAIT, read_frame(&mut bad)).await.unwrap();
    assert_eq!(got.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
    // A token of the right length that differs in one byte.
    let mut near = connect(run.path()).await;
    let mut token = STATION_TOKEN.as_bytes().to_vec();
    token[31] = b'2';
    write_frame(&mut near, &Frame::Hello(token)).await.unwrap();
    assert!(timeout(WAIT, read_frame(&mut near)).await.unwrap().is_err());
    // The run goes on for the right client.
    let mut client = open(run.path()).await;
    send_input(&mut client, "x\n").await;
    let (output, code) = read_to_exit(&mut client).await;
    assert_eq!(code, 0);
    assert!(contains(&output, b"end-x"));
    run.finished().await;
}

#[tokio::test]
async fn maintenance_first_frame_that_is_not_hello_is_closed() {
    let run = start("read -r x; echo end", POST_OK);
    for frame in [
        Frame::Input(b"a\n".to_vec()),
        Frame::SnapshotEnd,
        Frame::Exit(0),
    ] {
        let mut bad = connect(run.path()).await;
        write_frame(&mut bad, &frame).await.unwrap();
        assert!(timeout(WAIT, read_frame(&mut bad)).await.unwrap().is_err());
    }
    // An unknown frame type is closed too.
    let mut bad = connect(run.path()).await;
    tokio::io::AsyncWriteExt::write_all(&mut bad, &[0, 0, 0, 1, 99])
        .await
        .unwrap();
    assert!(timeout(WAIT, read_frame(&mut bad)).await.unwrap().is_err());
    let mut client = open(run.path()).await;
    send_input(&mut client, "x\n").await;
    let (_, code) = read_to_exit(&mut client).await;
    assert_eq!(code, 0);
    run.finished().await;
}

#[tokio::test]
async fn maintenance_a_client_frame_that_is_not_input_or_resize_closes_it() {
    let run = start("read -r x; echo end", POST_OK);
    let mut client = open(run.path()).await;
    read_until_snapshot_end(&mut client).await;
    write_frame(&mut client, &Frame::Output(b"x".to_vec()))
        .await
        .unwrap();
    while timeout(WAIT, read_frame(&mut client))
        .await
        .unwrap()
        .is_ok()
    {}
    // The child was not touched: a new client finishes it.
    let mut next = open(run.path()).await;
    send_input(&mut next, "x\n").await;
    let (_, code) = read_to_exit(&mut next).await;
    assert_eq!(code, 0);
    run.finished().await;
}

async fn read_until_snapshot_end(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        match timeout(WAIT, read_frame(stream)).await.unwrap().unwrap() {
            Frame::Output(bytes) => out.extend(bytes),
            Frame::SnapshotEnd => return out,
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

#[tokio::test]
async fn maintenance_replay_gives_the_ring_and_a_new_client_replaces_the_old() {
    let run = start("echo first; read -r x; echo second", POST_OK);
    let mut old = open(run.path()).await;
    read_until(&mut old, "first").await;
    let mut new = open(run.path()).await;
    let snapshot = read_until_snapshot_end(&mut new).await;
    assert!(contains(&snapshot, b"first"), "{snapshot:?}");
    // The old client reads the end of the stream.
    while timeout(WAIT, read_frame(&mut old)).await.unwrap().is_ok() {}
    send_input(&mut new, "go\n").await;
    let (output, code) = read_to_exit(&mut new).await;
    assert_eq!(code, 0);
    assert!(contains(&output, b"second"));
    let (dir, _) = run.finished().await;
    let stored = files::read_output(dir.path(), ID).unwrap().unwrap();
    assert!(contains(&stored, b"first") && contains(&stored, b"second"));
}

#[tokio::test]
async fn maintenance_resize_reaches_the_terminal() {
    let run = start("read -r x; stty size", POST_OK);
    let mut client = open(run.path()).await;
    write_frame(
        &mut client,
        &Frame::Resize {
            cols: 132,
            rows: 43,
        },
    )
    .await
    .unwrap();
    // A size out of range is ignored.
    write_frame(
        &mut client,
        &Frame::Resize {
            cols: 0,
            rows: 5000,
        },
    )
    .await
    .unwrap();
    send_input(&mut client, "x\n").await;
    let (output, _) = read_to_exit(&mut client).await;
    assert!(
        contains(&output, b"43 132"),
        "{}",
        String::from_utf8_lossy(&output)
    );
    run.finished().await;
}

#[tokio::test]
async fn maintenance_input_after_the_exit_is_discarded_and_the_exit_is_recorded() {
    let run = start(
        "echo bye; read -r gate; exit 7",
        "sleep 1; echo 'wtype 1.0-1'",
    );
    let mut client = open(run.path()).await;
    read_until(&mut client, "bye").await;
    send_input(&mut client, "\n").await;
    // The runner waits in the post-check: the child is gone.
    tokio::time::sleep(Duration::from_millis(300)).await;
    send_input(&mut client, "late-input\n").await;
    let (output, code) = read_to_exit(&mut client).await;
    assert_eq!(code, 7);
    assert!(!contains(&output, b"late-input"));
    let (dir, exit) = run.finished().await;
    assert_eq!(exit, 0);
    let state = files::read_state(dir.path(), ID).unwrap().unwrap();
    assert_eq!(state.state, RunStateKind::Exited);
    assert_eq!(state.exit_code, Some(7));
    assert_eq!(state.post_check, Some(PostCheck::Ok));
    let stored = files::read_output(dir.path(), ID).unwrap().unwrap();
    assert!(contains(&stored, b"bye"));
    assert!(!contains(&stored, b"late-input"));
    // The socket path of this runner is gone.
    assert!(!files::socket_path(dir.path(), ID).exists());
}

#[tokio::test]
async fn maintenance_post_check_that_differs_is_recorded_as_a_change() {
    let run = start("echo done; read -r gate", "echo 'wtype 9.9-9'");
    let mut client = open(run.path()).await;
    send_input(&mut client, "\n").await;
    let (_, code) = read_to_exit(&mut client).await;
    assert_eq!(code, 0);
    let (dir, _) = run.finished().await;
    let state = files::read_state(dir.path(), ID).unwrap().unwrap();
    assert_eq!(state.post_check, Some(PostCheck::TransactionChanged));
}

#[tokio::test]
async fn maintenance_a_child_that_dies_by_a_signal_has_the_code_minus_one() {
    let run = start("kill -9 $$", POST_OK);
    let (dir, exit) = run.finished().await;
    assert_eq!(exit, 0);
    let state = files::read_state(dir.path(), ID).unwrap().unwrap();
    assert_eq!(state.exit_code, Some(-1));
}

#[tokio::test]
async fn maintenance_run_without_a_client_still_records_its_exit() {
    let run = start("echo alone", POST_OK);
    let (dir, exit) = run.finished().await;
    assert_eq!(exit, 0);
    let state = files::read_state(dir.path(), ID).unwrap().unwrap();
    assert_eq!(state.state, RunStateKind::Exited);
    let stored = files::read_output(dir.path(), ID).unwrap().unwrap();
    assert!(contains(&stored, b"alone"));
}

#[tokio::test]
async fn maintenance_state_is_running_while_the_child_runs() {
    let run = start("read -r x", POST_OK);
    let mut client = open(run.path()).await;
    let state = files::read_state(run.path(), ID).unwrap().unwrap();
    assert_eq!(state.state, RunStateKind::Running);
    assert_eq!(state.exit_code, None);
    let sock = files::socket_path(run.path(), ID);
    let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    send_input(&mut client, "x\n").await;
    read_to_exit(&mut client).await;
    run.finished().await;
}

fn plan(dir: &std::path::Path) -> ChildPlan {
    ChildPlan {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "echo must-not-run".into()],
        postcheck_program: postcheck_script(dir, POST_OK),
    }
}

#[tokio::test]
async fn maintenance_runner_with_a_state_file_exits_3_and_leaves_it() {
    let dir = private_dir();
    let state = files::state_path(dir.path(), ID);
    std::fs::write(&state, b"old").unwrap();
    let code = serve(dir.path(), spec(ID), plan(dir.path())).await;
    assert_eq!(code, 3);
    assert_eq!(std::fs::read(&state).unwrap(), b"old");
    assert!(!files::output_path(dir.path(), ID).exists());
}

#[tokio::test]
async fn maintenance_runner_with_a_socket_path_exits_3_and_does_not_remove_it() {
    let dir = private_dir();
    let sock = files::socket_path(dir.path(), ID);
    drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
    assert!(sock.exists());
    let code = serve(dir.path(), spec(ID), plan(dir.path())).await;
    assert_eq!(code, 3);
    assert!(sock.exists());
    assert!(files::read_state(dir.path(), ID).unwrap().is_none());
}

#[tokio::test]
async fn maintenance_runner_with_a_link_at_the_socket_path_exits_3() {
    let dir = private_dir();
    let sock = files::socket_path(dir.path(), ID);
    std::os::unix::fs::symlink("/nonexistent-target", &sock).unwrap();
    let code = serve(dir.path(), spec(ID), plan(dir.path())).await;
    assert_eq!(code, 3);
    assert!(std::fs::symlink_metadata(&sock).is_ok());
}

#[tokio::test]
async fn maintenance_runner_that_finds_the_lock_held_exits_3() {
    let dir = private_dir();
    let lock = rustix::fs::open(
        files::lock_path(dir.path(), ID),
        OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .unwrap();
    flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
    let code = serve(dir.path(), spec(ID), plan(dir.path())).await;
    assert_eq!(code, 3);
    let _keep: OwnedFd = lock;
    assert!(files::read_state(dir.path(), ID).unwrap().is_none());
}

#[test]
fn maintenance_main_refuses_a_bad_spec_with_the_code_2() {
    let dir = private_dir();
    assert_eq!(super::runner::main(dir.path(), ID), 2);
    assert_eq!(super::runner::main(dir.path(), "not-an-id"), 2);
    let mut bad = spec(ID);
    bad.tools = vec!["wlrctl".to_owned()];
    let path = files::spec_path(dir.path(), ID);
    std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(super::runner::main(dir.path(), ID), 2);
    assert!(files::read_state(dir.path(), ID).unwrap().is_none());
}

#[test]
fn maintenance_plan_is_the_fixed_transaction_of_the_spec() {
    let mut s = spec(ID);
    s.tools = vec!["wtype".to_owned(), "busctl".to_owned()];
    let plan = ChildPlan::from_spec(&s);
    assert_eq!(plan.program, std::path::PathBuf::from("/usr/bin/sudo"));
    let args: Vec<_> = plan
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "/usr/bin/pacman",
            "-S",
            "--needed",
            "--noconfirm",
            "extra/wtype",
            "core/systemd"
        ]
    );
    assert_eq!(
        plan.postcheck_program,
        std::path::PathBuf::from("/usr/bin/pacman")
    );
}

#[tokio::test]
async fn maintenance_slow_client_is_dropped_and_the_child_still_finishes() {
    let run = start(
        "read -r gate; head -c 6000000 /dev/zero | tr '\\0' x; echo finished-line",
        POST_OK,
    );
    // The client does the handshake, starts the output and never reads.
    let mut slow = open(run.path()).await;
    send_input(&mut slow, "\n").await;
    let (dir, exit) = run.finished().await;
    assert_eq!(exit, 0);
    let state = files::read_state(dir.path(), ID).unwrap().unwrap();
    assert_eq!(state.state, RunStateKind::Exited);
    assert_eq!(state.exit_code, Some(0));
    let stored = files::read_output(dir.path(), ID).unwrap().unwrap();
    assert!(contains(&stored, b"finished-line"));
    assert_eq!(stored.len(), 256 * 1024);
    // The connection was closed by the runner: the rest of the stream ends.
    let mut sink = vec![0_u8; 64 * 1024];
    timeout(WAIT, async {
        while slow.read(&mut sink).await.unwrap_or(0) > 0 {}
    })
    .await
    .unwrap();
}
