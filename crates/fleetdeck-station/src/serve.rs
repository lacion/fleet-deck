//! `fleetdeck station serve` (M2 P2-7, P3-5c): the station process. It
//! opens `station.db`, recovers it, starts the session manager with the
//! adapters of `station.toml`, loads (or creates) the station identity key,
//! starts the link node (listeners and dialers of the paired deck,
//! [`crate::link::StationNode`]) and serves the local socket until the
//! shutdown signal. At shutdown it stops every live session gracefully.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_core::config::{HostChoice, StationConfig};
use fleetdeck_core::identity::{Identity, IdentityError};
use fleetdeck_core::paths::{Env, NodeDirs, ensure_private_dir};
use fleetdeck_core::restart::Restart;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::command::StopMode;
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, SessionHost};
use tokio::sync::watch;

use crate::fleet_git_runtime::FleetGitRuntime;
use crate::link::{NodeError, StationNode};
use crate::local;
use crate::manager::target::ShimSettings;
use crate::manager::{Policy, SessionManager, adapters_from_config, setup_envs_from_config};
use crate::store::{Store, StoreConfig};
use crate::workspace::{PrivateEnv, PrivatePaths};

/// The host that a station runs: `shim` needs Linux for now (shim parity,
/// step 0), so a macOS station runs the direct host.
fn effective_host(host: HostChoice, is_macos: bool) -> HostChoice {
    if is_macos && host == HostChoice::Shim {
        HostChoice::Direct
    } else {
        host
    }
}

/// How long the shutdown waits for the pumps to store the last events.
pub const PUMP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// How often the station scans its credential locations again for new
/// identities (DL-173, PRE-PR r3 finding 1).
pub const CREDENTIAL_RESCAN: std::time::Duration = std::time::Duration::from_secs(60);

