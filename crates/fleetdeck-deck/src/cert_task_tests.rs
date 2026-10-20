#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::fs;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicUsize;

use fleetdeck_core::clock::{Clock, ManualClock};
use tokio::sync::Notify;

use super::*;
use crate::http::tls::BoardTls;
use crate::http::tls_tailscale::{TAILSCALE_PEM, isrg_anchors, write_tailscale};
use crate::http::tls_test_pki::{NAME, Pki};
use crate::live::DeckLive;

const SLOW: Intervals = Intervals {
    normal: Duration::from_secs(30),
    retry: Duration::from_millis(40),
    near_end: Duration::from_secs(14 * 24 * 60 * 60),
};
const FAST: Intervals = Intervals {
    normal: Duration::from_millis(40),
    ..SLOW
};

/// 2026-10-15 as Unix ms.
const NOW_MS: u64 = 1_792_022_400_000;

struct Fake {
    name: StdMutex<String>,
    script: StdMutex<VecDeque<Result<Vec<u8>, CertError>>>,
    issued: AtomicUsize,
    gate: Option<Arc<Notify>>,
}

impl Fake {
    fn new(name: &str, script: Vec<Result<Vec<u8>, CertError>>, gate: bool) -> Arc<Self> {
        Arc::new(Self {
            name: StdMutex::new(name.to_owned()),
            script: StdMutex::new(script.into()),
            issued: AtomicUsize::new(0),
            gate: gate.then(|| Arc::new(Notify::new())),
        })
    }

    fn issued(&self) -> usize {
        self.issued.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Issuer for Fake {
    async fn issue(&self, _domain: &str) -> Result<Vec<u8>, CertError> {
        self.issued.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(CertError::Failed))
    }

    async fn self_name(&self) -> Result<String, CertError> {
        Ok(self.name.lock().unwrap().clone())
    }
}

struct Fx {
    pki: Pki,
    dir: tempfile::TempDir,
    clock: ManualClock,
    live: LiveCell,
    resolver: Arc<BoardResolver>,
    stop: watch::Sender<bool>,
    intervals: Intervals,
}

fn live(on: bool) -> DeckLive {
    DeckLive {
        name: "d".to_owned(),
        conflict_mail: true,
        fleet_control: false,
        desktop_control: false,
        tailscale_cert: on,
        tailscale_bin: None,
        knobs: crate::knobs::DeckKnobs::default(),
        worker_grant: fleetdeck_core::config_worker_grant::WorkerGrantConfig {
            enabled: false,
            ..fleetdeck_core::config_worker_grant::WorkerGrantConfig::default()
        },
    }
}

impl Fx {
    fn new(on: bool, owner: bool) -> Self {
        let clock = ManualClock::new(NOW_MS);
        let (c, k) = BoardTls::generate(&["127.0.0.1".to_owned()]).unwrap();
        let fallback = BoardTls::from_pem(&c, &k, true)
            .unwrap()
            .resolver
            .fallback();
        let resolver = BoardResolver::new(fallback, owner, Some(NAME.to_owned()))
            .with_clock(Arc::new(clock.clone()));
        Self {
            pki: Pki::new(),
            dir: tempfile::tempdir().unwrap(),
            clock,
            live: LiveCell::new(live(on)),
            resolver: Arc::new(resolver),
            stop: watch::channel(false).0,
            intervals: SLOW,
        }
    }

    fn fast(mut self) -> Self {
        self.intervals = FAST;
        self
    }

    fn pem(&self) -> Vec<u8> {
        self.pki.check(Duration::ZERO).unwrap().to_pem().unwrap()
    }

    fn deps(&self, host: Option<&str>, issuer: Arc<Fake>) -> CertDeps {
        CertDeps {
            live: self.live.clone(),
            resolver: Arc::clone(&self.resolver),
            dir: self.dir.path().to_path_buf(),
            hostname: host.map(str::to_owned),
            issuer,
            clock: Arc::new(self.clock.clone()),
            anchors: Arc::from(self.pki.anchors()),
            intervals: self.intervals,
            state: Arc::new(RwLock::new(CertState::off())),
        }
    }

