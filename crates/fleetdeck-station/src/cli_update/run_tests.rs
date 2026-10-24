//! The attempt (design §3.7) with fakes for the manager side, the target,
//! the stage and the way back: no network and no real CLI. The fixtures are
//! shared with the tests of the recovery and the schedule.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_proto::cli_update::CliUpdateAttempt;
use fleetdeck_testkit::Det;

use super::*;
use crate::cli_update::stage::Placed;
use crate::store::StoreConfig;
use crate::store::cli_update_journal::{JournalAction, MovedEntry, QuarantineAction};

pub(in crate::cli_update) const FROM: &str = "2.1.309";
pub(in crate::cli_update) const TARGET: &str = "2.1.310";
pub(in crate::cli_update) const CODEX_FROM: &str = "0.163.0";
pub(in crate::cli_update) const CODEX_TARGET: &str = "0.164.0";

/// The events of the fakes in the order that they happened.
pub(in crate::cli_update) type Log = Arc<Mutex<Vec<String>>>;

fn push(log: &Log, text: impl Into<String>) {
    log.lock().unwrap().push(text.into());
}

struct HoldGuard(Log);

impl Drop for HoldGuard {
    fn drop(&mut self) {
        push(&self.0, "hold_end");
    }
}

pub(in crate::cli_update) struct FakeHost {
    pub live: Mutex<Vec<SessionId>>,
    pub hold: Mutex<Option<&'static str>>,
    pub installed: Mutex<Option<String>>,
    pub log: Log,
}

#[async_trait]
impl Host for FakeHost {
    async fn live_sessions(&self, _kind: ProviderKind) -> Vec<SessionId> {
        self.live.lock().unwrap().clone()
    }
    async fn begin_hold(&self, _kind: ProviderKind) -> Result<Hold, &'static str> {
        if let Some(r) = *self.hold.lock().unwrap() {
            return Err(r);
        }
        push(&self.log, "hold_begin");
        Ok(Box::new(HoldGuard(Arc::clone(&self.log))))
    }
    async fn installed(&self, _kind: ProviderKind) -> Option<String> {
        self.installed.lock().unwrap().clone()
    }
    async fn refresh(&self) {
        push(&self.log, "refresh");
    }
    fn quarantine(&self, kind: ProviderKind, _text: &str) {
        push(&self.log, format!("quarantine {kind:?}"));
    }
    fn release_quarantine(&self, kind: ProviderKind) {
        push(&self.log, format!("release {kind:?}"));
    }
}

pub(in crate::cli_update) struct FakeChannel {
    pub result: Mutex<Result<Verified, &'static str>>,
    pub calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Channel for FakeChannel {
    async fn resolve(
        &self,
        _install: &Install,
        _pin: Option<&str>,
        _channel: &str,
    ) -> Result<Verified, &'static str> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.result.lock().unwrap().clone()
    }
}

pub(in crate::cli_update) struct FakeConfig(pub Mutex<ProviderUpdatesConfig>);

impl ConfigSource for FakeConfig {
    fn current(&self) -> ProviderUpdatesConfig {
        self.0.lock().unwrap().clone()
    }
}

/// The stage, activation and way back of the fakes. `versions` is what the
/// launcher of each kind reports now.
pub(in crate::cli_update) struct FakeInstaller {
    pub versions: Mutex<HashMap<ProviderKind, String>>,
    pub handshake_ok: AtomicBool,
    pub stage_err: Mutex<Option<Failure>>,
    pub rollback_err: Mutex<Option<Failure>>,
    /// The failure of A2 or V. The version flips first when `flips` is set.
    pub activate_err: Mutex<Option<Failure>>,
    pub flips: AtomicBool,
    pub restore_err: Mutex<Option<Failure>>,
    pub restore_sleep: Mutex<Option<Duration>>,
    pub log: Log,
    pub store: Store,
    pub host: Arc<FakeHost>,
    /// A session that starts while the stage runs (step 13).
    pub live_after_stage: Mutex<Option<SessionId>>,
    pub journal_at_stage: Mutex<Option<usize>>,
    pub channel_calls: Arc<AtomicUsize>,
    pub calls_at_activate: Mutex<Option<usize>>,
}

