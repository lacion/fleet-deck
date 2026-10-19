//! Tests of the states, the recovery and `maintenance.list`.

use fleetdeck_proto::desktop_tools::{MaintenanceList, MaintenanceState, PostCheck};
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

use super::files::{self, RunStateKind};
use super::records::{now_ms, state_of};
use super::service::{Host, Unit};
use super::service_support::{Launch, REQUEST, Rig, id_of, write_spec, write_state};

const OLD: i64 = 1_700_000_000_000;

async fn state(rig: &Rig, id: &str) -> (MaintenanceState, Option<i32>, Option<PostCheck>) {
    let spec = files::read_spec(rig.dir.path(), id).unwrap();
    state_of(rig.host.as_ref(), rig.dir.path(), &spec).await
}

fn recorded(rig: &Rig, id: &str) -> Option<RunStateKind> {
    files::read_state(rig.dir.path(), id)
        .unwrap()
        .map(|s| s.state)
}

#[tokio::test]
async fn maintenance_state_exited_wins_over_a_gone_unit() {
    let rig = Rig::new(Launch::Fail);
    let id = id_of(1);
    write_spec(&rig, &id, REQUEST, OLD);
    write_state(&rig, &id, RunStateKind::Exited, Some(7));
    rig.host.set_unit(&id, Unit::Gone);
    assert_eq!(
        state(&rig, &id).await,
        (MaintenanceState::Exited, Some(7), Some(PostCheck::Ok))
    );
    assert_eq!(recorded(&rig, &id), Some(RunStateKind::Exited));
}

#[tokio::test]
async fn maintenance_state_running_with_a_gone_unit_is_lost_and_written() {
    let rig = Rig::new(Launch::Fail);
    let id = id_of(1);
    write_spec(&rig, &id, REQUEST, OLD);
    write_state(&rig, &id, RunStateKind::Running, None);
    rig.host.set_unit(&id, Unit::Gone);
    assert_eq!(state(&rig, &id).await, (MaintenanceState::Lost, None, None));
    assert_eq!(recorded(&rig, &id), Some(RunStateKind::Lost));
    // Lost stays lost, also when the unit is active again.
    rig.host.set_unit(&id, Unit::Active);
    assert_eq!(state(&rig, &id).await.0, MaintenanceState::Lost);
}

#[tokio::test]
async fn maintenance_state_running_with_an_active_or_unknown_unit_stays_running() {
    let rig = Rig::new(Launch::Fail);
    let id = id_of(1);
    write_spec(&rig, &id, REQUEST, OLD);
    write_state(&rig, &id, RunStateKind::Running, None);
    for unit in [Unit::Active, Unit::Unknown] {
        rig.host.set_unit(&id, unit);
        assert_eq!(state(&rig, &id).await.0, MaintenanceState::Running);
        assert_eq!(recorded(&rig, &id), Some(RunStateKind::Running));
    }
}

#[tokio::test]
async fn maintenance_state_no_state_file_and_an_active_unit_is_admitted() {
    let rig = Rig::new(Launch::Fail);
    let id = id_of(1);
    write_spec(&rig, &id, REQUEST, OLD);
    rig.host.set_unit(&id, Unit::Active);
    assert_eq!(state(&rig, &id).await.0, MaintenanceState::Admitted);
    assert_eq!(recorded(&rig, &id), None);
}

#[tokio::test]
async fn maintenance_state_no_state_file_a_gone_unit_and_a_spec_over_15_seconds_is_lost() {
    let rig = Rig::new(Launch::Fail);
    let old = id_of(1);
    let fresh = id_of(2);
    write_spec(&rig, &old, REQUEST, now_ms() - 20_000);
    write_spec(
        &rig,
        &fresh,
        "77777777777777777777777777777777",
        now_ms() - 5_000,
    );
    rig.host.set_unit(&old, Unit::Gone);
    rig.host.set_unit(&fresh, Unit::Gone);
    assert_eq!(state(&rig, &old).await.0, MaintenanceState::Lost);
    assert_eq!(recorded(&rig, &old), Some(RunStateKind::Lost));
    // A young spec has time to start.
    assert_eq!(state(&rig, &fresh).await.0, MaintenanceState::Admitted);
    assert_eq!(recorded(&rig, &fresh), None);
}