    fn start(&self, issuer: &Arc<Fake>) -> CertTask {
        spawn(
            self.deps(Some(NAME), Arc::clone(issuer)),
            self.stop.subscribe(),
        )
    }

    fn served(&self) -> bool {
        let now = self.clock.now_ms().try_into().unwrap();
        let pair = self.resolver.pick(Some(NAME), now);
        pair.cert[0] == self.pki.leaf.der
    }

    fn file(&self) -> bool {
        self.dir.path().join(TAILSCALE_PEM).exists()
    }
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..600 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("not reached: {what}");
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn the_setting_is_off_by_default_and_nothing_is_issued() {
    let fx = Fx::new(false, false);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], false);
    let task = fx.start(&fake);
    settle().await;
    assert_eq!(fake.issued(), 0);
    assert_eq!(task.state().state, CertStateKind::Off);
    assert!(!task.state().enabled);
    assert!(!fx.served() && !fx.file());
}

#[tokio::test]
async fn on_issues_stores_and_serves() {
    let fx = Fx::new(true, false);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], false);
    let task = fx.start(&fake);
    until("valid", || task.state().state == CertStateKind::Valid).await;
    let s = task.state();
    assert!(s.enabled && s.reason_code.is_none());
    assert_eq!(s.name.as_deref(), Some(NAME));
    assert_eq!(s.not_after_ms, Some(1_798_761_600_000));
    assert!(fx.served() && fx.file());
}

#[tokio::test]
async fn a_name_that_differs_from_the_status_name_is_not_issued() {
    let fx = Fx::new(true, false);
    let fake = Fake::new("other.tail1.ts.net.", vec![Ok(fx.pem())], false);
    let task = fx.start(&fake);
    until("mismatch", || {
        task.state().state == CertStateKind::NameMismatch
    })
    .await;
    assert_eq!(fake.issued(), 0);
    assert!(!fx.served() && !fx.file());
    // The status name with a trailing dot and capitals is the same name.
    let same = Fake::new("Deck.Tail1.TS.net.", vec![Ok(fx.pem())], false);
    let task = fx.start(&same);
    until("valid", || task.state().state == CertStateKind::Valid).await;
    assert_eq!(same.issued(), 1);
}

#[tokio::test]
async fn no_host_name_or_a_name_outside_ts_net_is_a_mismatch() {
    for host in [None, Some("deck.example.com")] {
        let fx = Fx::new(true, false);
        let fake = Fake::new(NAME, vec![Ok(fx.pem())], false);
        let task = spawn(fx.deps(host, Arc::clone(&fake)), fx.stop.subscribe());
        until("mismatch", || {
            task.state().state == CertStateKind::NameMismatch
        })
        .await;
        assert_eq!(fake.issued(), 0);
    }
}

#[tokio::test]
async fn off_during_a_pending_issue_stores_and_serves_nothing() {
    let fx = Fx::new(true, false);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], true);
    let task = fx.start(&fake);
    until("the call", || fake.issued() == 1).await;
    fx.live.publish(live(false));
    until("off", || task.state().state == CertStateKind::Off).await;
    fake.gate.as_ref().unwrap().notify_one();
    settle().await;
    assert!(!fx.served() && !fx.file());
    assert_eq!(task.state().state, CertStateKind::Off);
    assert_eq!(fake.issued(), 1);
}

#[tokio::test]
async fn a_stale_run_drops_its_result() {
    let fx = Fx::new(false, false);
    let fake = Fake::new(NAME, vec![], false);
    let core = Arc::new(Core {
        state: Arc::new(RwLock::new(CertState::off())),
        generation: AtomicU64::new(0),
        served: Mutex::new(None),
        deps: fx.deps(Some(NAME), fake),
    });
    let cert = fx.pki.check(Duration::ZERO).unwrap();
    core.generation.fetch_add(1, Ordering::SeqCst);
    assert!(matches!(core.commit(0, &cert).await, Commit::Stale));
    assert!(!fx.served() && !fx.file());
    assert!(matches!(core.commit(1, &cert).await, Commit::Done));
    assert!(fx.served() && fx.file());
}

