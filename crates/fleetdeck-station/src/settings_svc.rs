//! The station handlers of `settings.get`, `settings.set`,
//! `settings.secret_set` and `station.restart` (DL-172, design 1.3, 1.4 and
//! 1.6), built on the settings core and on the live policy of the manager.
//!
//! - Every error has a fixed code. A message holds a key name and a fixed
//!   text, never a value. The secret value reaches no log, no error and no
//!   `Debug` output: a request that does not parse gets a fixed text, because
//!   a parse error of serde can quote the value.
//! - A save runs `prepare`, the file commit and `publish` under one gate, so
//!   two saves never interleave their prepare and publish steps.
//! - The core work is blocking (files, fsync): each call runs in
//!   `spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::paths::Env;
use fleetdeck_core::restart::Restart;
use fleetdeck_core::settings::{SaveError, SettingsWriter, StationHolder, view_station};
use fleetdeck_link::methods::code;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::settings::{
    RestartAnswer, RestartRequest, SecretSetParams, SettingsSetParams,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::manager::SessionManager;
use crate::shell::Shells;
use crate::workspace::PrivatePaths;

/// The wait between the answer of `station.restart` and the restart request.
pub const RESTART_DELAY: Duration = Duration::from_millis(200);

/// The longest wait of a restart admission for the starts in flight.
pub const RESTART_SETTLE: Duration = Duration::from_secs(10);

/// The settings of one station: the only writer of its config file while the
/// station runs.
pub struct StationSettings {
    path: PathBuf,
    store: PathBuf,
    env: Env,
    writer: Arc<SettingsWriter<StationHolder>>,
    /// Holds a save from its `prepare` to its `publish`.
    gate: Arc<std::sync::Mutex<()>>,
    /// Serializes the restart admissions (PRE-PR r1 finding 3).
    restart_gate: tokio::sync::Mutex<()>,
    /// How long an admission waits for the starts in flight.
    settle: Duration,
    restart: Option<Arc<dyn Restart>>,
    /// Gets the inode of the config file and of every secret file after a
    /// write (PRE-PR r2 finding 1).
    private: Arc<PrivatePaths>,
}

impl std::fmt::Debug for StationSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StationSettings")
            .field("path", &self.path)
            .field("restart", &self.restart.is_some())
            .finish_non_exhaustive()
    }
}

fn ok(value: impl Serialize) -> Outcome {
    match serde_json::to_value(value) {
        Ok(v) => Outcome::Ok(v),
        Err(_) => fixed("internal", "the answer could not be built"),
    }
}

fn fixed(code: &str, message: &str) -> Outcome {
    Outcome::Err(WireError::new(code, message))
}

fn unsupported(what: &str) -> Outcome {
    fixed("unsupported", what)
}

/// The wire error of a failed save: its code, its fixed text and the keys
/// of an elevation request.
fn save_failed(e: &SaveError) -> Outcome {
    let data = match e {
        SaveError::ElevationRequired { keys } => Some(json!({ "keys": keys })),
        _ => None,
    };
    Outcome::Err(WireError {
        code: e.code().to_owned(),
        message: e.to_string(),
        data,
    })
}

impl StationSettings {
    /// Creates the holder of the config file `path` with the secret store
    /// `store`, and collects the orphan secrets (design 1.1, U1). Call it
    /// before the link starts. After every save the config file and the
    /// secret directory are registered in `private`.
    #[must_use]
    pub fn new(
        path: &Path,
        store: &Path,
        env: Env,
        restart: Option<Arc<dyn Restart>>,
        private: Arc<PrivatePaths>,
    ) -> Self {
        let writer = SettingsWriter::<StationHolder>::new(path, Some(store), &env);
        let gone = writer.collect_orphans();
        if !gone.is_empty() {
            tracing::info!(count = gone.len(), "removed unused secret files");
        }
        Self {
            path: path.to_path_buf(),
            store: store.to_path_buf(),
            env,
            writer: Arc::new(writer),
            gate: Arc::default(),
            restart_gate: tokio::sync::Mutex::new(()),
            settle: RESTART_SETTLE,
            restart,
            private,
        }
    }

    /// Changes the wait of a restart admission (tests shorten it).
    #[must_use]
    pub fn with_settle(mut self, settle: Duration) -> Self {
        self.settle = settle;
        self
    }

    /// `settings.get`: the view of the config file. A secret is shown as
    /// `{ set }`.
    pub async fn get(&self) -> Outcome {
        let (path, store, env) = (self.path.clone(), self.store.clone(), self.env.clone());
        let task = tokio::task::spawn_blocking(move || view_station(&path, &env, &store, &[]));
        match task.await {
            Ok(Ok(view)) => ok(view),
            // A parse error can quote the file: only a fixed text goes out.
            Ok(Err(_)) | Err(_) => {
                tracing::warn!("settings: the config file cannot be read");
                fixed("internal", "the station settings cannot be read")
            }
        }
    }

    /// `settings.set`: saves a change and publishes it at once.
    pub async fn set(&self, manager: &Arc<SessionManager>, params: Value) -> Outcome {
        let params: SettingsSetParams = match serde_json::from_value(params) {
            Ok(p) => p,
            Err(_) => return fixed(code::PROTOCOL_ERROR, "params: the request is not valid"),
        };
        let (writer, gate, manager) = (
            Arc::clone(&self.writer),
            Arc::clone(&self.gate),
            Arc::clone(manager),
        );
        let registry = self.registrar();
        let task = tokio::task::spawn_blocking(move || {
            let _gate = gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved_to = writer.save(&params.change, params.elevated, |c| manager.prepare(c));
            // The rename may have happened also when the save reports an
            // error after it: register what is on disk in every case.
            registry();
            let (saved, _config, prepared) = saved_to?;
            // The rename is the commit point: publish also when the disk did
            // not confirm the write (design 1.3).
            manager.publish(prepared);
            Ok::<_, SaveError>(saved)
        });
        Self::answer(task.await)
    }

