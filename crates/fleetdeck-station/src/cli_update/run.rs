//! One attempt (design §3.7).
//!
//! [`Engine`] holds what an attempt needs: the store, the manager side
//! ([`Host`]), the target and its signature check ([`Channel`]), the stage,
//! activation and way back ([`Installer`]) and the settings
//! ([`ConfigSource`]). Tests replace each of them with a fake.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::config::ProviderUpdatesConfig;
use fleetdeck_core::{ChildEnv, SharedIds, new_id};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::cli_update::{
    CliUpdateRunAnswer, InstallMethod, UpdateOutcome, UpdateTrigger, reason,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::update_probe::UpdateProbe;

use super::activate::remove_path;
use super::detect::{Install, Tool, detect};
use super::gate::{GateInput, GateVerdict, gate};
use super::recover::{keep_ids, sweep};
use super::stage::{Ctx, Failure, PgidSink, native_stage_file, probe_env, sha256_file};
use super::verify::Verified;
use crate::store::Store;
use crate::store::cli_update_journal::{Activation, NewJournal, RecoveryRecord};
use crate::store::cli_updates::{AttemptEnd, NewAttempt};

/// Step 17: the staging steps (up to 12 min).
pub const STAGE_PHASE: Duration = Duration::from_secs(12 * 60);
/// Step 17: the hold (up to 3 min).
pub const HOLD_PHASE: Duration = Duration::from_secs(3 * 60);
/// After a phase limit: the wait for the file system tasks of the phase.
pub const SETTLE_LIMIT: Duration = Duration::from_secs(30);

/// The limits of an attempt. Tests make them short.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub stage: Duration,
    pub hold: Duration,
    pub settle: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            stage: STAGE_PHASE,
            hold: HOLD_PHASE,
            settle: SETTLE_LIMIT,
        }
    }
}

/// What a hold is: dropping it ends the hold.
pub type Hold = Box<dyn Send>;

/// The manager side of an attempt.
#[async_trait]
pub trait Host: Send + Sync {
    /// The sessions with a live process of `kind`.
    async fn live_sessions(&self, kind: ProviderKind) -> Vec<SessionId>;
    /// §3.7.1. `Err` is `station_restarting`, `another_update` or
    /// `starts_in_flight`.
    async fn begin_hold(&self, kind: ProviderKind) -> Result<Hold, &'static str>;
    /// The version of the first instance of `kind` in the provider report.
    async fn installed(&self, kind: ProviderKind) -> Option<String>;
    /// Probes the providers again (§3.8).
    async fn refresh(&self);
    fn quarantine(&self, kind: ProviderKind, text: &str);
    fn release_quarantine(&self, kind: ProviderKind);
}

/// §3.5: the verified target.
#[async_trait]
pub trait Channel: Send + Sync {
    /// # Errors
    /// A code of [`reason`].
    async fn resolve(
        &self,
        install: &Install,
        pin: Option<&str>,
        channel: &str,
    ) -> Result<Verified, &'static str>;
}

/// The `[provider_updates]` values now.
pub trait ConfigSource: Send + Sync {
    fn current(&self) -> ProviderUpdatesConfig;
}

/// What S1 to S4 staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staged {
    Native {
        placed: super::stage::Placed,
    },
    Codex {
        root: PathBuf,
        sm: PathBuf,
        moved: Vec<String>,
    },
}

/// §3.6: stage, rollback copy, activate and the way back.
#[async_trait]
pub trait Installer: Send + Sync {
    /// §3.6.1 on `program`, then the fence.
    async fn probe(
        &self,
        ctx: &Ctx<'_>,
        kind: ProviderKind,
        program: &Path,
        env: &ChildEnv,
    ) -> Result<UpdateProbe, Failure>;
    /// S1 to S4. `instance` is the environment of the instance.
    async fn stage(
        &self,
        ctx: &Ctx<'_>,
        install: &Install,
        verified: &Verified,
        instance: &ChildEnv,
    ) -> Result<Staged, Failure>;
    /// A1. The caller commits the journal row with the answer before A2.
    async fn rollback_copy(
        &self,
        ctx: &Ctx<'_>,
        install: &Install,
        staged: &Staged,
        rollback_sha256: Option<&str>,
    ) -> Result<Activation, Failure>;
    /// A2 and V.
    async fn activate(
        &self,
        ctx: &Ctx<'_>,
        install: &Install,
        staged: &Staged,
        act: &Activation,
        target: &str,
        env: &ChildEnv,
    ) -> Result<(), Failure>;
    /// §3.6.2 on the install that the journal row names (never a new
    /// detection; pre-PR review finding 1).
    async fn restore(
        &self,
        ctx: &Ctx<'_>,
        rec: &RecoveryRecord,
        env: &ChildEnv,
    ) -> Result<(), Failure>;
    /// Waits until no file system task of this installer runs (pre-PR
    /// review finding 2). `false` when one still runs after `limit`.
    async fn settle(&self, limit: Duration) -> bool;
}