#[tokio::test]
async fn a_failure_keeps_the_previous_certificate_and_sets_the_reason() {
    let fx = Fx::new(true, false).fast();
    let fake = Fake::new(
        NAME,
        vec![Ok(fx.pem()), Err(CertError::HttpsDisabled)],
        false,
    );
    let task = fx.start(&fake);
    until("valid", || task.state().state == CertStateKind::Valid).await;
    until("reason", || task.state().reason_code.is_some()).await;
    let s = task.state();
    assert_eq!(s.reason_code, Some("https_disabled"));
    assert!(matches!(
        s.state,
        CertStateKind::Valid | CertStateKind::Renewing
    ));
    assert!(fx.served() && fx.file());
    // A bad output is refused the same way, with its own code.
    let bad = Fake::new(NAME, vec![Ok(b"junk".to_vec())], false);
    let fx2 = Fx::new(true, false);
    let task = fx2.start(&bad);
    until("failed", || task.state().state == CertStateKind::Failed).await;
    assert_eq!(task.state().reason_code, Some("bad_output"));
    assert!(!fx2.served() && !fx2.file());
}

#[tokio::test]
async fn a_certificate_that_reaches_its_end_is_expired_and_not_served() {
    let fx = Fx::new(true, false).fast();
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], false);
    let task = fx.start(&fake);
    until("valid", || task.state().state == CertStateKind::Valid).await;
    fx.clock.advance(Duration::from_secs(100 * 24 * 60 * 60));
    assert!(!fx.served());
    until("expired", || task.state().state == CertStateKind::Expired).await;
}

#[test]
fn the_wait_follows_the_time_left() {
    let iv = Intervals::PRODUCTION;
    let day = Duration::from_secs(24 * 60 * 60);
    let hour = Duration::from_secs(60 * 60);
    let at = |failed, left| next_wait(failed, left, &iv);
    assert_eq!(at(false, Some(60 * day)), 12 * hour);
    assert_eq!(at(false, None), 12 * hour);
    assert_eq!(at(true, Some(60 * day)), hour);
    assert_eq!(at(true, None), hour);
    assert_eq!(at(false, Some(13 * day)), hour);
    assert_eq!(at(false, Some(14 * day)), 12 * hour);
    // The loop wakes when the certificate ends.
    assert_eq!(
        at(false, Some(Duration::from_secs(60))),
        Duration::from_secs(60) + Duration::from_millis(1)
    );
    // A certificate that is over does not make a busy loop.
    assert_eq!(at(false, Some(Duration::ZERO)), hour);
}

#[tokio::test]
async fn a_stored_valid_file_is_served_before_any_issue_call() {
    let fx = Fx::new(true, false);
    write_tailscale(fx.dir.path(), &fx.pki.check(Duration::ZERO).unwrap()).unwrap();
    let fake = Fake::new(NAME, vec![], true);
    let task = fx.start(&fake);
    // No await yet: the task did not run.
    assert!(fx.served());
    assert_eq!(fake.issued(), 0);
    assert_eq!(task.state().state, CertStateKind::Valid);
    until("the call", || fake.issued() == 1).await;
    assert!(fx.served());
}

#[tokio::test]
async fn a_stored_expired_file_is_not_served_and_the_state_is_expired() {
    let fx = Fx::new(true, false);
    write_tailscale(fx.dir.path(), &fx.pki.check(Duration::ZERO).unwrap()).unwrap();
    fx.clock.advance(Duration::from_secs(100 * 24 * 60 * 60));
    let fake = Fake::new(NAME, vec![], true);
    let task = fx.start(&fake);
    assert!(!fx.served());
    assert_eq!(task.state().state, CertStateKind::Expired);
    assert_eq!(task.state().not_after_ms, Some(1_798_761_600_000));
}

