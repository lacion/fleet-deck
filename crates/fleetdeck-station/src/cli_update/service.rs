//! The update service of one station: the real parts of an attempt, the
//! link methods and the verification of a quarantined CLI (design §3.7,
//! §3.8.1, §3.10).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use fleetdeck_core::config::{HostChoice, ProviderUpdatesConfig, StationConfig};
use fleetdeck_core::paths::Env;
use fleetdeck_core::{ChildEnv, SharedIds};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::cli_update::{
    CliUpdateAttemptsPage, CliUpdateAttemptsQuery, CliUpdateReport, CliUpdateRun,
    CliUpdateRunAnswer, InstallMethod, ProviderUpdateState, UpdateOutcome, UpdateTrigger, reason,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::VersionStatus;
use fleetdeck_providers::update_probe::UpdateProbe;
use fleetdeck_providers::version::version_status;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::activate::{
    RESTORE_FAILED, RealFs, activate_codex, activate_native, journal_tool, remove_path, restore,
    rollback_codex, rollback_native, verify_codex, verify_native,
};
use super::channel::{resolve_claude, resolve_codex};
use super::detect::{Install, Tool};
use super::mutations::Mutations;
use super::recover::{Recovery, recover};
use super::run::{
    Channel, ConfigSource, Engine, Hold, Host, Installer, Parts, ProviderTarget, Staged,
    channel_text, end_of, new_attempt,
};
use super::schedule::Schedule;
use super::stage::{
    Ctx, Failure, bind_codex, bind_native, codex_stage_root, download_native, probe_checked,
    probe_env, probe_staged, sha256_file, stage_codex, staged_entry,
};
use super::verify::Verified;
use crate::integrations::exec::Programs;
use crate::manager::SessionManager;
use crate::store::Store;
use crate::store::cli_update_journal::{Activation, RecoveryRecord};

fn io_failed<T>(_: T) -> Failure {
    Failure::Failed(reason::INSTALL_FAILED)
}

/// The real stage, activation and way back (§3.6).
pub struct RealInstaller {
    /// The environment of the station process (the proxy and CA names).
    station: Vec<(OsString, OsString)>,
    /// Every blocking task that changes files (pre-PR review finding 2).
    mutations: Mutations,
}

impl RealInstaller {
    #[must_use]
    pub fn new() -> Self {
        Self {
            station: std::env::vars_os().collect(),
            mutations: Mutations::new(),
        }
    }
}

impl Default for RealInstaller {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Installer for RealInstaller {
    async fn probe(
        &self,
        ctx: &Ctx<'_>,
        kind: ProviderKind,
        program: &Path,
        env: &ChildEnv,
    ) -> Result<UpdateProbe, Failure> {
        probe_checked(ctx, kind, program, env).await
    }

    async fn stage(
        &self,
        ctx: &Ctx<'_>,
        install: &Install,
        verified: &Verified,
        instance: &ChildEnv,
    ) -> Result<Staged, Failure> {
        let env = probe_env(instance, ctx.marker).ok_or(Failure::Failed(reason::INSTALL_FAILED))?;
        match (&install.tool, verified) {
            (
                Tool::Native { versions, .. },
                Verified::Native {
                    target,
                    platform,
                    expected_sha256,
                    ..
                },
            ) => {
                let file = download_native(ctx, versions, target, platform).await?;
                let (v, t, e) = (versions.clone(), target.clone(), expected_sha256.clone());
                let placed = self
                    .mutations
                    .run(move || bind_native(&file, &v, &t, &e))
                    .await
                    .map_err(io_failed)??;
                if let Err(f) =
                    probe_staged(ctx, ProviderKind::Claude, &placed.path, &env, target).await
                {
                    if placed.created {
                        remove_path(&placed.path);
                    }
                    return Err(f);
                }
                Ok(Staged::Native { placed })
            }
            (Tool::Npm { .. } | Tool::Bun { .. }, Verified::Npm { target, set, .. }) => {
                let layout = Failure::Refused(reason::UNSUPPORTED_LAYOUT);
                let root = codex_stage_root(&install.tool, ctx.attempt_id).ok_or(layout)?;
                let sm =
                    stage_codex(ctx, &install.tool, &root, target, instance, &self.station).await?;
                let (tool, r, s) = (install.tool.clone(), root.clone(), set.clone());
                let moved = self
                    .mutations
                    .run(move || bind_codex(&tool, &r, &s))
                    .await
                    .map_err(io_failed)??;
                probe_staged(ctx, ProviderKind::Codex, &staged_entry(&sm), &env, target).await?;
                Ok(Staged::Codex { root, sm, moved })
            }
            _ => Err(Failure::Refused(reason::UNSUPPORTED_LAYOUT)),
        }
    }

    async fn rollback_copy(
        &self,
        ctx: &Ctx<'_>,
        install: &Install,
        staged: &Staged,
        rollback_sha256: Option<&str>,
    ) -> Result<Activation, Failure> {
        match (&install.tool, staged) {
            (Tool::Native { .. }, Staged::Native { .. }) => {
                let sum = rollback_sha256
                    .ok_or(Failure::Failed(reason::INSTALL_FAILED))?
                    .to_owned();
                let (dir, id, real) = (
                    ctx.station_dir.to_path_buf(),
                    ctx.attempt_id.to_owned(),
                    install.real.clone(),
                );
                self.mutations
                    .run(move || rollback_native(&RealFs, &dir, &id, &real, &sum))
                    .await
                    .map_err(io_failed)?
            }
            (
                Tool::Npm { modules, .. } | Tool::Bun { modules, .. },
                Staged::Codex { root, moved, .. },
            ) => {
                let (m, r, mv) = (modules.clone(), root.clone(), moved.clone());
                self.mutations
                    .run(move || rollback_codex(&m, &r, &mv))
                    .await
                    .map_err(io_failed)?
                    .map_err(io_failed)
            }
            _ => Err(Failure::Refused(reason::UNSUPPORTED_LAYOUT)),
        }
    }

    async fn activate(
        &self,
        ctx: &Ctx<'_>,
        install: &Install,
        staged: &Staged,
        act: &Activation,
        target: &str,
        env: &ChildEnv,
    ) -> Result<(), Failure> {
        match (&install.tool, staged) {
            (Tool::Native { link, .. }, Staged::Native { placed }) => {
                activate_native(link, &placed.path, ctx.attempt_id).map_err(io_failed)?;
                verify_native(ctx, &install.launcher, &placed.path, target, env).await
            }
            (
                Tool::Npm { modules, .. } | Tool::Bun { modules, .. },
                Staged::Codex { root, sm, .. },
            ) => {
                let (m, s, r, mv) = (modules.clone(), sm.clone(), root.clone(), act.moved.clone());
                self.mutations
                    .run(move || activate_codex(&m, &s, &r, &mv))
                    .await
                    .map_err(io_failed)?
                    .map_err(io_failed)?;
                verify_codex(ctx, &install.launcher, modules, target, env).await
            }
            _ => Err(Failure::Refused(reason::UNSUPPORTED_LAYOUT)),
        }
    }

    async fn restore(
        &self,
        ctx: &Ctx<'_>,
        rec: &RecoveryRecord,
        env: &ChildEnv,
    ) -> Result<(), Failure> {
        let tool = journal_tool(rec).ok_or(Failure::Failed(RESTORE_FAILED))?;
        restore(ctx, rec, &tool, env).await
    }

    async fn settle(&self, limit: std::time::Duration) -> bool {
        self.mutations.settle(limit).await
    }
}

/// The real target resolution (§3.5).
pub struct RealChannel {
    programs: Programs,
    tmp_root: PathBuf,
}

impl RealChannel {
    #[must_use]
    pub fn new(station_dir: &Path) -> Self {
        Self {
            programs: Programs::default(),
            tmp_root: station_dir.join("cli-update/gpg"),
        }
    }
}

#[async_trait]
impl Channel for RealChannel {
    async fn resolve(
        &self,
        install: &Install,
        pin: Option<&str>,
        channel: &str,
    ) -> Result<Verified, &'static str> {
        match (install.kind, &install.tool) {
            (
                ProviderKind::Claude,
                Tool::Native {
                    gpgv: Some(gpgv), ..
                },
            ) => {
                std::fs::create_dir_all(&self.tmp_root).map_err(|_| reason::SIGNATURE_INVALID)?;
                resolve_claude(&self.programs, gpgv, &self.tmp_root, pin, channel).await
            }
            (ProviderKind::Claude, Tool::Native { .. }) => Err(reason::GPGV_MISSING),
            (ProviderKind::Codex, _) => resolve_codex(&self.programs, pin).await,
            _ => Err(reason::REPORT_ONLY_METHOD),
        }
    }
}

/// The manager side of an attempt. It holds the manager weakly: the manager
/// holds the service.
pub struct ManagerHost {
    manager: Weak<SessionManager>,
}

#[async_trait]
impl Host for ManagerHost {
    async fn live_sessions(&self, kind: ProviderKind) -> Vec<SessionId> {
        match self.manager.upgrade() {
            Some(m) => m.live_sessions_of(kind).await,
            None => Vec::new(),
        }
    }

    async fn begin_hold(&self, kind: ProviderKind) -> Result<Hold, &'static str> {
        let m = self.manager.upgrade().ok_or(reason::STATION_RESTARTING)?;
        let hold = m.begin_provider_update(kind).await?;
        Ok(Box::new(hold))
    }

    async fn installed(&self, kind: ProviderKind) -> Option<String> {
        let m = self.manager.upgrade()?;
        m.providers()
            .await
            .into_iter()
            .find(|p| p.provider == kind)
            .and_then(|p| p.version)
    }

    async fn refresh(&self) {
        if let Some(m) = self.manager.upgrade() {
            m.refresh_providers().await;
        }
    }

    fn quarantine(&self, kind: ProviderKind, text: &str) {
        if let Some(m) = self.manager.upgrade() {
            m.quarantine_provider(kind, text);
        }
    }

    fn release_quarantine(&self, kind: ProviderKind) {
        if let Some(m) = self.manager.upgrade() {
            m.release_provider_quarantine(kind);
        }
    }
}