#[async_trait]
impl Installer for FakeInstaller {
    async fn probe(
        &self,
        _ctx: &Ctx<'_>,
        kind: ProviderKind,
        _program: &Path,
        _env: &ChildEnv,
    ) -> Result<UpdateProbe, Failure> {
        Ok(UpdateProbe {
            version: self.versions.lock().unwrap().get(&kind).cloned(),
            handshake: if self.handshake_ok.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err("no_answer")
            },
        })
    }

    async fn stage(
        &self,
        ctx: &Ctx<'_>,
        _install: &Install,
        _verified: &Verified,
        _instance: &ChildEnv,
    ) -> Result<Staged, Failure> {
        push(&self.log, "stage");
        let rows = self.store.cli_update_journals().await.unwrap();
        *self.journal_at_stage.lock().unwrap() = Some(rows.len());
        if let Some(f) = *self.stage_err.lock().unwrap() {
            return Err(f);
        }
        if let Some(id) = self.live_after_stage.lock().unwrap().take() {
            self.host.live.lock().unwrap().push(id);
        }
        Ok(Staged::Native {
            placed: Placed {
                path: ctx.station_dir.join("staged"),
                created: false,
            },
        })
    }

    async fn rollback_copy(
        &self,
        ctx: &Ctx<'_>,
        _install: &Install,
        _staged: &Staged,
        rollback_sha256: Option<&str>,
    ) -> Result<Activation, Failure> {
        push(&self.log, "rollback_copy");
        if let Some(f) = *self.rollback_err.lock().unwrap() {
            return Err(f);
        }
        Ok(Activation {
            rollback_path: ctx.station_dir.join("rollback-copy"),
            rollback_sha256: rollback_sha256.map(str::to_owned),
            moved: vec![MovedEntry {
                name: "claude".to_owned(),
                digest: None,
            }],
        })
    }

    async fn activate(
        &self,
        _ctx: &Ctx<'_>,
        install: &Install,
        _staged: &Staged,
        _act: &Activation,
        target: &str,
        _env: &ChildEnv,
    ) -> Result<(), Failure> {
        push(&self.log, "activate");
        *self.calls_at_activate.lock().unwrap() = Some(self.channel_calls.load(Ordering::SeqCst));
        let err = *self.activate_err.lock().unwrap();
        if err.is_none() || self.flips.load(Ordering::SeqCst) {
            self.versions
                .lock()
                .unwrap()
                .insert(install.kind, target.to_owned());
        }
        err.map_or(Ok(()), Err)
    }

    async fn restore(
        &self,
        _ctx: &Ctx<'_>,
        rec: &RecoveryRecord,
        _tool: &Tool,
        _env: &ChildEnv,
    ) -> Result<(), Failure> {
        push(&self.log, "restore");
        let sleep = *self.restore_sleep.lock().unwrap();
        if let Some(d) = sleep {
            tokio::time::sleep(d).await;
        }
        if let Some(f) = *self.restore_err.lock().unwrap() {
            return Err(f);
        }
        self.versions
            .lock()
            .unwrap()
            .insert(rec.provider, rec.from_version.clone());
        Ok(())
    }
}

/// An engine over a real store and the fakes, with a native Claude (or an
/// npm Codex) in a temp folder.
pub(in crate::cli_update) struct Rig {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub engine: Arc<Engine>,
    pub host: Arc<FakeHost>,
    pub channel: Arc<FakeChannel>,
    pub installer: Arc<FakeInstaller>,
    pub config: Arc<FakeConfig>,
    pub log: Log,
    pub kind: ProviderKind,
}

fn write_exec(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"#!/bin/sh\necho fake\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn link(target: impl AsRef<Path>, at: &Path) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target.as_ref(), at).unwrap();
}

/// The config of the tests: no soak time, updates on.
pub(in crate::cli_update) fn test_config() -> ProviderUpdatesConfig {
    ProviderUpdatesConfig {
        enabled: true,
        min_age_hours: 0,
        ..ProviderUpdatesConfig::default()
    }
}

