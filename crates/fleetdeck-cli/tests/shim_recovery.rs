//! Station recovery of shim rows with real shims (ADR-005 §3, §3.2, §4.1;
//! slice 4, DL-104): the `shims` table of the station store, the
//! generation committed before every attach, the committed output cursor,
//! and every launch and recovery boundary. `/bin/sh` fake providers.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::event::{Event, ProviderKind};
use fleetdeck_providers::checkpoint::{AdapterCheckpoint, REASON_UNSUPPORTED};
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost, ShimHostConfig};
use fleetdeck_providers::host::shim::journal::{Record, ShimFiles};
use fleetdeck_providers::host::shim::proof::AttachKey;
use fleetdeck_providers::host::shim::wire::ExitState;
use fleetdeck_providers::host::{ProcessControl as _, ProcessSpec};
use fleetdeck_station::manager::shims::{
    LaunchError, Recovered, launch_shim, reason, recover_shims,
};
use fleetdeck_station::store::{
    NewEvent, NewSession, NewShim, OfflineApprovalPolicy, OutputCommit, OutputMeta, RequestTime,
    ShimState, Store, StoreConfig,
};
use fleetdeck_testkit::Det;
use tokio::io::{AsyncBufReadExt as _, BufReader};

mod shim_reaper;

use shim_reaper::{kill9, shim_pid};

const WAIT: Duration = Duration::from_secs(10);
const LINES: &str =
    "i=0; while [ $i -lt 30 ]; do echo line$i; i=$((i+1)); sleep 0.02; done; sleep 30";

struct Rig {
    tmp: tempfile::TempDir,
    store: Store,
    _det: Det,
}

impl Rig {
    async fn new() -> Self {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        store
            .create_session(NewSession {
                session_id: "s1".into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("station")).unwrap();
        Self {
            tmp,
            store,
            _det: det,
        }
    }

    /// A fresh host per station start: it holds only configuration. Its
    /// incarnation is the store's, as on a real station.
    fn host(&self) -> ShimHost {
        let mut config = ShimHostConfig::new(
            PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
            self.tmp.path().join("station"),
            self.store.incarnation().as_str().to_owned(),
        );
        config.runtime_dir = None;
        config.tmp_dir = self.tmp.path().to_owned();
        config.buffer_bytes = 1024 * 1024;
        ShimHost::new(config)
    }

    fn spec(&self, script: &str) -> ProcessSpec {
        ProcessSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: self.tmp.path().to_owned(),
            env: ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]),
        }
    }

    async fn launch(&self, script: &str) -> Attached {
        launch_shim(
            &self.store,
            &self.host(),
            &"s1".into(),
            &self.spec(script),
            AckMode::Manual,
        )
        .await
        .unwrap()
    }

    fn files(&self, shim_id: &str) -> ShimFiles {
        ShimFiles::new(&self.host().config().shims_dir(), shim_id)
    }

    async fn recover(&self) -> Vec<(String, Recovered)> {
        recover_shims(
            &self.store,
            &self.host(),
            AckMode::Manual,
            Duration::from_secs(1),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|(row, outcome)| (row.shim_id, outcome))
        .collect()
    }

    async fn state(&self, shim_id: &str) -> (ShimState, Option<String>, u64) {
        let row = self.store.shim(shim_id.to_owned()).await.unwrap().unwrap();
        (row.state, row.reason, row.generation)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rfind(')')
                .map(|i| s[i + 1..].trim_start().starts_with('Z'))
        })
        == Some(false)
}