#[tokio::test]
async fn maintenance_state_an_unknown_unit_never_makes_a_run_lost() {
    let rig = Rig::new(Launch::Fail);
    let none = id_of(1);
    let running = id_of(2);
    write_spec(&rig, &none, REQUEST, OLD);
    write_spec(&rig, &running, "77777777777777777777777777777777", OLD);
    write_state(&rig, &running, RunStateKind::Running, None);
    rig.host.set_unit_default(Unit::Unknown);
    assert_eq!(state(&rig, &none).await.0, MaintenanceState::Admitted);
    assert_eq!(state(&rig, &running).await.0, MaintenanceState::Running);
    assert_eq!(recorded(&rig, &none), None);
    assert_eq!(recorded(&rig, &running), Some(RunStateKind::Running));
}

#[tokio::test]
async fn maintenance_state_a_state_file_that_fails_its_checks_counts_as_no_state() {
    let rig = Rig::new(Launch::Fail);
    let id = id_of(1);
    write_spec(&rig, &id, REQUEST, now_ms());
    std::fs::write(files::state_path(rig.dir.path(), &id), "not json").unwrap();
    std::fs::set_permissions(
        files::state_path(rig.dir.path(), &id),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(state(&rig, &id).await.0, MaintenanceState::Admitted);
}

/// A host whose answer `Gone` comes after the runner wrote its exit.
struct RaceHost {
    dir: std::path::PathBuf,
}

impl Host for RaceHost {
    fn launch(
        &self,
        _argv: Vec<std::ffi::OsString>,
        _env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> futures_util::future::BoxFuture<'_, std::io::Result<i32>> {
        Box::pin(async { Err(std::io::Error::other("no launch")) })
    }

    fn unit(&self, id: &str) -> futures_util::future::BoxFuture<'_, Unit> {
        let state = files::RunState {
            v: 1,
            state: RunStateKind::Exited,
            exit_code: Some(0),
            post_check: Some(PostCheck::TransactionChanged),
            at: 1,
        };
        files::write_state(&self.dir, id, &state).unwrap();
        Box::pin(async { Unit::Gone })
    }
}

#[tokio::test]
async fn maintenance_state_an_exit_written_before_the_unit_went_is_not_overwritten_by_lost() {
    let rig = Rig::new(Launch::Fail);
    let id = id_of(1);
    let spec = write_spec(&rig, &id, REQUEST, OLD);
    write_state(&rig, &id, RunStateKind::Running, None);
    let host = RaceHost {
        dir: rig.dir.path().to_owned(),
    };
    let got = state_of(&host, rig.dir.path(), &spec).await;
    assert_eq!(
        got,
        (
            MaintenanceState::Exited,
            Some(0),
            Some(PostCheck::TransactionChanged)
        )
    );
    assert_eq!(recorded(&rig, &id), Some(RunStateKind::Exited));
}

async fn list(rig: &Rig, params: Value) -> Outcome {
    rig.svc
        .call(
            "maintenance.list",
            params,
            super::service_support::peer(1),
            true,
        )
        .await
}

fn runs_of(outcome: Outcome) -> MaintenanceList {
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).unwrap(),
        Outcome::Err(e) => panic!("{e}"),
    }
}

fn request_of(n: u32) -> String {
    format!("{:032x}", 0x1000 + n)
}

/// 18 finished runs: the ID `n` was created at `OLD + n`.
fn finished_runs(rig: &Rig, count: u32) {
    for n in 1..=count {
        let id = id_of(n);
        write_spec(rig, &id, &request_of(n), OLD + i64::from(n));
        write_state(rig, &id, RunStateKind::Exited, Some(0));
        files::write_output(rig.dir.path(), &id, b"output").unwrap();
        std::fs::write(files::lock_path(rig.dir.path(), &id), "").unwrap();
    }
}

