//! Tests of [`DirectChildHost`]. Most use `/bin/sh` (Linux and macOS, A11);
//! the fake agents prove the path that the adapters use.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_testkit::FakeAgent;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader, Lines};

use super::*;

const QUICK: StopPolicy = StopPolicy {
    grace: Duration::from_millis(300),
    term_grace: Duration::from_millis(300),
};

fn base_env() -> ChildEnv {
    let mut env = ChildEnv::empty();
    env.set("PATH", "/usr/bin:/bin").unwrap();
    env
}

fn sh(script: &str, cwd: &Path) -> ProcessSpec {
    ProcessSpec {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), script.into()],
        cwd: cwd.to_path_buf(),
        env: base_env(),
        launch: None,
    }
}

fn fake(agent: FakeAgent, scenario: &str, env: ChildEnv, cwd: &Path) -> ProcessSpec {
    let mut env = env;
    env.set(fleetdeck_testkit::fake::SCENARIO_ENV, scenario)
        .unwrap();
    ProcessSpec {
        program: agent.locate().unwrap(),
        args: Vec::new(),
        cwd: cwd.to_path_buf(),
        env,
        launch: None,
    }
}

type Out = Lines<BufReader<Box<dyn AsyncRead + Send + Unpin>>>;

async fn start(spec: ProcessSpec) -> (Out, Arc<dyn ProcessControl>) {
    let p = DirectChildHost::new().spawn(spec).await.unwrap();
    (BufReader::new(p.stdout).lines(), p.control)
}

async fn line(out: &mut Out) -> String {
    tokio::time::timeout(Duration::from_secs(10), out.next_line())
        .await
        .expect("no output in time")
        .unwrap()
        .expect("stdout ended")
}

/// True when no process `pid` exists (it may take a moment to be reaped by
/// init after its group was killed).
async fn gone(pid: i32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if rustix::process::test_kill_process(pid).is_err() || is_zombie(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// A killed grandchild can stay a zombie until its new parent reaps it.
fn is_zombie(pid: rustix::process::Pid) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let stat = format!("/proc/{}/stat", pid.as_raw_nonzero());
    std::fs::read_to_string(stat).is_ok_and(|s| {
        s.rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
    })
}

#[tokio::test]
async fn a_fake_agent_runs_through_the_host_and_stops_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let spec = fake(FakeAgent::Claude, "echo", base_env(), dir.path());
    let (mut out, control) = start(spec).await;
    control.write(b"{\"type\":\"ping\"}\n").await.unwrap();
    assert_eq!(line(&mut out).await, "{\"type\":\"ping\"}");
    control.write(b"second\n").await.unwrap();
    assert_eq!(line(&mut out).await, "second");

    let report = control.stop(StopPolicy::default()).await;
    assert_eq!(report.termination, Termination::Code(0));
    assert_eq!(report.cause, EndCause::Stopped);
    assert_eq!(control.exit(), Some(report));
    assert!(matches!(
        control.write(b"late\n").await,
        Err(HostError::StdinClosed)
    ));
}

#[tokio::test]
async fn the_child_gets_exactly_the_scrubbed_environment() {
    let dir = tempfile::tempdir().unwrap();
    let ambient: Vec<(OsString, OsString)> = [
        ("PATH", "/usr/bin:/bin"),
        ("LANG", "C.UTF-8"),
        ("ANTHROPIC_API_KEY", "ambient-key"),
        ("CLAUDECODE", "1"),
        ("OPENAI_API_KEY", "ambient-openai"),
        ("FLEETDECK_LOG", "trace"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let mut instance = fleetdeck_core::config::ProviderInstance::default_for(
        fleetdeck_core::config::Driver::Claude,
    );
    instance.config_home = Some(dir.path().join("claude-home"));
    let env = ChildEnv::for_instance(ChildEnv::scrubbed(ambient), &instance).unwrap();
    let (mut out, control) = start(fake(FakeAgent::Claude, "env", env, dir.path())).await;
    let mut seen = Vec::new();
    while let Ok(Some(l)) = out.next_line().await {
        seen.push(l);
    }
    let expected = [
        format!(
            "CLAUDE_CONFIG_DIR={}",
            dir.path().join("claude-home").display()
        ),
        "FLEETDECK_FAKE_SCENARIO=env".to_owned(),
        "LANG=C.UTF-8".to_owned(),
        "PATH=/usr/bin:/bin".to_owned(),
    ];
    assert_eq!(seen, expected);
    assert_eq!(control.wait().await.termination, Termination::Code(0));
}

#[tokio::test]
async fn the_working_directory_is_the_spec_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let (mut out, control) = start(sh("pwd -P", dir.path())).await;
    let real = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(PathBuf::from(line(&mut out).await), real);
    assert_eq!(control.wait().await.cause, EndCause::Exited);
}

#[tokio::test]
async fn the_exit_report_has_the_code_and_the_stderr_tail() {
    let dir = tempfile::tempdir().unwrap();
    let (_out, control) = start(sh("echo boom >&2; exit 3", dir.path())).await;
    let report = control.wait().await;
    assert_eq!(report.termination, Termination::Code(3));
    assert_eq!(report.code(), Some(3));
    assert_eq!(report.cause, EndCause::Exited);
    assert_eq!(report.stderr_tail, "boom\n");
}

#[tokio::test]
async fn a_stderr_flood_does_not_block_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let script = "i=0; while [ $i -lt 2000 ]; do echo 'noise noise noise noise noise noise' >&2; \
                  i=$((i+1)); done; echo done >&2; echo finished";
    let (mut out, control) = start(sh(script, dir.path())).await;
    assert_eq!(line(&mut out).await, "finished");
    let report = control.wait().await;
    assert!(report.stderr_tail.len() <= STDERR_TAIL_BYTES);
    assert!(
        report.stderr_tail.ends_with("done\n"),
        "{}",
        report.stderr_tail
    );
}