/// The environment and the folders of the layout of `kind` under `home`.
fn layout(home: &Path, kind: ProviderKind) -> (ChildEnv, &'static str) {
    let mut env = ChildEnv::empty();
    env.set("HOME", home).unwrap();
    match kind {
        ProviderKind::Claude => {
            write_exec(&home.join(".local/share/claude/versions").join(FROM));
            link(
                home.join(".local/share/claude/versions").join(FROM),
                &home.join(".local/bin/claude"),
            );
            write_exec(&home.join("tools/gpgv"));
            let path = std::env::join_paths([home.join(".local/bin"), home.join("tools")]);
            env.set("PATH", path.unwrap()).unwrap();
            (env, "claude")
        }
        _ => {
            let prefix = home.join("nvm/node");
            write_exec(&prefix.join("lib/node_modules/@openai/codex/bin/codex.js"));
            link(
                "../lib/node_modules/@openai/codex/bin/codex.js",
                &prefix.join("bin/codex"),
            );
            write_exec(&prefix.join("bin/npm"));
            env.set("PATH", prefix.join("bin")).unwrap();
            (env, "codex")
        }
    }
}

fn verified(kind: ProviderKind, target: &str, sha: &str) -> Verified {
    match kind {
        ProviderKind::Claude => Verified::Native {
            target: target.to_owned(),
            published_at: Some(0),
            platform: "linux-x64",
            expected_sha256: sha.to_owned(),
        },
        _ => Verified::Npm {
            target: target.to_owned(),
            published_at: Some(0),
            set: Vec::new(),
        },
    }
}