/// The live `[provider_updates]` values: `station.toml` is read at each
/// call; a file that does not parse keeps the values of the start.
pub struct FileConfig {
    path: PathBuf,
    secrets: PathBuf,
    fallback: ProviderUpdatesConfig,
}

impl FileConfig {
    #[must_use]
    pub fn new(path: PathBuf, secrets: PathBuf, fallback: ProviderUpdatesConfig) -> Self {
        Self {
            path,
            secrets,
            fallback,
        }
    }
}

impl ConfigSource for FileConfig {
    fn current(&self) -> ProviderUpdatesConfig {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| {
                StationConfig::parse_with_store(&text, &Env::from_process(), Some(&self.secrets))
                    .ok()
            })
            .map_or_else(|| self.fallback.clone(), |c| c.provider_updates)
    }
}

/// The configured provider instances, with their binary and environment.
#[must_use]
pub fn targets_from_config(config: &StationConfig) -> Vec<ProviderTarget> {
    let base = ChildEnv::from_process();
    let mut out = Vec::new();
    for i in &config.provider_instances {
        let kind = match i.driver {
            fleetdeck_core::config::Driver::Claude => ProviderKind::Claude,
            fleetdeck_core::config::Driver::Codex => ProviderKind::Codex,
        };
        match ChildEnv::for_instance(base.clone(), i) {
            Ok(env) => out.push(ProviderTarget {
                kind,
                binary: i.binary.clone(),
                env,
            }),
            Err(e) => tracing::warn!(instance = %i.id, error = %e, "no update for this instance"),
        }
    }
    out
}