    /// `settings.secret_set`: stores a secret and points the config to it.
    pub async fn secret_set(&self, manager: &Arc<SessionManager>, params: Value) -> Outcome {
        // The text of a serde error can quote the value: say nothing of it.
        let Ok(params) = serde_json::from_value::<SecretSetParams>(params) else {
            return fixed(code::PROTOCOL_ERROR, "params: the request is not valid");
        };
        let (writer, gate, manager) = (
            Arc::clone(&self.writer),
            Arc::clone(&self.gate),
            Arc::clone(manager),
        );
        let registry = self.registrar();
        let task = tokio::task::spawn_blocking(move || {
            let _gate = gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved_to =
                writer.set_secret(&params.secret, &params.revision, params.elevated, |c| {
                    manager.prepare(c)
                });
            registry();
            let (saved, _config, prepared) = saved_to?;
            manager.publish(prepared);
            Ok::<_, SaveError>(saved)
        });
        Self::answer(task.await)
    }

    /// The step that registers the config file and the secret files in the
    /// inode registry: it runs under the save gate, after the write.
    fn registrar(&self) -> impl FnOnce() + Send + 'static {
        let (private, path, store) = (
            Arc::clone(&self.private),
            self.path.clone(),
            self.store.clone(),
        );
        move || {
            private.register(&path);
            private.register_dir(&store);
        }
    }

    fn answer(
        done: Result<
            Result<fleetdeck_proto::settings::SettingsSaved, SaveError>,
            tokio::task::JoinError,
        >,
    ) -> Outcome {
        match done {
            Ok(Ok(saved)) => ok(saved),
            Ok(Err(e)) => save_failed(&e),
            Err(_) => fixed("internal", "the save did not finish"),
        }
    }

    /// `station.restart`: lists what a restart ends. It asks the process to
    /// restart after the answer, when nothing is listed or when the caller
    /// accepts to stop it.
    ///
    /// The admission runs under the restart gate (PRE-PR r1): it freezes new
    /// sessions, waits for the starts in flight, and only then lists what
    /// the restart ends. A refusal or a failure unfreezes; the freeze stays
    /// after the answer `restarting: true`.
    pub async fn restart(
        &self,
        manager: &Arc<SessionManager>,
        shells: &Shells,
        params: Value,
    ) -> Outcome {
        let request: RestartRequest = match serde_json::from_value(params) {
            Ok(r) => r,
            Err(_) => return fixed(code::PROTOCOL_ERROR, "params: the request is not valid"),
        };
        if !request.elevated {
            return save_failed(&SaveError::ElevationRequired {
                keys: vec!["restart".to_owned()],
            });
        }
        let Some(handle) = self.restart.clone() else {
            return unsupported("this station cannot restart itself");
        };
        let _admission = self.restart_gate.lock().await;
        // The handoff of a deferred setup start waits for the end of this
        // check: the counts, the lists and the commit see one state
        // (PRE-PR r3 finding 2).
        let _handoff = manager.restart_state().lock_handoff().await;
        manager.begin_restart();
        if !manager.settle_starts(self.settle).await {
            manager.end_restart();
            return fixed("unavailable", "starts are still running; try again");
        }
        let would_stop = match manager.restart_would_stop(shells).await {
            Ok(list) => list,
            Err(_) => {
                manager.end_restart();
                return fixed("internal", "the restart list cannot be built");
            }
        };
        // B8: a shell command that runs is never stopped by a consent.
        match manager.shell_work_open().await {
            Ok(false) => {}
            Ok(true) => {
                manager.end_restart();
                return fixed(
                    "invalid_state",
                    "A shell command is running; wait for it to end or release it.",
                );
            }
            Err(_) => {
                manager.end_restart();
                return fixed("internal", "the restart list cannot be built");
            }
        }
        if !would_stop.is_empty() && !request.stop_direct_sessions {
            manager.end_restart();
            return ok(RestartAnswer {
                restarting: false,
                would_stop,
            });
        }
        // The accepted starts that the freeze holds end here, with a reason,
        // so the recovery after the restart finds nothing ambiguous.
        if manager.fail_queued_starts().await.is_err() {
            manager.end_restart();
            return fixed("internal", "the queued starts cannot be ended");
        }
        manager.commit_restart();
        tokio::spawn(async move {
            tokio::time::sleep(RESTART_DELAY).await;
            handle.request();
        });
        ok(RestartAnswer {
            restarting: true,
            would_stop: Vec::new(),
        })
    }
}

/// The link handler: dispatches one of the four methods. A station without
/// a settings holder answers `unsupported`.
pub async fn call(
    manager: &Arc<SessionManager>,
    shells: &Shells,
    method: &str,
    params: Value,
) -> Outcome {
    let Some(settings) = manager.settings() else {
        return unsupported("this station has no settings holder");
    };
    match method {
        "settings.get" => settings.get().await,
        "settings.set" => settings.set(manager, params).await,
        "settings.secret_set" => settings.secret_set(manager, params).await,
        "station.restart" => settings.restart(manager, shells, params).await,
        other => fixed(
            crate::link::NOT_IMPLEMENTED,
            &format!("`{other}` is not served by this station build"),
        ),
    }
}
