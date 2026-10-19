//! Helpers of the tests of the station side: a fake `Host` (the launch can
//! start the runner core of BG1-3a with a `/bin/sh` child), fake plans, and
//! a rig with a private folder.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::files::{self, RunState, RunStateKind, Spec};
use super::runner::{ChildPlan, serve};
use super::service::{Host, MaintenanceService, Plans, Unit};
use super::test_support::{POST_OK, package, postcheck_script, private_dir, spec};
use crate::desktop_tools::StoredPlan;

pub(super) const PLAN: &str = "44444444444444444444444444444444";
pub(super) const REQUEST: &str = "33333333333333333333333333333333";
pub(super) const OTHER_PLAN: &str = "55555555555555555555555555555555";
pub(super) const OTHER_REQUEST: &str = "66666666666666666666666666666666";
pub(super) const WAIT: Duration = Duration::from_secs(30);

/// What the fake launch does.
#[derive(Clone)]
pub(super) enum Launch {
    /// An error.
    Fail,
    /// A normal return with this exit code, and no runner.
    Code(i32),
    /// Starts the runner core with `/bin/sh -c <script>`.
    Runner(String),
}

pub(super) struct FakeHost {
    pub launches: Mutex<Vec<Vec<OsString>>>,
    pub envs: Mutex<Vec<Vec<(OsString, OsString)>>>,
    launch: Mutex<Launch>,
    units: Mutex<HashMap<String, Unit>>,
    unit_default: Mutex<Unit>,
    runs: Mutex<Vec<JoinHandle<i32>>>,
    post: TempDir,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeHost {
    pub(super) fn new(launch: Launch) -> Arc<Self> {
        Arc::new(Self {
            launches: Mutex::new(Vec::new()),
            envs: Mutex::new(Vec::new()),
            launch: Mutex::new(launch),
            units: Mutex::new(HashMap::new()),
            unit_default: Mutex::new(Unit::Active),
            runs: Mutex::new(Vec::new()),
            post: tempfile::tempdir().unwrap(),
        })
    }

    pub(super) fn set_unit(&self, id: &str, unit: Unit) {
        lock(&self.units).insert(id.to_owned(), unit);
    }

    pub(super) fn set_unit_default(&self, unit: Unit) {
        *lock(&self.unit_default) = unit;
    }

    pub(super) fn launch_count(&self) -> usize {
        lock(&self.launches).len()
    }

    /// Waits for every runner that the fake launch started.
    pub(super) async fn finished(&self) -> Vec<i32> {
        let runs: Vec<_> = std::mem::take(&mut *lock(&self.runs));
        let mut codes = Vec::new();
        for run in runs {
            codes.push(timeout(WAIT, run).await.unwrap().unwrap());
        }
        codes
    }

    fn value_of(argv: &[OsString], flag: &str) -> PathBuf {
        let at = argv.iter().position(|a| a == flag).unwrap();
        PathBuf::from(&argv[at + 1])
    }
}

impl Host for FakeHost {
    fn launch(
        &self,
        argv: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
    ) -> BoxFuture<'_, io::Result<i32>> {
        lock(&self.launches).push(argv.clone());
        lock(&self.envs).push(env);
        let mode = lock(&self.launch).clone();
        Box::pin(async move {
            match mode {
                Launch::Fail => Err(io::Error::other("the fake launch failed")),
                Launch::Code(code) => Ok(code),
                Launch::Runner(script) => {
                    let id = Self::value_of(&argv, "--id");
                    let dir = Self::value_of(&argv, "--dir");
                    let id = id.to_str().unwrap().to_owned();
                    let spec = files::read_spec(&dir, &id).unwrap();
                    let plan = ChildPlan {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), script.into()],
                        postcheck_program: postcheck_script(self.post.path(), POST_OK),
                    };
                    let run = tokio::spawn(async move { serve(&dir, spec, plan).await });
                    lock(&self.runs).push(run);
                    Ok(0)
                }
            }
        })
    }

    fn unit(&self, id: &str) -> BoxFuture<'_, Unit> {
        let unit = lock(&self.units)
            .get(id)
            .copied()
            .unwrap_or(*lock(&self.unit_default));
        Box::pin(async move { unit })
    }
}

pub(super) struct FakePlans {
    plans: Mutex<Vec<StoredPlan>>,
    pub recheck: Mutex<Result<(), &'static str>>,
    pub removed: Mutex<Vec<String>>,
}