/// §3.7.2 for a station start: the real installer and the configured
/// instances. It runs before `SessionManager::start`.
pub async fn recover_at_start(
    store: &Store,
    config: &StationConfig,
    station_dir: &Path,
    ids: &SharedIds,
) {
    let targets = targets_from_config(config);
    let installer = RealInstaller::new();
    recover(&Recovery {
        store,
        installer: &installer,
        targets: &targets,
        station_dir,
        curl: &Programs::default().curl,
        ids,
        cfg: &config.provider_updates,
        limit: super::recover::RECOVER_LIMIT,
    })
    .await;
}

/// What [`CliUpdates::new`] needs.
pub struct ServiceParts<'a> {
    pub manager: &'a Arc<SessionManager>,
    pub config: &'a StationConfig,
    pub source: Arc<dyn ConfigSource>,
    pub station_dir: &'a Path,
    pub ids: SharedIds,
    /// The station hosts sessions in a shim.
    pub shim_host: bool,
}

/// The update service of the station.
pub struct CliUpdates {
    engine: Arc<Engine>,
    schedule: Arc<Schedule>,
}

impl CliUpdates {
    #[must_use]
    pub fn new(p: ServiceParts<'_>) -> Arc<Self> {
        let programs = Programs::default();
        let engine = Arc::new(Engine::new(Parts {
            store: p.manager.store().clone(),
            host: Arc::new(ManagerHost {
                manager: Arc::downgrade(p.manager),
            }),
            channel: Arc::new(RealChannel::new(p.station_dir)),
            installer: Arc::new(RealInstaller::new()),
            config: p.source,
            targets: targets_from_config(p.config),
            station_dir: p.station_dir.to_path_buf(),
            ids: p.ids,
            shim_host: p.shim_host,
            enabled: p.config.provider_updates.enabled,
            curl: programs.curl,
        }));
        let schedule = Arc::new(Schedule::new(Arc::clone(&engine), p.config.name.clone()));
        Arc::new(Self { engine, schedule })
    }