async fn gone(pid: u32) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while alive(pid) {
        assert!(tokio::time::Instant::now() < deadline, "{pid} still runs");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn line(r: &mut BufReader<tokio::io::DuplexStream>) -> String {
    let mut text = String::new();
    tokio::time::timeout(WAIT, r.read_line(&mut text))
        .await
        .unwrap()
        .unwrap();
    text
}

fn event(text: &str) -> NewEvent {
    Event::ModeChanged {
        mode: text.trim().to_owned(),
    }
    .into()
}

fn reattached(outcome: Recovered) -> Attached {
    match outcome {
        Recovered::Reattached(attached) => attached,
        other => panic!("not reattached: {other:?}"),
    }
}

#[tokio::test]
async fn a_launch_commits_the_row_the_identity_and_generation_one_before_the_attach() {
    let rig = Rig::new().await;
    let first = rig.launch(LINES).await;
    let row = rig
        .store
        .shim(first.record.shim_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, ShimState::Running);
    assert_eq!(row.generation, 1);
    assert_eq!(first.record.generation, 1);
    assert_eq!(row.provider_identity, first.record.provider_identity);
    assert_eq!(row.socket_path, first.record.socket_path);
    first.process.kill().await;
}

#[tokio::test]
async fn a_station_restart_reattaches_with_a_higher_generation_and_replays_from_the_committed_cursor_once()
 {
    let rig = Rig::new().await;
    let first = rig.launch(LINES).await;
    let id = first.record.shim_id.clone();
    let mut out = BufReader::new(first.stdout);
    let mut text = String::new();
    let mut events = Vec::new();
    for _ in 0..5 {
        let l = line(&mut out).await;
        events.push(event(&l));
        text.push_str(&l);
    }
    // The output transaction: events and cursor commit, then the ack.
    let cursor = text.len() as u64;
    let committed = rig
        .store
        .shim_commit_output(id.clone(), 1, cursor, events)
        .await
        .unwrap();
    assert!(matches!(committed, OutputCommit::Committed(e) if e.len() == 5));
    assert!(first.process.ack(cursor).await);
    // Two more lines reach the station, which dies before it commits them.
    line(&mut out).await;
    line(&mut out).await;
    drop((first.process, out));

    let mut done = rig.recover().await;
    assert_eq!(done.len(), 1);
    let (_, outcome) = done.pop().unwrap();
    let second = reattached(outcome);
    assert_eq!(second.record.generation, 2);
    assert_eq!(rig.state(&id).await, (ShimState::Running, None, 2));
    assert_eq!(second.floor, cursor);
    // The old generation commits nothing any more.
    assert_eq!(
        rig.store
            .shim_commit_output(id.clone(), 1, cursor + 100, vec![event("stale")])
            .await
            .unwrap(),
        OutputCommit::StaleGeneration
    );
    let mut out = BufReader::new(second.stdout);
    for _ in 5..30 {
        text.push_str(&line(&mut out).await);
    }
    let expected: String = (0..30).map(|i| format!("line{i}\n")).collect();
    assert_eq!(text, expected, "every line exactly once");
    second.process.kill().await;
}

#[tokio::test]
async fn a_crash_after_the_generation_commit_and_before_the_attach_skips_a_generation_and_fences_the_old_attachment()
 {
    let rig = Rig::new().await;
    let first = rig.launch(LINES).await;
    let id = first.record.shim_id.clone();
    // The previous start committed generation 2 and died before attaching.
    assert_eq!(
        rig.store.shim_next_generation(id.clone()).await.unwrap(),
        Some(2)
    );
    let second = reattached(rig.recover().await.pop().unwrap().1);
    assert_eq!(second.record.generation, 3);
    assert_eq!(rig.state(&id).await.2, 3);
    // The generation-1 attachment that still holds a socket is taken over.
    assert!(first.process.write(b"x\n").await.is_err());
    // A later attach that claims an old generation is refused by the shim.
    let mut stale = second.record.clone();
    stale.generation = 1;
    assert!(rig.host().reattach(&stale, AckMode::Manual).await.is_err());
    second.process.kill().await;
}

#[tokio::test]
async fn a_shim_that_was_killed_is_exited_with_its_exit_file_or_lost_without_one() {
    let rig = Rig::new().await;
    let with_exit = rig.launch("sleep 30").await;
    let without = rig.launch("sleep 30").await;
    let mut pids = Vec::new();
    for a in [&with_exit, &without] {
        let shim = shim_pid(&a.record.shim_id).expect("the shim process");
        kill9(shim);
        kill9(a.process.pid());
        pids.push(shim);
        pids.push(a.process.pid());
    }
    for pid in pids {
        gone(pid).await;
    }
    let exit = ExitState {
        termination: "signal:9".to_owned(),
        cause: "killed".to_owned(),
        stderr_tail: String::new(),
        descendants_remaining: Some(0),
        inspection: 1,
        final_offset: 0,
    };
    rig.files(&with_exit.record.shim_id)
        .write_exit(&exit)
        .unwrap();
    let ids = (
        with_exit.record.shim_id.clone(),
        without.record.shim_id.clone(),
    );
    let sockets = (
        with_exit.record.socket_path.clone(),
        without.record.socket_path.clone(),
    );
    drop((with_exit, without));
    for (id, outcome) in rig.recover().await {
        if id == ids.0 {
            assert!(matches!(outcome, Recovered::Exited(ref e) if e.termination == "signal:9"));
        } else {
            assert!(matches!(
                outcome,
                Recovered::Lost(reason::EXECUTION_UNKNOWN)
            ));
        }
    }
    let (state, why, _) = rig.state(&ids.0).await;
    assert_eq!((state, why.as_deref()), (ShimState::Exited, Some("exited")));
    let (state, why, _) = rig.state(&ids.1).await;
    assert_eq!(
        (state, why.as_deref()),
        (ShimState::Lost, Some("execution_unknown"))
    );
    // The stale socket files go only after the exit file was read.
    assert!(!sockets.0.exists() && !sockets.1.exists());
    assert!(rig.recover().await.is_empty(), "ended rows stay ended");
}

#[tokio::test]
async fn a_refused_attach_is_attach_refused_and_leaves_the_provider_running() {
    let rig = Rig::new().await;
    let first = rig.launch("sleep 30").await;
    let id = first.record.shim_id.clone();
    let provider = first.process.pid();
    drop(first);
    // Another station's key: the proof fails at the shim.
    rig.files(&id)
        .write_key(&AttachKey::generate().unwrap())
        .unwrap();
    let (_, outcome) = rig.recover().await.pop().unwrap();
    assert!(matches!(outcome, Recovered::Lost(reason::ATTACH_REFUSED)));
    let (state, why, _) = rig.state(&id).await;
    assert_eq!(
        (state, why.as_deref()),
        (ShimState::Lost, Some("attach_refused"))
    );
    assert!(alive(provider), "a refused attach never stops the provider");
    let shim = shim_pid(&id).unwrap();
    kill9(provider);
    kill9(shim);
}

/// Prepares a launch and commits its `starting` row, as a station that
/// crashed before it started the shim.
async fn starting(
    rig: &Rig,
    script: &str,
) -> (
    String,
    Option<fleetdeck_providers::host::shim::start::Prepared>,
) {
    let host = rig.host();
    let prepared = host.prepare(&rig.spec(script)).unwrap();
    let id = prepared.record.shim_id.clone();
    rig.store
        .shim_starting(NewShim {
            shim_id: id.clone(),
            session_id: "s1".into(),
            socket_path: prepared.record.socket_path.clone(),
            launch_boot_id: None,
            native_target: None,
        })
        .await
        .unwrap();
    (id, Some(prepared))
}

#[tokio::test]
async fn only_an_intact_journal_without_spawn_attempted_retires_a_launch_as_shim_never_started() {
    let rig = Rig::new().await;
    let marker = rig.tmp.path().join("ran");
    let script = format!("touch {}; sleep 30", marker.display());
    let (id, prepared) = starting(&rig, &script).await;
    let (_, outcome) = rig.recover().await.pop().unwrap();
    assert!(matches!(outcome, Recovered::NeverStarted));
    let (state, why, _) = rig.state(&id).await;
    assert_eq!(
        (state, why.as_deref()),
        (ShimState::NeverStarted, Some("shim_never_started"))
    );
    assert!(rig.files(&id).is_fenced());
    // The late shim of that launch sees the fence and starts no provider.
    assert!(rig.host().start(prepared.unwrap()).await.is_err());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!marker.exists(), "a fenced launch never runs the provider");
}