pub(in crate::cli_update) async fn open_store(dir: &Path, det: &Det) -> Store {
    let s = Store::open(
        &dir.join("station.db"),
        det.clock(),
        det.ids(),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s
}

pub(in crate::cli_update) async fn rig(kind: ProviderKind) -> Rig {
    rig_with(kind, true).await
}

/// `enabled` is `provider_updates.enabled` at the station start.
pub(in crate::cli_update) async fn rig_with(kind: ProviderKind, enabled: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let det = Det::new();
    let store = open_store(dir.path(), &det).await;
    let home = dir.path().join("home");
    let (env, binary) = layout(&home, kind);
    let (from, target) = match kind {
        ProviderKind::Claude => (FROM, TARGET),
        _ => (CODEX_FROM, CODEX_TARGET),
    };
    let sha = match kind {
        ProviderKind::Claude => {
            sha256_file(&home.join(".local/share/claude/versions").join(FROM)).unwrap()
        }
        _ => String::new(),
    };
    let log: Log = Arc::default();
    let host = Arc::new(FakeHost {
        live: Mutex::new(Vec::new()),
        hold: Mutex::new(None),
        installed: Mutex::new(Some(from.to_owned())),
        log: Arc::clone(&log),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let channel = Arc::new(FakeChannel {
        result: Mutex::new(Ok(verified(kind, target, &sha))),
        calls: Arc::clone(&calls),
    });
    let installer = Arc::new(FakeInstaller {
        versions: Mutex::new(HashMap::from([(kind, from.to_owned())])),
        handshake_ok: AtomicBool::new(true),
        stage_err: Mutex::new(None),
        rollback_err: Mutex::new(None),
        activate_err: Mutex::new(None),
        flips: AtomicBool::new(false),
        restore_err: Mutex::new(None),
        restore_sleep: Mutex::new(None),
        log: Arc::clone(&log),
        store: store.clone(),
        host: Arc::clone(&host),
        live_after_stage: Mutex::new(None),
        journal_at_stage: Mutex::new(None),
        channel_calls: calls,
        calls_at_activate: Mutex::new(None),
    });
    let config = Arc::new(FakeConfig(Mutex::new(test_config())));
    let engine = Arc::new(Engine::new(Parts {
        store: store.clone(),
        host: host.clone(),
        channel: channel.clone(),
        installer: installer.clone(),
        config: config.clone(),
        targets: vec![ProviderTarget {
            kind,
            binary: PathBuf::from(binary),
            env,
        }],
        station_dir: dir.path().join("station"),
        ids: det.ids(),
        shim_host: false,
        enabled,
        curl: PathBuf::from("curl"),
    }));
    Rig {
        dir,
        store,
        engine,
        host,
        channel,
        installer,
        config,
        log,
        kind,
    }
}

impl Rig {
    pub(in crate::cli_update) async fn run(
        &self,
        check_only: bool,
        force: bool,
    ) -> CliUpdateRunAnswer {
        self.engine
            .run_attempt(self.kind, UpdateTrigger::Owner, check_only, force)
            .await
    }

    /// Waits until the task of a started attempt ended.
    pub(in crate::cli_update) async fn settle(&self) {
        for _ in 0..500 {
            if self.engine.running_id().is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the attempt did not end");
    }

    pub(in crate::cli_update) async fn last(&self) -> Option<CliUpdateAttempt> {
        self.store.cli_update_last(self.kind).await.unwrap()
    }

    pub(in crate::cli_update) fn events(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub(in crate::cli_update) fn version(&self) -> String {
        self.installer.versions.lock().unwrap()[&self.kind].clone()
    }

    pub(in crate::cli_update) async fn journals(&self) -> usize {
        self.store.cli_update_journals().await.unwrap().len()
    }

    pub(in crate::cli_update) async fn quarantine(&self) -> Option<String> {
        let rows = self.store.cli_update_quarantines().await.unwrap();
        rows.into_iter().next().map(|q| q.reason)
    }

    /// A quarantine row of the provider (a `rollback_failed` that ended).
    pub(in crate::cli_update) async fn quarantine_now(&self) {
        let a = new_attempt(
            "q0",
            self.kind,
            UpdateTrigger::Owner,
            InstallMethod::Native,
            "stable",
            Some(FROM),
            Some(TARGET),
        );
        let j = NewJournal {
            provider: self.kind,
            method: InstallMethod::Native,
            from_version: FROM.to_owned(),
            target_version: TARGET.to_owned(),
            launcher: "/x".into(),
            dest: "/x".into(),
            stage_root: self.dir.path().join("none"),
            rollback_sha256: None,
        };
        self.store.cli_update_begin(a, j).await.unwrap();
        let q = QuarantineAction::Set {
            reason: reason::ROLLBACK_FAILED,
            text: "reinstall".to_owned(),
        };
        let end = end_of(
            UpdateOutcome::RollbackFailed,
            Some(reason::ROLLBACK_FAILED),
            None,
        );
        self.store
            .cli_update_finish("q0".to_owned(), end, JournalAction::Delete, q)
            .await
            .unwrap();
    }
}

fn pos(events: &[String], name: &str) -> usize {
    events.iter().position(|e| e == name).unwrap()
}

#[tokio::test]
async fn an_update_ends_updated_and_the_hold_wraps_the_activation() {
    let r = rig(ProviderKind::Claude).await;
    let answer = r.run(false, false).await;
    assert!(answer.attempt_id.is_some());
    assert_eq!(answer.refused, None);
    r.settle().await;
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::Updated);
    assert_eq!(last.to_version.as_deref(), Some(TARGET));
    assert_eq!(r.journals().await, 0);
    let e = r.events();
    assert!(pos(&e, "hold_begin") < pos(&e, "rollback_copy"));
    assert!(pos(&e, "rollback_copy") < pos(&e, "activate"));
    assert!(pos(&e, "activate") < pos(&e, "hold_end"));
    assert!(e.contains(&"refresh".to_owned()));
}

#[tokio::test]
async fn up_to_date_writes_a_row() {
    let r = rig(ProviderKind::Claude).await;
    *r.channel.result.lock().unwrap() = Ok(verified(ProviderKind::Claude, FROM, ""));
    let answer = r.run(false, false).await;
    assert_eq!(answer.attempt_id, None);
    assert_eq!(r.last().await.unwrap().outcome, UpdateOutcome::UpToDate);
}

#[tokio::test]
async fn a_refused_gate_writes_a_row() {
    let r = rig(ProviderKind::Claude).await;
    *r.channel.result.lock().unwrap() = Ok(verified(ProviderKind::Claude, "2.1.288", ""));
    let answer = r.run(false, false).await;
    assert_eq!(answer.refused.as_deref(), Some(reason::BELOW_TESTED));
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::Refused);
    assert_eq!(last.reason.as_deref(), Some(reason::BELOW_TESTED));
}

#[tokio::test]
async fn a_busy_owner_attempt_at_step_9_writes_no_row_and_answers_busy() {
    let r = rig(ProviderKind::Claude).await;
    r.host
        .live
        .lock()
        .unwrap()
        .push(SessionId::from("s1".to_owned()));
    let answer = r.run(false, false).await;
    assert_eq!(answer.refused.as_deref(), Some(reason::SESSIONS_RUNNING));
    assert_eq!(answer.busy.len(), 1);
    assert!(r.last().await.is_none());
    assert!(!r.events().contains(&"stage".to_owned()));
}

#[tokio::test]
async fn a_session_that_starts_during_the_stage_ends_skipped_busy() {
    let r = rig(ProviderKind::Claude).await;
    *r.installer.live_after_stage.lock().unwrap() = Some(SessionId::from("s1".to_owned()));
    r.run(false, false).await;
    r.settle().await;
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::SkippedBusy);
    assert_eq!(last.reason.as_deref(), Some(reason::SESSIONS_RUNNING));
    assert!(!r.events().contains(&"activate".to_owned()));
    assert_eq!(r.version(), FROM);
    assert_eq!(r.journals().await, 0);
}

#[tokio::test]
async fn force_on_native_proceeds_with_live_sessions() {
    let r = rig(ProviderKind::Claude).await;
    r.host
        .live
        .lock()
        .unwrap()
        .push(SessionId::from("s1".to_owned()));
    let answer = r.run(false, true).await;
    assert!(answer.attempt_id.is_some());
    r.settle().await;
    assert_eq!(r.last().await.unwrap().outcome, UpdateOutcome::Updated);
}

#[tokio::test]
async fn force_on_npm_is_still_busy() {
    let r = rig(ProviderKind::Codex).await;
    r.host
        .live
        .lock()
        .unwrap()
        .push(SessionId::from("s1".to_owned()));
    let answer = r.run(false, true).await;
    assert_eq!(answer.refused.as_deref(), Some(reason::SESSIONS_RUNNING));
    assert!(r.last().await.is_none());
}

#[tokio::test]
async fn the_journal_row_exists_before_the_first_child_runs() {
    let r = rig(ProviderKind::Claude).await;
    r.run(false, false).await;
    r.settle().await;
    assert_eq!(*r.installer.journal_at_stage.lock().unwrap(), Some(1));
}

#[tokio::test]
async fn a_staging_failure_ends_failed_with_the_install_untouched() {
    let r = rig(ProviderKind::Claude).await;
    *r.installer.stage_err.lock().unwrap() = Some(Failure::Failed(reason::INSTALL_FAILED));
    r.run(false, false).await;
    r.settle().await;
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::Failed);
    assert_eq!(r.journals().await, 0);
    assert_eq!(r.version(), FROM);
    assert!(!r.events().contains(&"hold_begin".to_owned()));
}

#[tokio::test]
async fn a_probe_failure_of_the_staged_copy_refuses_and_never_takes_the_hold() {
    let r = rig(ProviderKind::Claude).await;
    *r.installer.stage_err.lock().unwrap() = Some(Failure::Refused(reason::HANDSHAKE_FAILED));
    r.run(false, false).await;
    r.settle().await;
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::Refused);
    assert_eq!(last.reason.as_deref(), Some(reason::HANDSHAKE_FAILED));
    let e = r.events();
    assert!(!e.contains(&"hold_begin".to_owned()));
    assert!(!e.contains(&"activate".to_owned()));
}