impl FakePlans {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            plans: Mutex::new(Vec::new()),
            recheck: Mutex::new(Ok(())),
            removed: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn add(&self, plan: StoredPlan) {
        lock(&self.plans).push(plan);
    }
}

impl Plans for FakePlans {
    fn plan(&self, plan_id: &str, peer: &Fingerprint, _now_ms: i64) -> Option<StoredPlan> {
        lock(&self.plans)
            .iter()
            .find(|p| p.plan_id == plan_id && p.peer.matches(peer))
            .cloned()
    }

    fn recheck<'a>(&'a self, _plan: &'a StoredPlan) -> BoxFuture<'a, Result<(), &'static str>> {
        let result = *lock(&self.recheck);
        Box::pin(async move { result })
    }

    fn remove(&self, plan_id: &str) {
        lock(&self.plans).retain(|p| p.plan_id != plan_id);
        lock(&self.removed).push(plan_id.to_owned());
    }
}

/// The deck link of the tests: the fingerprint of the key `byte` repeated.
pub(super) fn peer(byte: u8) -> Fingerprint {
    format!("{byte:02x}").repeat(32).parse().unwrap()
}

pub(super) fn stored_plan(plan_id: &str, who: &Fingerprint) -> StoredPlan {
    StoredPlan {
        plan_id: plan_id.to_owned(),
        peer: *who,
        tools: vec!["wtype".to_owned()],
        targets: vec!["extra/wtype".to_owned()],
        transaction: vec![package("wtype", "1.0-1")],
        conf_sha256: "a".repeat(64),
        expires_at_ms: i64::MAX,
    }
}

pub(super) fn open_params(request: &str, plan: &str) -> Value {
    json!({"plan_id": plan, "request_id": request, "cols": 100, "rows": 30})
}

/// A service on a private folder with fake processes and plans.
pub(super) struct Rig {
    pub dir: TempDir,
    pub svc: Arc<MaintenanceService>,
    pub host: Arc<FakeHost>,
    pub plans: Arc<FakePlans>,
}

impl Rig {
    pub(super) fn new(launch: Launch) -> Self {
        let dir = private_dir();
        let host = FakeHost::new(launch);
        let plans = FakePlans::new();
        let mut svc = MaintenanceService::new(
            dir.path().to_owned(),
            PathBuf::from("/opt/fleetdeck/bin/current"),
            host.clone(),
            plans.clone(),
        );
        // A launch that never starts a runner must not hold a test for 10 s.
        svc.settle = Duration::from_millis(300);
        Self {
            dir,
            svc: Arc::new(svc),
            host,
            plans,
        }
    }

    /// A rig with the plan `PLAN` of the deck `peer(1)`.
    pub(super) fn with_plan(launch: Launch) -> Self {
        let rig = Self::new(launch);
        rig.plans.add(stored_plan(PLAN, &peer(1)));
        rig
    }

    pub(super) async fn open(&self, request: &str, plan: &str, allow_shell: bool) -> Value {
        match self
            .svc
            .call(
                "maintenance.open",
                open_params(request, plan),
                peer(1),
                allow_shell,
            )
            .await
        {
            fleetdeck_proto::link::Outcome::Ok(v) => v,
            fleetdeck_proto::link::Outcome::Err(e) => {
                json!({"error": e.code, "text": e.message})
            }
        }
    }

    pub(super) fn specs(&self) -> Vec<Spec> {
        super::records::list_specs(self.dir.path()).unwrap()
    }
}

/// Writes a spec with the given ID, request and creation time.
pub(super) fn write_spec(rig: &Rig, id: &str, request: &str, created_at: i64) -> Spec {
    let mut s = spec(id);
    s.request_id = request.to_owned();
    s.deck = peer(1).to_string();
    s.created_at = created_at;
    files::write_spec(rig.dir.path(), &s).unwrap();
    s
}

pub(super) fn write_state(rig: &Rig, id: &str, kind: RunStateKind, code: Option<i32>) {
    let state = RunState {
        v: 1,
        state: kind,
        exit_code: code,
        post_check: code.map(|_| fleetdeck_proto::desktop_tools::PostCheck::Ok),
        at: 1,
    };
    files::write_state(rig.dir.path(), id, &state).unwrap();
}

/// A maintenance ID from a number.
pub(super) fn id_of(n: u32) -> String {
    format!("{n:032x}")
}