#[tokio::test]
async fn a_launch_interrupted_after_spawn_attempted_or_with_lost_evidence_is_never_retired_as_not_dispatched()
 {
    let rig = Rig::new().await;
    let (attempted, _) = starting(&rig, "true").await;
    rig.files(&attempted)
        .append(&Record::SpawnAttempted)
        .unwrap();
    let (missing, _) = starting(&rig, "true").await;
    std::fs::remove_file(rig.files(&missing).journal()).unwrap();
    let (truncated, _) = starting(&rig, "true").await;
    std::fs::write(rig.files(&truncated).journal(), b"{\"created\":").unwrap();
    let (foreign, _) = starting(&rig, "true").await;
    std::fs::remove_file(rig.files(&foreign).journal()).unwrap();
    rig.files(&foreign)
        .create_journal("another-incarnation", None)
        .unwrap();
    // The volatile runtime files are gone, as after a reboot.
    let done = rig.recover().await;
    assert_eq!(done.len(), 4);
    for (id, outcome) in done {
        assert!(
            matches!(outcome, Recovered::Lost(reason::EXECUTION_UNKNOWN)),
            "{id}: {outcome:?}"
        );
        let (state, why, _) = rig.state(&id).await;
        assert_eq!(
            (state, why.as_deref()),
            (ShimState::Lost, Some("execution_unknown"))
        );
    }
}

