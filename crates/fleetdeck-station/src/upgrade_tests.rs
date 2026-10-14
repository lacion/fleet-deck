//! Station self-upgrade (M3 7-3, DL-128; ADR-005 §3.3, §5.3, tests 20, 21,
//! 22 and 48).

use std::path::{Path, PathBuf};

use fleetdeck_proto::event::{Event, ProviderKind};
use fleetdeck_providers::checkpoint::AdapterCheckpoint;
use fleetdeck_providers::host::shim::wire::{self, ProviderIdentity};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::*;
use crate::store::{
    NewSession, NewShim, OfflineApprovalPolicy, OutputCommit, OutputMeta, ReadResult, RequestTime,
    ShimState, Store, StoreConfig,
};

const SHIM: &str = "shim-1";

struct Rig {
    det: Det,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            det: Det::new(),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("station.db")
    }

    fn upgrades(&self) -> PathBuf {
        self.dir.path().join("upgrades")
    }

    async fn open(&self) -> Store {
        Store::open(
            &self.db(),
            self.det.clock(),
            self.det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap()
    }

    /// A station database with one session and one running shim.
    async fn with_running_shim(&self) -> Store {
        let s = self.open().await;
        s.recover(Vec::new()).await.unwrap();
        s.create_session(NewSession {
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
        s.shim_starting(NewShim {
            shim_id: SHIM.to_owned(),
            session_id: "s1".into(),
            socket_path: PathBuf::from("/run/x/shim-1.sock"),
            launch_boot_id: None,
            native_target: None,
        })
        .await
        .unwrap();
        assert!(
            s.shim_running(
                SHIM.to_owned(),
                ProviderIdentity {
                    pid: 42,
                    start_time: 7,
                    boot_id: "boot".to_owned(),
                },
            )
            .await
            .unwrap()
        );
        s
    }

    fn record(&self, state: UpdateState) -> UpdateRecord {
        let record = UpdateRecord {
            update_id: "upd-1".to_owned(),
            from_version: "2.0.0".to_owned(),
            to_version: "2.1.0".to_owned(),
            state,
            live_advanced: false,
            snapshot: self.dir.path().join("snap"),
        };
        record.save(&self.upgrades()).unwrap();
        record
    }

    fn launcher(&self) -> (Launcher, PathBuf, PathBuf) {
        let launcher = Launcher::new(self.dir.path().join("bin"));
        let binary = self.dir.path().join("fleetdeck-build");
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        let old = launcher.install(&binary, "2.0.0", "upd-0").unwrap();
        let new = launcher.install(&binary, "2.1.0", "upd-1").unwrap();
        (launcher, old, new)
    }
}

async fn events(s: &Store) -> usize {
    match s.read_after(0, 10_000).await.unwrap() {
        ReadResult::Events { events, .. } => events.len(),
        ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
    }
}

fn checkpoint(n: u64) -> AdapterCheckpoint {
    AdapterCheckpoint {
        provider: ProviderKind::Claude,
        version: 1,
        state: json!({ "n": n }),
    }
}

fn set_protocol(db: &Path, protocol: u32) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute("UPDATE shims SET protocol = ?1", [protocol])
        .unwrap();
}

// ADR-005 test 20.
#[test]
fn a_station_of_the_previous_build_attaches_to_a_shim_of_the_new_build() {
    // Build N speaks protocols {N−1, N}; build N−1 speaks {N−1}.
    let previous: &[u32] = wire::PROTOCOLS;
    let next: Vec<u32> = wire::PROTOCOLS
        .iter()
        .copied()
        .chain([wire::PROTOCOL + 1])
        .collect();
    assert_eq!(wire::pick_common(previous, &next), Some(wire::PROTOCOL));
    assert_eq!(wire::pick_common(&next, previous), Some(wire::PROTOCOL));
    // Two builds without a shared version never attach.
    assert_eq!(wire::pick_common(&[wire::PROTOCOL + 1], previous), None);
}

// ADR-005 test 21.
#[tokio::test]
async fn an_upgrade_that_cannot_speak_a_running_shim_protocol_is_not_activated() {
    let rig = Rig::new();
    drop(rig.with_running_shim().await);
    set_protocol(&rig.db(), wire::PROTOCOL);
    let snap = rig.dir.path().join("snap");
    let files = snapshot(&rig.db(), &snap).unwrap();
    assert!(!files.is_empty());
    let mut record = rig.record(UpdateState::Prepared);
    let (launcher, old, new) = rig.launcher();
    launcher.point(&old).unwrap();

    // A new build that dropped the running shim's protocol.
    let work = tempfile::tempdir().unwrap();
    let err = trial(&snap.join("station.db"), work.path(), &[wire::PROTOCOL + 1]).unwrap_err();
    assert!(
        matches!(&err, UpgradeError::ShimIncompatible { shim_id, protocol, .. }
            if shim_id == SHIM && *protocol == wire::PROTOCOL),
        "{err}"
    );
    record.state = UpdateState::Failed;
    record.save(&rig.upgrades()).unwrap();
    assert!(matches!(
        launcher.activate(&record, &new),
        Err(UpgradeError::NotTrialed(_))
    ));
    // The old build keeps running: the launcher and the live row are as before.
    assert_eq!(launcher.current(), Some(old));
    let s = rig.open().await;
    assert_eq!(
        s.shim(SHIM.to_owned()).await.unwrap().unwrap().state,
        ShimState::Running
    );

    // A build that speaks the protocol passes the trial on a copy.
    let work = tempfile::tempdir().unwrap();
    let report = trial(&snap.join("station.db"), work.path(), wire::PROTOCOLS).unwrap();
    assert_eq!(report.shims, vec![(SHIM.to_owned(), wire::PROTOCOL)]);
    assert!(report.schema_version >= 15);
}

// ADR-005 test 22.
#[tokio::test]
async fn a_historical_restore_is_refused_while_a_shim_is_live_and_a_permitted_one_gets_a_new_incarnation()
 {
    let rig = Rig::new();
    let s = rig.with_running_shim().await;
    let before = s.incarnation().as_str().to_owned();
    drop(s);
    let snap = rig.dir.path().join("snap");
    snapshot(&rig.db(), &snap).unwrap();
    let record = rig.record(UpdateState::Activated);

    // A live shim refuses the historical restore: binary only.
    assert_eq!(live_shims(&rig.db()).unwrap(), 1);
    assert_eq!(
        rollback_plan(&record, live_shims(&rig.db()).unwrap()),
        RollbackPlan::BinaryOnly {
            reason: "shim_live"
        }
    );

    // The shim ends; nothing advanced: the restore is permitted.
    let s = rig.open().await;
    assert!(
        s.shim_end(SHIM.to_owned(), ShimState::Exited, "exited".to_owned(), 1)
            .await
            .unwrap()
    );
    drop(s);
    assert_eq!(live_shims(&rig.db()).unwrap(), 0);
    assert_eq!(rollback_plan(&record, 0), RollbackPlan::RestoreSnapshot);
    restore(&snap, &rig.db(), "inc-restored").unwrap();
    let s = rig.open().await;
    assert_eq!(s.incarnation().as_str(), "inc-restored");
    assert_ne!(s.incarnation().as_str(), before);
}

// ADR-005 test 48.
#[tokio::test]
async fn a_failed_activation_after_an_output_commit_and_ack_keeps_events_checkpoint_cursor_and_generation()
 {
    let rig = Rig::new();
    drop(rig.with_running_shim().await);
    let snap = rig.dir.path().join("snap");
    snapshot(&rig.db(), &snap).unwrap();
    let (launcher, old, new) = rig.launcher();
    launcher.point(&old).unwrap();
    let mut record = rig.record(UpdateState::Trialed);
    launcher.activate(&record, &new).unwrap();
    record.state = UpdateState::Activated;
    record.save(&rig.upgrades()).unwrap();

    // The new station marks the update live, then commits output and acks.
    assert_eq!(mark_live(&rig.upgrades()).unwrap(), 1);
    let s = rig.open().await;
    s.recover(Vec::new()).await.unwrap();
    let generation = s
        .shim_next_generation(SHIM.to_owned())
        .await
        .unwrap()
        .unwrap();
    let committed = s
        .shim_commit_output_with(
            SHIM.to_owned(),
            generation,
            12,
            vec![
                Event::ModeChanged {
                    mode: "plan".to_owned(),
                }
                .into(),
            ],
            OutputMeta {
                read_at: RequestTime::Now,
                checkpoint: Some(checkpoint(12)),
                frames: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(committed, OutputCommit::Committed(_)));
    let events_before = events(&s).await;
    let incarnation = s.incarnation().as_str().to_owned();
    drop(s);

    // The activation fails afterwards: the rollback is binary-only.
    let record = UpdateRecord::load(&rig.upgrades(), "upd-1").unwrap();
    assert!(record.live_advanced);
    let plan = rollback_plan(&record, live_shims(&rig.db()).unwrap());
    assert_eq!(
        plan,
        RollbackPlan::BinaryOnly {
            reason: "live_advanced"
        }
    );
    launcher.roll_back().unwrap();
    assert_eq!(launcher.current(), Some(old));

    // Events, checkpoint, cursor and generation are kept.
    let s = rig.open().await;
    assert_eq!(s.incarnation().as_str(), incarnation);
    assert_eq!(events(&s).await, events_before);
    let row = s.shim(SHIM.to_owned()).await.unwrap().unwrap();
    assert_eq!((row.generation, row.cursor), (generation, 12));
    let stored = s.shim_checkpoint(SHIM.to_owned()).await.unwrap().unwrap();
    assert_eq!(
        AdapterCheckpoint::from_json(&stored).unwrap(),
        checkpoint(12)
    );
}

#[test]
fn a_snapshot_copies_the_main_wal_and_shm_files_and_needs_the_main_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("station.db");
    assert!(snapshot(&db, &dir.path().join("a")).is_err());
    std::fs::write(&db, b"main").unwrap();
    std::fs::write(dir.path().join("station.db-wal"), b"wal").unwrap();
    std::fs::write(dir.path().join("station.db-shm"), b"shm").unwrap();
    let files = snapshot(&db, &dir.path().join("b")).unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(
        std::fs::read(dir.path().join("b/station.db-wal")).unwrap(),
        b"wal"
    );
}