#[tokio::test]
async fn a_failure_after_a2_rolls_back_with_no_channel_call_after_a2() {
    let r = rig(ProviderKind::Claude).await;
    *r.installer.activate_err.lock().unwrap() = Some(Failure::Failed(reason::VERSION_MISMATCH));
    r.installer.flips.store(true, Ordering::SeqCst);
    r.run(false, false).await;
    r.settle().await;
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::RolledBack);
    assert_eq!(last.reason.as_deref(), Some(reason::VERSION_MISMATCH));
    assert_eq!(r.version(), FROM);
    assert_eq!(r.journals().await, 0);
    let at = r.installer.calls_at_activate.lock().unwrap().unwrap();
    assert_eq!(r.channel.calls.load(Ordering::SeqCst), at);
    assert!(r.events().contains(&"restore".to_owned()));
}

#[tokio::test]
async fn a_failed_restore_ends_rollback_failed_with_a_quarantine_row() {
    let r = rig(ProviderKind::Claude).await;
    *r.installer.activate_err.lock().unwrap() = Some(Failure::Failed(reason::VERSION_MISMATCH));
    *r.installer.restore_err.lock().unwrap() = Some(Failure::Failed(reason::ROLLBACK_FAILED));
    r.run(false, false).await;
    r.settle().await;
    assert_eq!(
        r.last().await.unwrap().outcome,
        UpdateOutcome::RollbackFailed
    );
    assert_eq!(
        r.quarantine().await.as_deref(),
        Some(reason::ROLLBACK_FAILED)
    );
    assert_eq!(r.journals().await, 0);
    assert!(r.events().contains(&"quarantine Claude".to_owned()));
}