#[tokio::test]
async fn a_live_starting_shim_is_attached_and_its_first_identity_commits_before_input() {
    let rig = Rig::new().await;
    let (id, prepared) = starting(&rig, "while IFS= read -r l; do echo got:$l; done").await;
    // The shim started; the station died before it committed the identity.
    let started = rig.host().start(prepared.unwrap()).await.unwrap();
    assert_eq!(rig.state(&id).await.0, ShimState::Starting);
    let attached = reattached(rig.recover().await.pop().unwrap().1);
    let row = rig.store.shim(id.clone()).await.unwrap().unwrap();
    assert_eq!(row.state, ShimState::Running);
    assert_eq!(row.provider_identity, started.provider_identity);
    assert_eq!(attached.record.provider_identity, started.provider_identity);
    assert_eq!(row.generation, 1);
    attached.process.write(b"hi\n").await.unwrap();
    let mut out = BufReader::new(attached.stdout);
    assert_eq!(line(&mut out).await, "got:hi\n");
    attached.process.kill().await;
}

#[tokio::test]
async fn a_shim_that_cannot_start_is_settled_by_recovery_as_never_started() {
    let rig = Rig::new().await;
    let mut config = rig.host().config().clone();
    config.exe = PathBuf::from("/nonexistent/fleetdeck");
    let failed = launch_shim(
        &rig.store,
        &ShimHost::new(config),
        &"s1".into(),
        &rig.spec("true"),
        AckMode::Manual,
    )
    .await;
    let Err(LaunchError::Settled { settled, .. }) = failed else {
        panic!("not settled: {failed:?}");
    };
    assert_eq!(settled, "shim_never_started");
    assert!(rig.store.shims_to_recover().await.unwrap().is_empty());
}

/// ADR-005 test 37, station half (DL-108): the station stored an adapter
/// checkpoint of a version that this build cannot read. Recovery attaches,
/// finds the checkpoint unreadable, and ends the row `lost`
/// (`adapter_checkpoint_unsupported`) without a fresh adapter; the shim and
/// its provider stay alive for a compatible build. A readable checkpoint
/// keeps the row running.
#[tokio::test]
async fn an_unsupported_adapter_checkpoint_version_makes_the_session_not_resumable_without_a_fresh_adapter()
 {
    let rig = Rig::new().await;
    let first = rig.launch("sleep 30").await;
    let id = first.record.shim_id.clone();
    let provider = first.process.pid();
    let readable = AdapterCheckpoint {
        provider: ProviderKind::Claude,
        version: fleetdeck_providers::claude::CHECKPOINT_VERSION,
        state: serde_json::json!({}),
    };
    let commit = |checkpoint: AdapterCheckpoint, generation: u64| {
        rig.store.shim_commit_output_with(
            id.clone(),
            generation,
            0,
            Vec::new(),
            OutputMeta {
                read_at: RequestTime::Now,
                checkpoint: Some(checkpoint),
                frames: Vec::new(),
                ..OutputMeta::default()
            },
        )
    };
    assert!(matches!(
        commit(readable.clone(), 1).await.unwrap(),
        OutputCommit::Committed(_)
    ));
    drop(first);
    // A readable checkpoint: the row is reattached.
    let (_, outcome) = rig.recover().await.pop().unwrap();
    let second = reattached(outcome);
    assert_eq!(rig.state(&id).await.0, ShimState::Running);
    // A newer build stored version + 1, then this build starts again.
    let newer = AdapterCheckpoint {
        version: fleetdeck_providers::claude::CHECKPOINT_VERSION + 1,
        ..readable
    };
    assert!(matches!(
        commit(newer, second.record.generation).await.unwrap(),
        OutputCommit::Committed(_)
    ));
    drop(second);
    let (_, outcome) = rig.recover().await.pop().unwrap();
    assert!(
        matches!(outcome, Recovered::Lost(REASON_UNSUPPORTED)),
        "{outcome:?}"
    );
    let (state, why, _) = rig.state(&id).await;
    assert_eq!(
        (state, why.as_deref()),
        (ShimState::Lost, Some("adapter_checkpoint_unsupported"))
    );
    assert!(
        rig.recover().await.is_empty(),
        "the row is not recovered again"
    );
    assert!(alive(provider), "no fresh adapter and no kill");
    let shim = shim_pid(&id).unwrap();
    kill9(provider);
    kill9(shim);
}