/// One configured provider instance: its binary and its environment.
#[derive(Debug, Clone)]
pub struct ProviderTarget {
    pub kind: ProviderKind,
    pub binary: PathBuf,
    pub env: ChildEnv,
}

/// What [`Engine::new`] needs.
pub struct Parts {
    pub store: Store,
    pub host: Arc<dyn Host>,
    pub channel: Arc<dyn Channel>,
    pub installer: Arc<dyn Installer>,
    pub config: Arc<dyn ConfigSource>,
    pub targets: Vec<ProviderTarget>,
    pub station_dir: PathBuf,
    pub ids: SharedIds,
    /// The station hosts sessions in a shim.
    pub shim_host: bool,
    /// `provider_updates.enabled` at the station start.
    pub enabled: bool,
    pub curl: PathBuf,
}

/// The last check of a provider.
#[derive(Debug, Clone)]
pub(super) struct Check {
    pub(super) available: Option<String>,
    pub(super) at: i64,
}

/// Where the process group of a child goes: the journal row of `kind`.
pub(super) struct JournalSink {
    pub(super) store: Store,
    pub(super) kind: ProviderKind,
}

#[async_trait]
impl PgidSink for JournalSink {
    async fn record(&self, pgid: i32) {
        if let Err(e) = self.store.cli_update_journal_pgid(self.kind, pgid).await {
            tracing::warn!(error = %e, "the update journal does not take the process group");
        }
    }
}

/// A sink for the children that run before the journal row exists.
pub struct NullSink;

#[async_trait]
impl PgidSink for NullSink {
    async fn record(&self, _pgid: i32) {}
}

/// The single flight (§3.7 step 1). Dropping it frees the slot.
pub struct Flight {
    engine: Arc<Engine>,
    pub id: String,
}

impl Drop for Flight {
    fn drop(&mut self) {
        if let Ok(mut running) = self.engine.running.lock() {
            *running = None;
        }
    }
}

/// What the task of a started attempt needs.
pub(super) struct Job {
    pub(super) id: String,
    pub(super) kind: ProviderKind,
    pub(super) install: Install,
    pub(super) instance: ChildEnv,
    pub(super) verified: Verified,
    pub(super) target: String,
    pub(super) from: String,
    pub(super) force: bool,
    pub(super) rollback_sha256: Option<String>,
    pub(super) stage_root: PathBuf,
}

/// The update service state of one station.
pub struct Engine {
    pub(super) store: Store,
    pub(super) host: Arc<dyn Host>,
    pub(super) channel: Arc<dyn Channel>,
    pub(super) installer: Arc<dyn Installer>,
    pub(super) config: Arc<dyn ConfigSource>,
    pub(super) targets: Vec<ProviderTarget>,
    pub(super) station_dir: PathBuf,
    pub(super) ids: SharedIds,
    pub(super) shim_host: bool,
    pub(super) enabled: bool,
    pub(super) curl: PathBuf,
    pub(super) running: Mutex<Option<String>>,
    pub(super) checks: Mutex<HashMap<ProviderKind, Check>>,
    pub(super) limits: Mutex<Limits>,
}

/// `stable`, `latest` or `pin` (the channel text of a row).
#[must_use]
pub fn channel_text(kind: ProviderKind, cfg: &ProviderUpdatesConfig) -> String {
    match kind {
        ProviderKind::Claude if cfg.claude_pin.is_some() => "pin".to_owned(),
        ProviderKind::Claude => cfg.claude_channel.text().to_owned(),
        _ if cfg.codex_pin.is_some() => "pin".to_owned(),
        _ => "latest".to_owned(),
    }
}

pub(super) fn pin_of(kind: ProviderKind, cfg: &ProviderUpdatesConfig) -> Option<String> {
    match kind {
        ProviderKind::Claude => cfg.claude_pin.clone(),
        _ => cfg.codex_pin.clone(),
    }
}