#[tokio::test]
async fn stop_escalates_to_sigterm_when_the_child_ignores_end_of_input() {
    let dir = tempfile::tempdir().unwrap();
    let (_out, control) = start(sh("exec sleep 30", dir.path())).await;
    let t0 = Instant::now();
    let report = control.stop(QUICK).await;
    assert_eq!(report.termination, Termination::Signal(15));
    assert_eq!(report.cause, EndCause::StopForced);
    assert!(t0.elapsed() >= QUICK.grace);
}

#[tokio::test]
async fn stop_escalates_to_sigkill_when_the_child_ignores_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let script = "trap '' TERM; echo ready; while :; do sleep 0.05; done";
    let (mut out, control) = start(sh(script, dir.path())).await;
    assert_eq!(line(&mut out).await, "ready");
    let report = control.stop(QUICK).await;
    assert_eq!(report.termination, Termination::Signal(9));
    assert_eq!(report.cause, EndCause::StopForced);
}

#[tokio::test]
async fn kill_ends_the_whole_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let (mut out, control) = start(sh("sleep 30 & echo $!; wait", dir.path())).await;
    let grandchild: i32 = line(&mut out).await.parse().unwrap();
    let report = control.kill().await;
    assert_eq!(report.termination, Termination::Signal(9));
    assert_eq!(report.cause, EndCause::Killed);
    assert!(
        gone(grandchild).await,
        "background process {grandchild} survived the kill"
    );
}

#[tokio::test]
async fn background_work_dies_when_the_leader_exits() {
    let dir = tempfile::tempdir().unwrap();
    let (mut out, control) = start(sh("sleep 30 & echo $!", dir.path())).await;
    let grandchild: i32 = line(&mut out).await.parse().unwrap();
    let report = control.wait().await;
    assert_eq!(report.termination, Termination::Code(0));
    assert_eq!(report.cause, EndCause::Exited);
    assert!(
        gone(grandchild).await,
        "background process {grandchild} outlived its session"
    );
}

#[tokio::test]
async fn dropping_the_control_kills_a_running_group() {
    let dir = tempfile::tempdir().unwrap();
    let (mut out, control) = start(sh("sleep 30 & echo $!; wait", dir.path())).await;
    let grandchild: i32 = line(&mut out).await.parse().unwrap();
    let leader = i32::try_from(control.pid()).unwrap();
    drop(control);
    assert!(
        gone(grandchild).await,
        "background process survived the drop"
    );
    assert!(gone(leader).await, "the leader survived the drop");
}

#[tokio::test]
async fn the_child_leads_its_own_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let (mut out, control) = start(sh("ps -o pgid= -p $$", dir.path())).await;
    let pgid: u32 = line(&mut out).await.trim().parse().unwrap();
    assert_eq!(pgid, control.pid());
    assert_ne!(pgid, std::process::id());
    control.wait().await;
}

#[tokio::test]
async fn stop_and_kill_after_the_exit_return_the_first_report() {
    let dir = tempfile::tempdir().unwrap();
    let (_out, control) = start(sh("exit 4", dir.path())).await;
    let first = control.wait().await;
    assert_eq!(control.stop(QUICK).await, first);
    assert_eq!(control.kill().await, first);
    assert_eq!(first.cause, EndCause::Exited);
}