/// Scans the credential locations of `private` every `every` until
/// `shutdown` is set. The first scan ran when the set was built.
pub async fn rescan_credentials(
    private: Arc<PrivatePaths>,
    every: std::time::Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let set = Arc::clone(&private);
                if let Err(e) = tokio::task::spawn_blocking(move || set.rescan()).await {
                    tracing::warn!(error = %e, "the credential rescan failed");
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// The wait before the first history prune (NV-26).
pub const PRUNE_FIRST: std::time::Duration = std::time::Duration::from_secs(60);

/// The time between two history prunes (NV-26).
pub const PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Prunes the event log, the receipts and the finished history rows of `store`
/// after `first`, then every `every`, until `shutdown` is set (NV-26). A
/// failed prune is logged and the task goes on.
pub async fn prune_history(
    store: Store,
    first: std::time::Duration,
    every: std::time::Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + first, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => match store.prune().await {
                Ok(report) => tracing::debug!(
                    events = report.events,
                    receipts = report.receipts,
                    history = report.history,
                    "the history prune ran"
                ),
                Err(e) => tracing::warn!(error = %e, "the history prune failed"),
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// Marks the activated updates in `dir` live. A record that cannot be read or
/// saved stops the start: a rollback must not restore a binary that the live
/// database already left behind (NV-29).
fn mark_live_or_fail(dir: &std::path::Path) -> Result<(), ServeError> {
    crate::upgrade::mark_live(dir)
        .map(|_| ())
        .map_err(|error| ServeError::Upgrade {
            dir: dir.to_owned(),
            error,
        })
}

/// Why the station cannot start.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("station directory `{path}`: {source}")]
    Dir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    #[error("provider instance environment: {0}")]
    ChildEnv(#[from] fleetdeck_core::childenv::ChildEnvError),
    #[error(transparent)]
    Local(#[from] local::LocalError),
    #[error("station identity key: {0}")]
    Identity(#[from] IdentityError),
    #[error("station link: {0}")]
    Link(#[from] NodeError),
    #[error("shim host: the station executable is unknown: {0}")]
    ShimExe(std::io::Error),
    #[error("account store: {0}")]
    Accounts(#[from] crate::accounts::store::StoreError),
    #[error(
        "the update records in {dir} cannot be marked live: {error}. \
         Fix or remove the named file, then start the station again.",
        dir = .dir.display()
    )]
    Upgrade {
        dir: PathBuf,
        #[source]
        error: crate::upgrade::UpgradeError,
    },
}

/// The shim host of `host = "shim"` (ADR-005 §1, §5.1): this executable
/// is the shim (`fleetdeck shim`), the journals live in the station
/// directory, the sockets in `$XDG_RUNTIME_DIR` (or the private temporary
/// directory), and the output buffer is `shim_buffer_mib`.
fn shim_settings(
    config: &StationConfig,
    station_dir: &std::path::Path,
    store: &Store,
) -> Result<ShimSettings, ServeError> {
    let exe = fleetdeck_providers::host::shim::exe_hash::own_exe().map_err(ServeError::ShimExe)?;
    let mut host = ShimHostConfig::new(
        exe,
        station_dir.to_owned(),
        store.incarnation().as_str().to_owned(),
    );
    host.buffer_bytes = usize::try_from(config.shim_buffer_mib)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024 * 1024);
    Ok(ShimSettings::new(Arc::new(ShimHost::new(host))))
}

/// Names the MCP bridge of the shim host (DL-194 §9.2, §9.3): the stable
/// service link `<station dir>/bin/current` (or this executable in a
/// development run) and the station socket. A path that is too long gives
/// no bridge, so a shim session has no tools.
fn set_bridge_endpoint(
    manager: &SessionManager,
    station_dir: &std::path::Path,
) -> Result<(), ServeError> {
    let link = station_dir.join("bin").join("current");
    let program = if std::fs::symlink_metadata(&link).is_ok() {
        link
    } else {
        std::env::current_exe().map_err(ServeError::ShimExe)?
    };
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    let incarnation = manager.store().incarnation().as_str().to_owned();
    let socket = crate::coordinator::socket::mcp_socket_path(
        runtime_dir.as_deref(),
        station_dir,
        &std::env::temp_dir(),
        &incarnation,
    );
    match socket {
        Some(socket) => manager
            .coordinator_bindings()
            .set_bridge(crate::coordinator::BridgeEndpoint { program, socket }),
        None => tracing::warn!("socket_path_too_long: a shim session has no coordinator tools"),
    }
    Ok(())
}

/// Runs the station until `stop` completes.
///
/// # Errors
/// [`ServeError`] at start; a running station only logs errors.
pub async fn serve(
    dirs: &NodeDirs,
    config: &StationConfig,
    restart: Option<Arc<dyn Restart>>,
    stop: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let station = RunningStation::start(dirs, config, None, restart).await?;
    stop.await;
    station.stop().await;
    Ok(())
}

/// A started station: the session manager, the link node and the local
/// socket (M2 P4-4: `fleetdeck up` runs it next to the deck).
#[derive(Debug)]
pub struct RunningStation {
    pub manager: Arc<SessionManager>,
    pub node: Arc<StationNode>,
    pub identity: Arc<Identity>,
    server: tokio::task::JoinHandle<()>,
    shutdown: watch::Sender<bool>,
    socket: PathBuf,
    shells: Arc<crate::shell::Shells>,
    /// The model-help watcher (R10, DL-167).
    assist: tokio::task::JoinHandle<()>,
    /// The periodic scan of the credential locations (DL-173).
    rescan: tokio::task::JoinHandle<()>,
    /// The periodic prune of the event log and the history (NV-26).
    prune: tokio::task::JoinHandle<()>,
    /// The sampler of the per-session load (R13 §10).
    loads: tokio::task::JoinHandle<()>,
    /// The Fleet Git writer, push trigger task, push loop (FG-2) and cleanup loop (FG-3).
    fleet_git: Vec<tokio::task::JoinHandle<()>>,
}

/// The longest wait for a Fleet Git task at the station stop.
const FLEET_GIT_STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether Fleet Git accepts local remotes: the `station.toml` key, or the
/// phase-1 env bridge `FLEETDECK_ALLOW_LOCAL_REMOTES` that is exactly `1`
/// (FG-5: T2.5 moves the test rig to station.toml and removes this bridge).
fn allow_local_remotes(key: bool, env: Option<&str>) -> bool {
    key || env == Some("1")
}

/// The Git options of a Fleet Git write: the program, and under Linux the
/// confinement with the allowed roots of the current policy (DL-200), read at
/// each job.
fn fleet_git_options(
    hub: Arc<crate::manager::PolicyHub>,
) -> crate::workspace::fleet_git::OptionsProvider {
    #[cfg(target_os = "linux")]
    let exe = fleetdeck_providers::host::shim::exe_hash::own_exe().ok();
    Arc::new(move || {
        #[allow(unused_mut)]
        let mut options = crate::workspace::GitOptions::isolated();
        #[cfg(target_os = "linux")]
        if let Some(exe) = &exe {
            options.confine = Some(crate::workspace::git::Confine {
                exe: exe.clone(),
                roots: hub.current().allowed_roots().to_vec(),
            });
        }
        #[cfg(not(target_os = "linux"))]
        let _ = &hub;
        options
    })
}

impl RunningStation {
    /// Opens and recovers `station.db`, starts the manager, the link node
    /// and the local socket. `deck` is the deck config file and the deck
    /// data directory when the deck runs in this process (`fleetdeck up`);
    /// they are private paths of the station too (DL-173). `restart` is the
    /// restart request of the process (DL-172); without it `station.restart`
    /// answers `unsupported`.
    ///
    /// # Errors
    /// [`ServeError`].
    pub async fn start(
        dirs: &NodeDirs,
        config: &StationConfig,
        deck: Option<(PathBuf, PathBuf)>,
        restart: Option<Arc<dyn Restart>>,
    ) -> Result<Self, ServeError> {
        let station_dir = dirs.station();
        ensure_private_dir(&station_dir).map_err(|source| ServeError::Dir {
            path: station_dir.clone(),
            source,
        })?;
        // From here the live database advances: a rollback of an activated
        // update is binary-only (ADR-005 §3.3, DL-128).
        mark_live_or_fail(&station_dir.join("upgrades"))?;
        let clock: SharedClock = Arc::new(SystemClock);
        let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
        let store = Store::open(
            &dirs.station_db(),
            clock,
            Arc::clone(&ids),
            StoreConfig::default(),
        )
        .await?;
        let host: Arc<dyn SessionHost> = Arc::new(DirectChildHost::new());
        let adapters = adapters_from_config(config, &host, Some(station_dir.join("raw")))?;
        let socket = local::socket_path(&station_dir);
        let listener = local::bind(&socket).await?;
        // The disk report measures the data directory (DL-193).
        crate::resources::set_data_dir(station_dir.clone());
        // DL-188 step 0: a macOS station with `host = "shim"` runs the
        // direct host, and its policy does not name the shim host.
        let host_choice = effective_host(config.host, cfg!(target_os = "macos"));
        if config.host == HostChoice::Shim && host_choice != HostChoice::Shim {
            tracing::warn!("the shim host needs Linux for now; this station runs the direct host");
        }
        let mut policy =
            Policy::from_config(config).with_setup_envs(setup_envs_from_config(config)?);
        if host_choice != config.host {
            policy = policy.without_shim_host();
        }
        // (FG-5: T2.5 moves the test rig to station.toml and removes this bridge)
        policy = policy.with_allow_local_remotes(allow_local_remotes(
            config.allow_local_remotes,
            std::env::var("FLEETDECK_ALLOW_LOCAL_REMOTES")
                .ok()
                .as_deref(),
        ));
        // The station's own credential and config locations (DL-173): the
        // set loads and scans its inode registry here, before the manager
        // starts (its turn checkpoints and reverts refuse these files,
        // FDV2-003), before any store writes a credential and before the
        // link starts.
        let private = Arc::new(PrivatePaths::new(
            dirs,
            Env::from_process().home.as_deref(),
            deck.as_ref().map(|(c, d)| (c.as_path(), d.as_path())),
            &PrivateEnv::from_process(),
        ));
        let manager = Arc::new(match host_choice {
            HostChoice::Direct => {
                SessionManager::start(store, policy, adapters, ids, Arc::clone(&private)).await?
            }
            HostChoice::Shim => {
                let shims = shim_settings(config, &station_dir, &store)?;
                SessionManager::start_with_shims(
                    store,
                    policy,
                    adapters,
                    ids,
                    shims,
                    Arc::clone(&private),
                )
                .await?
            }
        });
        if host_choice == HostChoice::Shim && cfg!(target_os = "linux") {
            set_bridge_endpoint(&manager, &station_dir)?;
        }
        // Worktree and clone provisioning runs Git under `fleetdeck
        // confine-exec` (B9, DL-200).
        #[cfg(target_os = "linux")]
        match fleetdeck_providers::host::shim::exe_hash::own_exe() {
            Ok(exe) => manager.set_confine_exe(exe),
            Err(e) => tracing::warn!(
                error = %e,
                "the station cannot find its own executable: worktree and clone provisioning is checked after Git, not confined"
            ),
        }
        // The credential store of the connected accounts (DL-173): mode
        // 0700, durable before the first account is written.
        manager.integrations().accounts().attach(
            crate::accounts::store::AccountStore::open(&station_dir.join("accounts"))?
                .with_private(Arc::clone(&private)),
        );
        let assist = Arc::new(crate::assist::Assist::new(
            Arc::clone(manager.policy_hub()),
            manager.store().clone(),
            assist_instances(config),
        ));
        manager.set_assist(Arc::clone(&assist));
        manager.set_settings(Arc::new(crate::settings_svc::StationSettings::new(
            &dirs.station_config(),
            &station_dir.join("secrets"),
            Env::from_process(),
            restart,
            Arc::clone(&private),
        )));
        let (shutdown, rx) = watch::channel(false);
        let rescan = tokio::spawn(rescan_credentials(private, CREDENTIAL_RESCAN, rx.clone()));
        let prune = tokio::spawn(prune_history(
            manager.store().clone(),
            PRUNE_FIRST,
            PRUNE_EVERY,
            rx.clone(),
        ));
        let loads = tokio::spawn(crate::resources::resources_sessions::run(
            Arc::clone(&manager),
            crate::resources::resources_sessions::SESSION_LOAD_EVERY,
            rx.clone(),
        ));
        let assist_task = tokio::spawn(crate::assist::watch::run(
            assist,
            Arc::clone(&manager),
            rx.clone(),
        ));
        let (mut fleet_git, land) = crate::manager::fleet_git_push::start(
            manager.store().clone(),
            Arc::clone(manager.policy_hub()),
            fleet_git_options(Arc::clone(manager.policy_hub())),
            rx.clone(),
        );
        manager.set_land_engine(land);
        let identity = Arc::new(Identity::load_or_create(&station_dir)?);
        // A station restart ends every shell (PLAN §6.8, A10): the PTYs
        // close with the process.
        let shells = Arc::new(crate::shell::Shells::new(crate::shell::ShellConfig {
            program: None,
            instances: terminal_instances(config),
        }));
        // The runtime of Fleet Git homes, traits and the config digest.
        let fleet_runtime = Arc::new(FleetGitRuntime::new(fleet_git_options(Arc::clone(
            manager.policy_hub(),
        ))));
        fleet_git.push(tokio::spawn(crate::drift::run(
            Arc::clone(&fleet_runtime),
            Arc::clone(&manager),
            Env::from_process().home,
            crate::drift::DRIFT_EVERY,
            rx.clone(),
        )));
        let artifacts =
            crate::taildrive::start(&fleet_runtime, &manager, config, &station_dir, &rx);
        fleet_git.extend(artifacts.await);
        // A clone of an earlier process did not finish: its folder stays, the
        // attempt is `interrupted` (design §8.4).
        let interrupted = manager.store().home_interrupt_cloning().await?;
        if interrupted > 0 {
            tracing::warn!(interrupted, "home clones stopped by the restart");
        }
        fleet_git.extend(crate::manager::fleet_git_cleanup::start(
            manager.store().clone(),
            Arc::clone(manager.policy_hub()),
            Arc::clone(&shells),
            fleet_git_options(Arc::clone(manager.policy_hub())),
            rx.clone(),
        ));
        let node = StationNode::start_with_fleet_git(
            Arc::clone(&manager),
            Arc::clone(&identity),
            config.name.clone(),
            &config.listen,
            BackoffPolicy::default(),
            Arc::clone(&shells),
            Some(station_dir.join("paste")),
            Some(station_dir.join("artifacts")),
            fleet_runtime,
        )
        .await?;
        tracing::info!(
            station = %config.name,
            key = %identity.fingerprint().short(),
            socket = %socket.display(),
            listen = ?node.listen_addrs().await,
            "station ready"
        );
        let server = tokio::spawn(local::serve(
            listener,
            Arc::clone(&manager),
            Some(Arc::clone(&node)),
            rx,
        ));
        Ok(Self {
            manager,
            node,
            identity,
            server,
            shutdown,
            socket,
            shells,
            assist: assist_task,
            rescan,
            prune,
            loads,
            fleet_git,
        })
    }

    /// Stops the local socket and the link node, then every live session
    /// gracefully.
    pub async fn stop(self) {
        tracing::info!("station stopping");
        let _ = self.shutdown.send(true);
        let _ = self.server.await;
        let _ = self.assist.await;
        let _ = self.rescan.await;
        let _ = self.prune.await;
        let _ = self.loads.await;
        for mut task in self.fleet_git {
            if tokio::time::timeout(FLEET_GIT_STOP_WAIT, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
        self.node.shutdown();
        // The stop waits for the terminal processes (up to the hangup grace).
        let shells = Arc::clone(&self.shells);
        if let Err(e) = tokio::task::spawn_blocking(move || shells.close_all()).await {
            tracing::warn!(error = %e, "shell close");
        }
        self.manager.shutdown(StopMode::Graceful).await;
        // The pumps store the last events of each process (`session.ended`).
        let deadline = tokio::time::Instant::now() + PUMP_GRACE;
        while self.manager.live_sessions() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        if let Err(e) = std::fs::remove_file(&self.socket) {
            tracing::debug!(error = %e, "socket file");
        }
    }
}

/// The provider instances that the model help can call (R10, DL-167): each
/// with its binary and its scrubbed environment. An instance whose
/// environment does not build is left out.
fn assist_instances(config: &StationConfig) -> Vec<crate::assist::AssistInstance> {
    let base = fleetdeck_core::childenv::ChildEnv::from_process();
    let mut out = Vec::new();
    for instance in &config.provider_instances {
        match fleetdeck_core::childenv::ChildEnv::for_instance(base.clone(), instance) {
            Ok(env) => out.push(crate::assist::AssistInstance {
                id: instance.id.clone(),
                driver: instance.driver,
                binary: instance.binary.clone(),
                env,
            }),
            Err(e) => tracing::warn!(
                instance = %instance.id,
                error = %e,
                "no model help for this provider instance"
            ),
        }
    }
    out
}

/// The provider instances that a terminal handover can start (DL-140): each
/// with its binary and its scrubbed environment. An instance whose
/// environment does not build has no terminal.
fn terminal_instances(
    config: &StationConfig,
) -> std::collections::BTreeMap<fleetdeck_proto::ProviderInstanceId, crate::shell::TerminalInstance>
{
    let base = fleetdeck_core::childenv::ChildEnv::from_process();
    let mut out = std::collections::BTreeMap::new();
    for instance in &config.provider_instances {
        match fleetdeck_core::childenv::ChildEnv::for_instance(base.clone(), instance) {
            Ok(env) => {
                out.insert(
                    instance.id.clone(),
                    crate::shell::TerminalInstance {
                        driver: instance.driver,
                        binary: instance.binary.clone(),
                        env,
                    },
                );
            }
            Err(e) => tracing::warn!(
                instance = %instance.id,
                error = %e,
                "no terminal handover for this provider instance"
            ),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::private_paths::private_paths_tests::private_of;

    #[test]
    fn effective_host_runs_the_direct_host_for_shim_on_macos_only() {
        use HostChoice::{Direct, Shim};
        assert_eq!(effective_host(Direct, true), Direct);
        assert_eq!(effective_host(Direct, false), Direct);
        assert_eq!(effective_host(Shim, true), Direct);
        assert_eq!(effective_host(Shim, false), Shim);
    }

    #[tokio::test]
    async fn the_rescan_task_registers_new_credentials_and_stops_with_the_station() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let private = Arc::new(private_of(&home));
        let late = home.join(".ssh/id_late");
        std::fs::write(&late, "secret").unwrap();
        let meta = std::fs::metadata(&late).unwrap();
        assert!(!private.is_private_file(&meta));
        let (stop, rx) = watch::channel(false);
        let task = tokio::spawn(rescan_credentials(
            Arc::clone(&private),
            std::time::Duration::from_millis(10),
            rx,
        ));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        while !private.is_private_file(&meta) {
            assert!(tokio::time::Instant::now() < deadline, "not registered");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        stop.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn the_prune_task_prunes_the_history_and_stops_on_shutdown() {
        let det = fleetdeck_testkit::Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        store
            .create_session(crate::store::NewSession {
                session_id: "s1".into(),
                provider: fleetdeck_proto::event::ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: crate::store::OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        assert!(
            store
                .artifact_suggest("s1".into(), "a.md".to_owned())
                .await
                .unwrap()
        );
        assert!(
            !store
                .artifact_suggest("s1".into(), "a.md".to_owned())
                .await
                .unwrap()
        );
        det.advance(std::time::Duration::from_secs(91 * 24 * 60 * 60));
        let (stop, rx) = watch::channel(false);
        let task = tokio::spawn(prune_history(
            store.clone(),
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(10),
            rx,
        ));
        // The old suggestion is gone, so the same path is new again.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !store
            .artifact_suggest("s1".into(), "a.md".to_owned())
            .await
            .unwrap()
        {
            assert!(tokio::time::Instant::now() < deadline, "not pruned");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        stop.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn a_malformed_update_record_stops_the_live_marker() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("u1.json"), "{ not json").unwrap();
        let err = mark_live_or_fail(tmp.path()).unwrap_err();
        assert!(matches!(err, ServeError::Upgrade { .. }), "{err:?}");
        let text = err.to_string();
        assert!(text.contains("cannot be marked live"), "{text}");
        assert!(text.contains("u1.json"), "{text}");
        assert!(text.contains("start the station again"), "{text}");
        // No upgrades directory is not an error.
        mark_live_or_fail(&tmp.path().join("none")).unwrap();
    }

    #[tokio::test]
    async fn the_station_does_not_open_its_database_when_the_live_marker_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = NodeDirs {
            config: tmp.path().join("config"),
            data: tmp.path().join("data"),
        };
        let upgrades = dirs.station().join("upgrades");
        std::fs::create_dir_all(&upgrades).unwrap();
        std::fs::write(upgrades.join("u1.json"), "{ not json").unwrap();
        let env = Env {
            home: Some(tmp.path().to_owned()),
            ..Env::default()
        };
        let config = StationConfig::load(&dirs.station_config(), &env).unwrap();
        let err = RunningStation::start(&dirs, &config, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServeError::Upgrade { .. }), "{err:?}");
        assert!(!dirs.station_db().exists());
    }
    #[test]
    fn the_env_bridge_ors_with_the_key_and_is_exactly_one() {
        assert!(!allow_local_remotes(false, None));
        assert!(allow_local_remotes(true, None));
        assert!(allow_local_remotes(false, Some("1")));
        assert!(allow_local_remotes(true, Some("0")));
        for other in ["", "0", "true", "yes", " 1", "11"] {
            assert!(!allow_local_remotes(false, Some(other)), "{other:?}");
        }
    }

    #[tokio::test]
    async fn a_station_restart_marks_a_cloning_home_attempt_interrupted() {
        use crate::store::home_attempts::{BeginHome, NewHomeAttempt};
        use fleetdeck_proto::fleet_git::{HomeAttemptState, code};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let dirs = NodeDirs {
            config: home.join("config"),
            data: home.join("data"),
        };
        ensure_private_dir(&dirs.station()).unwrap();
        let dest = home.join("projects").join("app");
        {
            // The earlier process: a clone started, then the station stopped.
            let det = fleetdeck_testkit::Det::new();
            let store = Store::open(
                &dirs.station_db(),
                det.clock(),
                det.ids(),
                StoreConfig::default(),
            )
            .await
            .unwrap();
            let new = NewHomeAttempt {
                attempt_id: "a1".to_owned(),
                project_id: "p1".to_owned(),
                generation: 1,
                request_digest: "d".to_owned(),
                retry_of: None,
                dest_path: dest.display().to_string(),
                url: "https://h.example/acme/app".to_owned(),
                remote_name: "origin".to_owned(),
            };
            assert!(matches!(
                store.home_begin(new).await.unwrap(),
                BeginHome::Created(_)
            ));
        }
        let env = Env {
            home: Some(home.clone()),
            ..Env::default()
        };
        let text = format!(
            "allowed_roots = [\"{}\"]\nhost = \"direct\"\n",
            home.display()
        );
        let config = StationConfig::parse(&text, &env).unwrap();
        let station = RunningStation::start(&dirs, &config, None, None)
            .await
            .unwrap();
        let row = station
            .manager
            .store()
            .home_attempt("p1".into(), "a1".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, HomeAttemptState::Interrupted);
        assert_eq!(row.code.as_deref(), Some(code::CLONE_INTERRUPTED));
        assert!(row.message.unwrap().contains(&dest.display().to_string()));
        station.stop().await;
    }
}