#[tokio::test]
async fn a_failed_fence_ends_process_left_and_keeps_the_journal_row() {
    let r = rig(ProviderKind::Claude).await;
    *r.installer.activate_err.lock().unwrap() = Some(Failure::ProcessLeft);
    r.run(false, false).await;
    r.settle().await;
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::RollbackFailed);
    assert_eq!(last.reason.as_deref(), Some(reason::PROCESS_LEFT));
    assert_eq!(r.journals().await, 1);
    assert_eq!(r.quarantine().await.as_deref(), Some(reason::PROCESS_LEFT));
    assert!(!r.events().contains(&"restore".to_owned()));
}

#[tokio::test]
async fn recovery_pending_while_a_journal_row_exists() {
    let r = rig(ProviderKind::Claude).await;
    r.quarantine_now().await;
    let a = new_attempt(
        "j1",
        r.kind,
        UpdateTrigger::Owner,
        InstallMethod::Native,
        "stable",
        Some(FROM),
        Some(TARGET),
    );
    let j = NewJournal {
        provider: r.kind,
        method: InstallMethod::Native,
        from_version: FROM.to_owned(),
        target_version: TARGET.to_owned(),
        launcher: "/x".into(),
        dest: "/x".into(),
        stage_root: r.dir.path().join("none"),
        rollback_sha256: None,
    };
    r.store.cli_update_begin(a, j).await.unwrap();
    let answer = r.run(false, false).await;
    assert_eq!(answer.refused.as_deref(), Some(reason::RECOVERY_PENDING));
}

#[tokio::test]
async fn check_only_writes_no_row_and_sets_available() {
    let r = rig(ProviderKind::Claude).await;
    let answer = r.run(true, false).await;
    assert_eq!(answer.state.available.as_deref(), Some(TARGET));
    assert_eq!(answer.attempt_id, None);
    assert!(r.last().await.is_none());
    assert!(!r.events().contains(&"stage".to_owned()));
}

#[tokio::test]
async fn check_only_on_a_quarantined_provider_verifies_and_clears_it_on_a_pass() {
    let r = rig(ProviderKind::Claude).await;
    r.quarantine_now().await;
    let answer = r.run(true, false).await;
    assert_eq!(answer.refused, None);
    assert_eq!(r.quarantine().await, None);
    assert_eq!(r.last().await.unwrap().outcome, UpdateOutcome::Verified);
    assert!(r.events().contains(&"release Claude".to_owned()));
}

#[tokio::test]
async fn check_only_keeps_the_quarantine_after_a_failed_handshake() {
    let r = rig(ProviderKind::Claude).await;
    r.quarantine_now().await;
    r.installer.handshake_ok.store(false, Ordering::SeqCst);
    let answer = r.run(true, false).await;
    assert_eq!(answer.refused.as_deref(), Some(reason::HANDSHAKE_FAILED));
    assert_eq!(
        r.quarantine().await.as_deref(),
        Some(reason::ROLLBACK_FAILED)
    );
    assert_ne!(r.last().await.unwrap().outcome, UpdateOutcome::Verified);
}

#[tokio::test]
async fn a_second_concurrent_attempt_is_refused_another_update() {
    let r = rig(ProviderKind::Claude).await;
    let flight = r.engine.take_flight().expect("flight");
    let answer = r.run(false, false).await;
    assert_eq!(answer.refused.as_deref(), Some(reason::ANOTHER_UPDATE));
    drop(flight);
    assert!(r.engine.running_id().is_none());
}