pub(super) fn new_attempt(
    id: &str,
    kind: ProviderKind,
    trigger: UpdateTrigger,
    method: InstallMethod,
    channel: &str,
    from: Option<&str>,
    target: Option<&str>,
) -> NewAttempt {
    NewAttempt {
        attempt_id: id.to_owned(),
        provider: kind,
        trigger,
        method,
        channel: channel.to_owned(),
        from_version: from.map(str::to_owned),
        target_version: target.map(str::to_owned),
    }
}

pub(super) fn end_of(outcome: UpdateOutcome, why: Option<&str>, to: Option<&str>) -> AttemptEnd {
    AttemptEnd {
        outcome,
        reason: why.map(str::to_owned),
        to_version: to.map(str::to_owned),
        target_version: None,
    }
}

pub(super) fn version_target(v: &Verified) -> (String, Option<i64>) {
    match v {
        Verified::Native {
            target,
            published_at,
            ..
        }
        | Verified::Npm {
            target,
            published_at,
            ..
        } => (target.clone(), *published_at),
    }
}

impl Engine {
    #[must_use]
    pub fn new(p: Parts) -> Self {
        Self {
            store: p.store,
            host: p.host,
            channel: p.channel,
            installer: p.installer,
            config: p.config,
            targets: p.targets,
            station_dir: p.station_dir,
            ids: p.ids,
            shim_host: p.shim_host,
            enabled: p.enabled,
            curl: p.curl,
            running: Mutex::new(None),
            checks: Mutex::new(HashMap::new()),
            limits: Mutex::new(Limits::default()),
        }
    }

    /// The attempt that runs now.
    #[must_use]
    pub fn running_id(&self) -> Option<String> {
        self.running.lock().ok().and_then(|r| r.clone())
    }

    /// §3.7 step 1: takes the single flight, or `None`.
    pub fn take_flight(self: &Arc<Self>) -> Option<Flight> {
        let mut running = self.running.lock().ok()?;
        if running.is_some() {
            return None;
        }
        let id = new_id::<String>(self.ids.as_ref());
        *running = Some(id.clone());
        Some(Flight {
            engine: Arc::clone(self),
            id,
        })
    }

    pub(super) fn primary(&self, kind: ProviderKind) -> Option<&ProviderTarget> {
        self.targets.iter().find(|t| t.kind == kind)
    }

