//! The marker cleanup of a precheck (FDV2-005): a child that leaves the
//! process group with `setsid` does not outlive the precheck.

use std::path::Path;

use super::*;

fn env() -> ChildEnv {
    ChildEnv::from_process()
}

/// A precheck command: it records its marker, starts `setsid sleep` and
/// waits until that child has written its pid, then runs `tail`.
fn detaching(tail: &str) -> String {
    format!(
        "echo $FLEETDECK_PRECHECK_ID > id.txt; \
         setsid sh -c 'echo $$ > child.pid; exec sleep 600' & \
         while [ ! -s child.pid ]; do sleep 0.05; done; {tail}"
    )
}

fn read_pid(dir: &Path) -> i32 {
    std::fs::read_to_string(dir.join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// True when the process is gone or only a zombie that init has not reaped.
fn dead(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rfind(')')
            .and_then(|at| stat[at + 1..].split_whitespace().next().map(str::to_owned))
            .is_some_and(|state| state == "Z"),
    }
}

async fn wait_dead(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !dead(pid) {
        assert!(Instant::now() < deadline, "process {pid} still runs");
        sleep(Duration::from_millis(50)).await;
    }
}

/// How many processes still carry the marker in `dir/id.txt`.
fn marked_left(dir: &Path) -> usize {
    let id = std::fs::read_to_string(dir.join("id.txt")).unwrap();
    let id = id.trim();
    assert_eq!(id.len(), 32, "the marker has 32 hex characters: {id}");
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    setup_cleanup::kill_marked(MARKER_VAR, id, Duration::ZERO).unwrap()
}

#[tokio::test]
async fn a_detached_child_is_killed_when_the_precheck_ends() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(&detaching("exit 0"), dir.path(), &env(), TIMEOUT)
        .await
        .unwrap();
    assert_eq!(r.exit_code, Some(0));
    assert!(!r.timed_out);
    assert_eq!(marked_left(dir.path()), 0);
    wait_dead(read_pid(dir.path())).await;
}

#[tokio::test]
async fn a_detached_child_is_killed_when_the_precheck_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(
        &detaching("sleep 30"),
        dir.path(),
        &env(),
        Duration::from_millis(1500),
    )
    .await
    .unwrap();
    assert!(r.timed_out);
    assert_eq!(marked_left(dir.path()), 0);
    wait_dead(read_pid(dir.path())).await;
}

#[tokio::test]
async fn kill_all_marked_kills_a_detached_child_of_a_running_precheck() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let task = tokio::spawn({
        let cwd = cwd.clone();
        async move { run(&detaching("sleep 30"), &cwd, &env(), TIMEOUT).await }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.path().join("child.pid").exists()
        || std::fs::read_to_string(dir.path().join("child.pid"))
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        assert!(Instant::now() < deadline, "the child did not start");
        sleep(Duration::from_millis(20)).await;
    }
    let pid = read_pid(dir.path());
    assert!(!dead(pid));
    tokio::task::spawn_blocking(kill_all_marked).await.unwrap();
    wait_dead(pid).await;
    // The shell is killed too, so the run ends without an exit code.
    let r = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(r.exit_code, None);
    assert!(!r.timed_out);
}

#[tokio::test]
async fn a_dropped_run_still_kills_its_detached_child() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let task = tokio::spawn({
        let cwd = cwd.clone();
        async move { run(&detaching("sleep 30"), &cwd, &env(), TIMEOUT).await }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(dir.path().join("child.pid"))
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        assert!(Instant::now() < deadline, "the child did not start");
        sleep(Duration::from_millis(20)).await;
    }
    let pid = read_pid(dir.path());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    wait_dead(pid).await;
}

#[test]
fn a_marker_is_32_random_hex_characters() {
    let (a, b) = (new_marker().unwrap(), new_marker().unwrap());
    assert_eq!(a.len(), 32);
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

#[tokio::test]
async fn a_cleanup_that_leaves_a_process_or_cannot_scan_is_a_leftover() {
    assert!(!leftover(Ok(Ok(0))));
    assert!(leftover(Ok(Ok(1))));
    assert!(leftover(Ok(Err("cannot list".into()))));
    let stopped = tokio::spawn(async { panic!("the scan panicked") })
        .await
        .unwrap_err();
    assert!(leftover(Err(stopped)));
    assert_eq!(
        PrecheckError::Leftover,
        PrecheckError::Leftover,
        "the variant exists"
    );
}