    #[must_use]
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// `cli_update.status`.
    pub async fn report(&self) -> CliUpdateReport {
        self.engine.report().await
    }

    /// Starts the schedule task (§3.9).
    #[must_use]
    pub fn start_schedule(&self, shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
        tokio::spawn(Arc::clone(&self.schedule).run(shutdown))
    }
}

fn bad(e: impl std::fmt::Display) -> (&'static str, String) {
    ("invalid_params", e.to_string())
}

fn value(v: impl serde::Serialize) -> Result<Value, (&'static str, String)> {
    serde_json::to_value(v).map_err(|e| ("internal", e.to_string()))
}

/// The three link methods. `Err` is the wire error code and text.
///
/// # Errors
/// `unsupported` without a service, `invalid_params`, `internal`.
pub async fn call(
    svc: Option<Arc<CliUpdates>>,
    method: &str,
    params: Value,
) -> Result<Value, (&'static str, String)> {
    let Some(svc) = svc else {
        return Err((
            "unsupported",
            "provider updates are not available on this station".to_owned(),
        ));
    };
    match method {
        "cli_update.status" => value(svc.report().await),
        "cli_update.run" => {
            let run: CliUpdateRun = serde_json::from_value(params).map_err(bad)?;
            let answer: CliUpdateRunAnswer = svc
                .engine
                .run_attempt(
                    run.provider,
                    UpdateTrigger::Owner,
                    run.check_only,
                    run.force,
                )
                .await;
            value(answer)
        }
        "cli_update.attempts" => {
            let q: CliUpdateAttemptsQuery = serde_json::from_value(params).map_err(bad)?;
            let store = &svc.engine.store;
            let rows = store
                .cli_update_attempts(q.after_seq, q.limit.clamp(1, 100))
                .await
                .map_err(|e| ("internal", e.to_string()))?;
            value(CliUpdateAttemptsPage {
                incarnation: store.incarnation().clone(),
                rows,
            })
        }
        other => Err((
            "not_implemented",
            format!("`{other}` is not served by this station build"),
        )),
    }
}

/// `true` when the station hosts sessions in a shim.
#[must_use]
pub fn shim_host(config: &StationConfig, effective: HostChoice) -> bool {
    config.host == HostChoice::Shim && effective == HostChoice::Shim
}