    /// Detection of the first instance of `kind`. Another instance of the
    /// kind with another resolved binary blocks it (`second_install`).
    ///
    /// # Errors
    /// `not_found`.
    pub(super) fn install_of(&self, kind: ProviderKind) -> Result<Install, &'static str> {
        let t = self.primary(kind).ok_or(reason::NOT_FOUND)?;
        let mut install = detect(kind, &t.binary, &t.env)?;
        if install.blocked.is_none() {
            let differs = self
                .targets
                .iter()
                .filter(|o| o.kind == kind)
                .skip(1)
                .any(|o| detect(kind, &o.binary, &o.env).is_ok_and(|i| i.real != install.real));
            if differs {
                install.blocked = Some(reason::SECOND_INSTALL);
            }
        }
        Ok(install)
    }

    /// The probe of the launcher (version and handshake) in a private cwd
    /// that this call removes.
    pub(super) async fn probe_launcher(
        &self,
        id: &str,
        kind: ProviderKind,
        install: &Install,
    ) -> Result<UpdateProbe, Failure> {
        let t = self
            .primary(kind)
            .ok_or(Failure::Failed(reason::NOT_FOUND))?;
        let env = probe_env(&t.env, id).ok_or(Failure::Failed(reason::INSTALL_FAILED))?;
        let ctx = Ctx {
            attempt_id: id,
            marker: id,
            station_dir: &self.station_dir,
            sink: &NullSink,
            curl: &self.curl,
        };
        let out = self
            .installer
            .probe(&ctx, kind, &install.launcher, &env)
            .await;
        remove_path(&self.station_dir.join("cli-update/probe-cwd").join(id));
        out
    }

    pub(super) fn set_check(&self, kind: ProviderKind, available: Option<String>) {
        let at = self.store.now_ms();
        if let Ok(mut c) = self.checks.lock() {
            c.insert(kind, Check { available, at });
        }
    }

    pub(super) async fn quarantine_reason(&self, kind: ProviderKind) -> Option<String> {
        let rows = self.store.cli_update_quarantines().await.ok()?;
        rows.into_iter()
            .find(|q| q.provider == kind)
            .map(|q| q.reason)
    }

    pub(super) async fn has_journal(&self, kind: ProviderKind) -> bool {
        self.store
            .cli_update_journals()
            .await
            .map(|rows| rows.iter().any(|j| j.provider == kind))
            .unwrap_or(false)
    }

    /// §3.7: one attempt. A started update answers at once and ends in a
    /// task; every other result is in the answer.
    pub async fn run_attempt(
        self: &Arc<Self>,
        kind: ProviderKind,
        trigger: UpdateTrigger,
        check_only: bool,
        force: bool,
    ) -> CliUpdateRunAnswer {
        let Some(flight) = self.take_flight() else {
            return self
                .answer(kind, None, None, Some(reason::ANOTHER_UPDATE), Vec::new())
                .await;
        };
        self.attempt(flight, kind, trigger, check_only, force).await
    }

    #[allow(clippy::too_many_lines)]
    async fn attempt(
        self: &Arc<Self>,
        flight: Flight,
        kind: ProviderKind,
        trigger: UpdateTrigger,
        check_only: bool,
        force: bool,
    ) -> CliUpdateRunAnswer {
        let id = flight.id.clone();
        let cfg = self.config.current();
        // 2
        if self.has_journal(kind).await {
            return self
                .answer(kind, None, None, Some(reason::RECOVERY_PENDING), Vec::new())
                .await;
        }
        // 3
        let install = match self.install_of(kind) {
            Ok(i) => i,
            Err(r) => return self.answer(kind, None, None, Some(r), Vec::new()).await,
        };
        let Some(instance) = self.primary(kind).map(|t| t.env.clone()) else {
            return self
                .answer(kind, None, None, Some(reason::NOT_FOUND), Vec::new())
                .await;
        };
        let keep = {
            let mut k = keep_ids(&self.store).await;
            k.push(id.clone());
            k
        };
        sweep(&self.station_dir, &install.tool, &keep);
        let channel = channel_text(kind, &cfg);
        let pin = pin_of(kind, &cfg);
        let installed = match self.probe_launcher(&id, kind, &install).await {
            Ok(p) => p.version,
            Err(f) => {
                let r = f.reason();
                return self
                    .answer(kind, Some(&install), None, Some(r), Vec::new())
                    .await;
            }
        };
        // 4
        if check_only && self.quarantine_reason(kind).await.is_some() {
            let refused = self.verify_inner(&id, kind, trigger).await.err();
            return self
                .answer(kind, Some(&install), None, refused, Vec::new())
                .await;
        }
        // 5
        if let Some(blocked) = install.blocked {
            if !check_only {
                let outcome = if matches!(blocked, reason::TOOL_MISSING | reason::GPGV_MISSING) {
                    UpdateOutcome::Refused
                } else {
                    UpdateOutcome::ReportOnly
                };
                self.record(
                    &id,
                    kind,
                    trigger,
                    &install,
                    &channel,
                    (installed.as_deref(), None),
                    (outcome, Some(blocked), installed.as_deref()),
                )
                .await;
            }
            return self
                .answer(kind, Some(&install), None, None, Vec::new())
                .await;
        }
        // 6
        let verified = match self
            .channel
            .resolve(&install, pin.as_deref(), &channel)
            .await
        {
            Ok(v) => v,
            Err(r) => {
                if !check_only {
                    let outcome = if r == reason::CHANNEL_UNREACHABLE {
                        UpdateOutcome::Failed
                    } else {
                        UpdateOutcome::Refused
                    };
                    self.record(
                        &id,
                        kind,
                        trigger,
                        &install,
                        &channel,
                        (installed.as_deref(), None),
                        (outcome, Some(r), installed.as_deref()),
                    )
                    .await;
                }
                return self
                    .answer(kind, Some(&install), None, Some(r), Vec::new())
                    .await;
            }
        };
        // 7
        let (target, published_at) = version_target(&verified);
        let verdict = gate(&GateInput {
            kind,
            installed: installed.as_deref(),
            target: &target,
            pinned: pin.is_some(),
            shim_host: self.shim_host,
            published_at,
            now_ms: self.store.now_ms(),
            min_age_hours: cfg.min_age_hours,
        });
        let verdict = match verdict {
            Ok(v) => v,
            Err(r) => {
                if !check_only {
                    self.record(
                        &id,
                        kind,
                        trigger,
                        &install,
                        &channel,
                        (installed.as_deref(), Some(&target)),
                        (UpdateOutcome::Refused, Some(r), installed.as_deref()),
                    )
                    .await;
                }
                return self
                    .answer(kind, Some(&install), None, Some(r), Vec::new())
                    .await;
            }
        };
        if verdict == GateVerdict::UpToDate {
            self.set_check(kind, None);
            if !check_only {
                self.record(
                    &id,
                    kind,
                    trigger,
                    &install,
                    &channel,
                    (installed.as_deref(), Some(&target)),
                    (UpdateOutcome::UpToDate, None, installed.as_deref()),
                )
                .await;
            }
            return self
                .answer(kind, Some(&install), None, None, Vec::new())
                .await;
        }
        // 8
        self.set_check(kind, Some(target.clone()));
        if check_only {
            return self
                .answer(kind, Some(&install), None, None, Vec::new())
                .await;
        }
        let Some(from) = installed.clone() else {
            // A CLI that does not report its version cannot be restored.
            self.record(
                &id,
                kind,
                trigger,
                &install,
                &channel,
                (None, Some(&target)),
                (UpdateOutcome::Refused, Some(reason::NOT_FOUND), None),
            )
            .await;
            return self
                .answer(
                    kind,
                    Some(&install),
                    None,
                    Some(reason::NOT_FOUND),
                    Vec::new(),
                )
                .await;
        };
        // 9
        let may_force = force && install.method == InstallMethod::Native;
        let live = self.host.live_sessions(kind).await;
        if !live.is_empty() && !may_force {
            return self
                .answer(
                    kind,
                    Some(&install),
                    None,
                    Some(reason::SESSIONS_RUNNING),
                    live,
                )
                .await;
        }
        // 10
        let paths = match &install.tool {
            Tool::Native { versions, .. } => {
                let real = install.real.clone();
                let sum = tokio::task::spawn_blocking(move || sha256_file(&real)).await;
                match sum {
                    Ok(Ok(sum)) => Some((
                        versions.join(&target),
                        native_stage_file(versions, &id),
                        Some(sum),
                    )),
                    _ => None,
                }
            }
            Tool::Npm { modules, .. } | Tool::Bun { modules, .. } => {
                super::stage::codex_stage_root(&install.tool, &id)
                    .map(|root| (modules.clone(), root, None))
            }
            Tool::None => None,
        };
        let Some((dest, stage_root, rollback_sha256)) = paths else {
            return self
                .answer(
                    kind,
                    Some(&install),
                    None,
                    Some(reason::INSTALL_FAILED),
                    Vec::new(),
                )
                .await;
        };
        let begin = self
            .store
            .cli_update_begin(
                new_attempt(
                    &id,
                    kind,
                    trigger,
                    install.method,
                    &channel,
                    Some(&from),
                    Some(&target),
                ),
                NewJournal {
                    provider: kind,
                    method: install.method,
                    from_version: from.clone(),
                    target_version: target.clone(),
                    launcher: install.launcher.clone(),
                    dest,
                    stage_root: stage_root.clone(),
                    rollback_sha256: rollback_sha256.clone(),
                },
            )
            .await;
        if let Err(e) = begin {
            tracing::warn!(error = %e, "the update attempt does not start");
            return self
                .answer(
                    kind,
                    Some(&install),
                    None,
                    Some(reason::RECOVERY_PENDING),
                    Vec::new(),
                )
                .await;
        }
        // 11
        let job = Job {
            id: id.clone(),
            kind,
            install: install.clone(),
            instance,
            verified,
            target,
            from,
            force: may_force,
            rollback_sha256,
            stage_root,
        };
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            engine.execute(flight, job).await;
        });
        self.answer(kind, Some(&install), Some(id), None, Vec::new())
            .await
    }

    /// One row inserted already ended.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record(
        &self,
        id: &str,
        kind: ProviderKind,
        trigger: UpdateTrigger,
        install: &Install,
        channel: &str,
        versions: (Option<&str>, Option<&str>),
        result: (UpdateOutcome, Option<&str>, Option<&str>),
    ) {
        let a = new_attempt(
            id,
            kind,
            trigger,
            install.method,
            channel,
            versions.0,
            versions.1,
        );
        let end = end_of(result.0, result.1, result.2);
        if let Err(e) = self.store.cli_update_insert(a, Some(end)).await {
            tracing::warn!(error = %e, "the update attempt row is not written");
        }
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
pub(super) mod tests;