#[tokio::test]
async fn the_setting_can_turn_on_and_off_while_the_deck_runs() {
    let fx = Fx::new(false, false);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], false);
    let task = fx.start(&fake);
    settle().await;
    assert!(!fx.served());
    fx.live.publish(live(true));
    until("valid", || task.state().state == CertStateKind::Valid).await;
    assert!(fx.served() && fx.file());
    fx.live.publish(live(false));
    until("off", || task.state().state == CertStateKind::Off).await;
    assert!(!fx.served() && !fx.file());
    assert_eq!(task.state(), CertState::off());
}

#[tokio::test]
async fn the_owner_files_win_and_the_state_is_still_reported() {
    let fx = Fx::new(true, true);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], false);
    let task = fx.start(&fake);
    until("valid", || task.state().state == CertStateKind::Valid).await;
    assert!(!fx.served());
}

#[tokio::test]
async fn the_task_stops_on_shutdown() {
    let fx = Fx::new(true, false);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], true);
    let task = fx.start(&fake);
    until("the call", || fake.issued() == 1).await;
    fx.stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task.join())
        .await
        .unwrap();
    assert!(!fx.served() && !fx.file());
}

#[test]
fn the_production_anchors_are_not_the_test_anchors() {
    let fx = Fx::new(true, false);
    let cert = fx.pki.check(Duration::ZERO).unwrap();
    let chain = vec![cert.chain[0].clone(), cert.chain[1].clone()];
    let (c2, k2) = crate::http::tls_tailscale::parse_pem(&cert.to_pem().unwrap()).unwrap();
    assert_eq!(chain, c2);
    let refused = check(
        c2,
        k2,
        NAME,
        UnixTime::since_unix_epoch(Duration::from_millis(NOW_MS)),
        Duration::ZERO,
        &isrg_anchors(),
    );
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_new_program_choice_cancels_the_running_issue_and_starts_again() {
    let fx = Fx::new(true, false);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], true);
    let task = fx.start(&fake);
    until("the first call", || fake.issued() == 1).await;
    let mut other = live(true);
    other.tailscale_bin = Some(PathBuf::from("/opt/other/tailscale"));
    fx.live.publish(other);
    // The schedule is 30 s: the second call is the effect of the change.
    until("the second call", || fake.issued() == 2).await;
    assert!(!fx.served() && !fx.file());
    fake.gate.as_ref().unwrap().notify_one();
    until("valid", || task.state().state == CertStateKind::Valid).await;
    assert_eq!(fake.issued(), 2);
    assert!(fx.served() && fx.file());
}

#[tokio::test]
async fn a_new_program_choice_while_off_starts_nothing() {
    let fx = Fx::new(false, false);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], false);
    let task = fx.start(&fake);
    let mut other = live(false);
    other.tailscale_bin = Some(PathBuf::from("/opt/other/tailscale"));
    fx.live.publish(other);
    settle().await;
    assert_eq!(fake.issued(), 0);
    assert_eq!(task.state().state, CertStateKind::Off);
}

#[tokio::test]
async fn turning_off_removes_the_staging_file_too() {
    let fx = Fx::new(true, false);
    let fake = Fake::new(NAME, vec![Ok(fx.pem())], false);
    let task = fx.start(&fake);
    until("valid", || task.state().state == CertStateKind::Valid).await;
    let staging = fx.dir.path().join("board-tailscale.pem.tmp");
    fs::write(&staging, "orphan").unwrap();
    fx.live.publish(live(false));
    until("off", || task.state().state == CertStateKind::Off).await;
    assert!(!staging.exists() && !fx.file());
}

#[tokio::test]
async fn the_start_removes_an_orphan_staging_file() {
    let fx = Fx::new(false, false);
    let staging = fx.dir.path().join("board-tailscale.pem.tmp");
    fs::write(&staging, "orphan").unwrap();
    let fake = Fake::new(NAME, vec![], false);
    let _task = fx.start(&fake);
    assert!(!staging.exists());
}
