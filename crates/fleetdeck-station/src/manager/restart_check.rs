//! What a restart of the station ends (DL-172 §1.6): the live sessions that
//! are hosted direct, the open shells and the running setup commands and
//! prechecks. A session under a shim is detached and kept.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::settings::{RestartItem, RestartKind};
use fleetdeck_proto::shell::ShellState;

use super::SessionManager;
use crate::store::Result;

/// The last folder name of a path, or the whole path when it has none.
fn folder(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// The reason of a queued start that a restart discards.
pub const QUEUED_START_LOST: &str = "the station restarted before this session started";

impl SessionManager {
    /// Lists what a restart would end now.
    ///
    /// # Errors
    /// A database error.
    pub(crate) async fn restart_would_stop(
        &self,
        shells: &crate::shell::Shells,
    ) -> Result<Vec<RestartItem>> {
        let mut items = Vec::new();
        for id in self.target.direct_sessions().await {
            let title = self.session_label(&id).await?;
            items.push(RestartItem {
                kind: RestartKind::Session,
                id: id.as_str().to_owned(),
                title,
            });
        }
        // An accepted start that the worker holds (the restart freeze) has
        // no session yet: the restart would end it (finding 3).
        for effect in self.store.queued_start_effects().await? {
            let label = self.session_label(&effect.session_id).await?;
            items.push(RestartItem {
                kind: RestartKind::Session,
                id: effect.session_id.as_str().to_owned(),
                title: format!("Queued start of {label}"),
            });
        }
        for shell in shells.list() {
            if shell.state == ShellState::Running {
                items.push(RestartItem {
                    kind: RestartKind::Shell,
                    title: format!("Shell in {}", folder(&shell.cwd)),
                    id: shell.shell_id,
                });
            }
        }
        for run in self.store.setup_runs().await? {
            let label = self.session_label(&run.session_id).await?;
            items.push(RestartItem {
                kind: RestartKind::Setup,
                id: run.session_id.as_str().to_owned(),
                title: format!("Setup command of {label}"),
            });
        }
        for (n, cwd) in crate::workspace::precheck::running() {
            items.push(RestartItem {
                kind: RestartKind::Setup,
                id: format!("precheck-{n}"),
                title: format!("Precheck in {}", folder(&cwd.to_string_lossy())),
            });
        }
        Ok(items)
    }

    /// Ends every queued start or resume that a restart now discards, as a
    /// failed start with [`QUEUED_START_LOST`]. The effect rows stay queued:
    /// the recovery at the next start retires them, and finds no session
    /// that is ambiguous. Call it after the consent and before the restart
    /// commits.
    ///
    /// # Errors
    /// A database error.
    pub(crate) async fn fail_queued_starts(&self) -> Result<usize> {
        let queued = self.store.queued_start_effects().await?;
        for effect in &queued {
            self.target
                .start_failed(&effect.session_id, QUEUED_START_LOST)
                .await;
        }
        Ok(queued.len())
    }

    /// The title of a session, else its callsign, else its id.
    async fn session_label(&self, id: &SessionId) -> Result<String> {
        let summary = self.store.summary(id.clone()).await?;
        Ok(summary
            .and_then(|s| {
                let named = |t: Option<String>| t.filter(|t| !t.trim().is_empty());
                named(s.title).or_else(|| named(s.callsign))
            })
            .unwrap_or_else(|| id.as_str().to_owned()))
    }
}