impl Engine {
    /// The update state of `kind`. `install` is the detection, or `None`
    /// when it is not at hand.
    pub async fn state_of(
        &self,
        kind: ProviderKind,
        install: Option<&Install>,
    ) -> ProviderUpdateState {
        let cfg = self.config.current();
        let found = match install {
            Some(i) => Ok(i.clone()),
            None => self.install_of(kind),
        };
        let (method, blocked) = match &found {
            Ok(i) => (i.method, i.blocked),
            Err(r) => (InstallMethod::Unknown, Some(*r)),
        };
        let check = self.checks.lock().ok().and_then(|c| c.get(&kind).cloned());
        ProviderUpdateState {
            provider: kind,
            method,
            updatable: blocked.is_none(),
            blocked: blocked.map(str::to_owned),
            channel: channel_text(kind, &cfg),
            installed: self.host.installed(kind).await,
            available: check.as_ref().and_then(|c| c.available.clone()),
            checked_at: check.map(|c| c.at),
            last: self.store.cli_update_last(kind).await.ok().flatten(),
            quarantined: self.quarantine_reason(kind).await,
        }
    }

    /// `cli_update.status`.
    pub async fn report(&self) -> CliUpdateReport {
        let cfg = self.config.current();
        let mut providers = Vec::new();
        for kind in [ProviderKind::Claude, ProviderKind::Codex] {
            if self.primary(kind).is_some() {
                providers.push(self.state_of(kind, None).await);
            }
        }
        let start = cfg.window_start_min;
        let end = (u32::from(start) + u32::from(cfg.window_hours) * 60) % (24 * 60);
        CliUpdateReport {
            enabled: self.enabled,
            window: format!(
                "{}-{:02}:{:02} {}",
                cfg.window_start_text(),
                end / 60,
                end % 60,
                cfg.timezone.text()
            ),
            providers,
            running: self.running_id(),
            last_seq: self.store.cli_update_last_seq().await.unwrap_or(0),
        }
    }

    pub(super) async fn answer(
        &self,
        kind: ProviderKind,
        install: Option<&Install>,
        attempt_id: Option<String>,
        refused: Option<&str>,
        busy: Vec<SessionId>,
    ) -> CliUpdateRunAnswer {
        CliUpdateRunAnswer {
            state: self.state_of(kind, install).await,
            attempt_id,
            refused: refused.map(str::to_owned),
            busy,
        }
    }
}

impl Engine {
    /// §3.8.1 `verify_current`. `Ok` clears the quarantine; `Err` is the
    /// reason and the quarantine stays. A pass writes one `verified` row.
    ///
    /// # Errors
    /// A code of [`reason`].
    pub(super) async fn verify_inner(
        &self,
        id: &str,
        kind: ProviderKind,
        trigger: UpdateTrigger,
    ) -> Result<(), &'static str> {
        if self.has_journal(kind).await {
            return Err(reason::RECOVERY_PENDING);
        }
        let install = self.install_of(kind)?;
        let probe = self
            .probe_launcher(id, kind, &install)
            .await
            .map_err(|f| f.reason())?;
        let version = probe.version.clone().ok_or(reason::NOT_FOUND)?;
        if version_status(kind, Some(&version)) == VersionStatus::Broken {
            return Err(reason::KNOWN_BROKEN);
        }
        if let Tool::Native { gpgv: Some(_), .. } = &install.tool {
            let verified = self
                .channel
                .resolve(&install, Some(&version), "stable")
                .await?;
            let Verified::Native {
                expected_sha256, ..
            } = verified
            else {
                return Err(reason::SIGNATURE_INVALID);
            };
            let real = install.real.clone();
            let sum = tokio::task::spawn_blocking(move || sha256_file(&real)).await;
            if !matches!(sum, Ok(Ok(s)) if s == expected_sha256) {
                return Err(reason::CHECKSUM_MISMATCH);
            }
        }
        if probe.handshake.is_err() {
            return Err(reason::HANDSHAKE_FAILED);
        }
        let cfg = self.config.current();
        let attempt = new_attempt(
            id,
            kind,
            trigger,
            install.method,
            &channel_text(kind, &cfg),
            Some(&version),
            None,
        );
        self.store
            .cli_update_verified(
                attempt,
                end_of(UpdateOutcome::Verified, None, Some(&version)),
            )
            .await
            .map_err(|_| reason::INSTALL_FAILED)?;
        self.host.release_quarantine(kind);
        self.host.refresh().await;
        Ok(())
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