#[tokio::test]
async fn a_missing_program_is_a_spawn_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = sh("true", dir.path());
    spec.program = dir.path().join("no-such-cli");
    let err = DirectChildHost::new().spawn(spec).await.unwrap_err();
    assert!(matches!(err, HostError::Spawn { .. }), "{err}");
}

#[tokio::test]
async fn the_direct_host_does_not_survive_a_station_restart() {
    let host = DirectChildHost::new();
    assert_eq!(host.kind(), HostKind::DirectChild);
    assert!(!host.survives_station_restart());
}

/// More than a pipe buffer (64 KiB on Linux, 16 KiB on macOS).
const BIG: usize = 1024 * 1024;

#[tokio::test]
async fn stop_keeps_its_deadline_while_a_write_to_a_non_reading_child_blocks() {
    let dir = tempfile::tempdir().unwrap();
    // `sleep` never reads stdin, so the write fills the pipe and blocks.
    let (_out, control) = start(sh("exec sleep 30", dir.path())).await;
    let writer = {
        let control = Arc::clone(&control);
        tokio::spawn(async move { control.write(&vec![b'x'; BIG]).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !writer.is_finished(),
        "the write should block on the full pipe"
    );

    let t0 = Instant::now();
    let report = tokio::time::timeout(Duration::from_secs(5), control.stop(QUICK))
        .await
        .expect("stop must not wait behind the blocked write");
    assert!(t0.elapsed() < QUICK.grace + QUICK.term_grace + Duration::from_secs(1));
    assert_eq!(report.cause, EndCause::StopForced);
    assert_eq!(report.termination, Termination::Signal(15));
    let written = tokio::time::timeout(Duration::from_secs(5), writer)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(written, Err(HostError::StdinClosed)),
        "{written:?}"
    );
}

#[tokio::test]
async fn close_stdin_releases_a_blocked_write_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (_out, control) = start(sh("exec sleep 30", dir.path())).await;
    let writer = {
        let control = Arc::clone(&control);
        tokio::spawn(async move { control.write(&vec![b'x'; BIG]).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!writer.is_finished());
    tokio::time::timeout(Duration::from_secs(2), control.close_stdin())
        .await
        .expect("close_stdin must not wait for the child");
    let written = tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(written, Err(HostError::StdinClosed)),
        "{written:?}"
    );
    assert!(matches!(
        control.write(b"more").await,
        Err(HostError::StdinClosed)
    ));
    // The child did not read, so it still runs; kill also returns promptly.
    let report = tokio::time::timeout(Duration::from_secs(5), control.kill())
        .await
        .unwrap();
    assert_eq!(report.cause, EndCause::Killed);
}

#[tokio::test]
async fn kill_releases_a_blocked_write() {
    let dir = tempfile::tempdir().unwrap();
    let (_out, control) = start(sh("exec sleep 30", dir.path())).await;
    let writer = {
        let control = Arc::clone(&control);
        tokio::spawn(async move { control.write(&vec![b'x'; BIG]).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let report = tokio::time::timeout(Duration::from_secs(5), control.kill())
        .await
        .unwrap();
    assert_eq!(report.termination, Termination::Signal(9));
    let written = tokio::time::timeout(Duration::from_secs(5), writer)
        .await
        .unwrap()
        .unwrap();
    assert!(written.is_err(), "{written:?}");
}

/// A canonical temp root and the identity of its sub folder `a` (B9).
#[cfg(target_os = "linux")]
fn pinned() -> (
    tempfile::TempDir,
    PathBuf,
    fleetdeck_proto::launch::LaunchIdentity,
) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir(root.join("a")).unwrap();
    let dir = fleetdeck_core::launch_dir::LaunchDir::open(&root, Path::new("a")).unwrap();
    let id = dir.identity().clone();
    (tmp, root, id)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_spawn_with_an_identity_runs_in_the_folder() {
    let (_tmp, root, id) = pinned();
    let mut spec = sh("pwd -P", &root);
    spec.launch = Some(id);
    let (mut out, control) = start(spec).await;
    assert_eq!(line(&mut out).await, root.join("a").to_str().unwrap());
    control.wait().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_spawn_with_an_identity_after_a_swap_fails_with_the_text() {
    let (_tmp, root, id) = pinned();
    std::fs::create_dir(root.join("other")).unwrap();
    std::fs::rename(root.join("a"), root.join("moved")).unwrap();
    std::os::unix::fs::symlink(root.join("other"), root.join("a")).unwrap();
    let mut spec = sh("pwd -P", &root);
    spec.launch = Some(id);
    let err = DirectChildHost::new().spawn(spec).await.unwrap_err();
    assert!(matches!(err, HostError::LaunchDir(_)), "{err:?}");
    assert_eq!(err.to_string(), fleetdeck_core::launch_dir::CHANGED);
}