#[tokio::test]
async fn maintenance_list_is_newest_first_and_takes_null_or_an_empty_object() {
    let rig = Rig::new(Launch::Fail);
    finished_runs(&rig, 3);
    for params in [Value::Null, json!({})] {
        let runs = runs_of(list(&rig, params).await).runs;
        let ids: Vec<_> = runs.iter().map(|r| r.maintenance_id.clone()).collect();
        assert_eq!(ids, [id_of(3), id_of(2), id_of(1)]);
        assert_eq!(runs[0].state, MaintenanceState::Exited);
        assert_eq!(runs[0].exit_code, Some(0));
        assert_eq!(runs[0].post_check, Some(PostCheck::Ok));
        assert_eq!(runs[0].tools, ["wtype"]);
        assert_eq!(runs[0].created_at, OLD + 3);
    }
    let bad = list(&rig, json!({"x": 1})).await;
    assert!(
        matches!(&bad, Outcome::Err(e) if e.code == "invalid_request"),
        "{bad:?}"
    );
}

#[tokio::test]
async fn maintenance_list_answers_16_and_prunes_older_finished_runs_with_their_files() {
    let rig = Rig::new(Launch::Fail);
    finished_runs(&rig, 18);
    let runs = runs_of(list(&rig, json!({})).await).runs;
    assert_eq!(runs.len(), 16);
    assert_eq!(runs[0].maintenance_id, id_of(18));
    assert_eq!(runs[15].maintenance_id, id_of(3));
    let dir = rig.dir.path();
    for n in [1, 2] {
        let id = id_of(n);
        assert!(!files::spec_path(dir, &id).exists());
        assert!(!files::state_path(dir, &id).exists());
        assert!(!files::output_path(dir, &id).exists());
        assert!(!files::lock_path(dir, &id).exists());
    }
    for n in [3, 18] {
        let id = id_of(n);
        assert!(files::spec_path(dir, &id).exists());
        assert!(files::state_path(dir, &id).exists());
        assert!(files::output_path(dir, &id).exists());
        assert!(files::lock_path(dir, &id).exists());
    }
}

#[tokio::test]
async fn maintenance_list_never_prunes_a_running_run() {
    let rig = Rig::new(Launch::Fail);
    finished_runs(&rig, 18);
    // The oldest run is still running (the unit is active) and has a socket.
    write_state(&rig, &id_of(1), RunStateKind::Running, None);
    std::fs::remove_file(files::output_path(rig.dir.path(), &id_of(1))).unwrap();
    let sock = files::socket_path(rig.dir.path(), &id_of(1));
    std::fs::write(&sock, "").unwrap();
    // The oldest run with no state yet (admitted) is kept too.
    std::fs::remove_file(files::state_path(rig.dir.path(), &id_of(2))).unwrap();
    let runs = runs_of(list(&rig, json!({})).await).runs;
    assert_eq!(runs.len(), 16);
    let dir = rig.dir.path();
    assert!(files::spec_path(dir, &id_of(1)).exists());
    assert!(files::state_path(dir, &id_of(1)).exists());
    assert!(files::spec_path(dir, &id_of(2)).exists());
    assert!(sock.exists());
    let again = runs_of(list(&rig, json!({})).await).runs;
    assert_eq!(again.len(), 16);
}

#[tokio::test]
async fn maintenance_list_skips_a_spec_that_is_not_valid() {
    let rig = Rig::new(Launch::Fail);
    finished_runs(&rig, 1);
    let bad = files::spec_path(rig.dir.path(), &id_of(9));
    std::fs::write(&bad, "{}").unwrap();
    std::fs::set_permissions(&bad, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let runs = runs_of(list(&rig, json!({})).await).runs;
    assert_eq!(runs.len(), 1);
}
