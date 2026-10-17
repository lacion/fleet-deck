//! `fleet_git.projects_set` and `fleet_git.push_now` (Fleet Git phase 1,
//! design §3.3, §5.8, ADR-009). The control channel already checked the
//! method, the scope and the feature.

use std::path::Path;

use fleetdeck_proto::fleet_git::{
    GitProjectView, GitSnapshot, HomeCheck, HomeCheckState, PushNowAnswer, PushNowParams,
    SnapshotAck, code,
};
use fleetdeck_proto::link::{Outcome, WireError};

use super::{bad_params, internal, ok};
use crate::manager::SessionManager;
use crate::manager::policy::{Policy, WorkspaceError};
use crate::store::git_state::SnapshotResult;
use crate::workspace::GitOptions;
use crate::workspace::fleet_git::check_home;

/// The text of the code `unknown_session` (design §5.8).
const UNKNOWN_SESSION: &str = "The station does not know this session.";
/// The text of the code `not_synced`.
const NOT_SYNCED: &str = "This session is not in a synced project.";
/// The check code of a home folder outside the allowed roots.
const OUTSIDE_ROOTS: &str = "outside_roots";

/// Pauses the Fleet Git loops while a deck link without `fleet_git.v1` is up
/// (design §3.3). The loops go on when the guard is dropped (the link ended).
pub(in crate::link) struct PauseGuard(Option<std::sync::Arc<std::sync::atomic::AtomicBool>>);

impl PauseGuard {
    /// `has_fleet_git`: the link negotiated the feature, so nothing pauses.
    pub(in crate::link) fn new(
        flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
        has_fleet_git: bool,
    ) -> Self {
        if has_fleet_git {
            return Self(None);
        }
        flag.store(true, std::sync::atomic::Ordering::Release);
        Self(Some(std::sync::Arc::clone(flag)))
    }
}

impl Drop for PauseGuard {
    fn drop(&mut self) {
        if let Some(flag) = &self.0 {
            flag.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

/// The SHA-256 of the canonical snapshot JSON: the typed value, written by
/// `serde_json` (fields in the order of the type, maps sorted). The same value
/// always gives the same text (design §3.3).
fn digest_of(snapshot: &GitSnapshot) -> Result<String, serde_json::Error> {
    let json = serde_json::to_vec(snapshot)?;
    let hash = ring::digest::digest(&ring::digest::SHA256, &json);
    Ok(hash.as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// The check of one home (design §3.8 item 3). A folder outside the allowed
/// roots is `refused` with `outside_roots` before any Git runs.
async fn check_one(policy: &Policy, options: &GitOptions, view: &GitProjectView) -> HomeCheck {
    if let Err(WorkspaceError::Outside(_)) = policy.workspace(&view.home_path) {
        return HomeCheck {
            project_id: view.project_id.clone(),
            generation: view.home_generation,
            state: HomeCheckState::Refused,
            canonical_path: None,
            seen: None,
            code: Some(OUTSIDE_ROOTS.to_owned()),
        };
    }
    check_home(
        options,
        Path::new(&view.home_path),
        view,
        policy.allow_local_remotes(),
    )
    .await
}

/// `fleet_git.projects_set` (design §3.3): the full state of the station.
/// A snapshot with a lower revision, or the same revision with another digest,
/// is stale: `stored: false` with the station's own revision, nothing changes.
/// The same revision and digest is a retry: `stored: true` with the stored
/// checks. A newer snapshot is checked project by project, then replaces the
/// stored state in one transaction.
pub(super) async fn projects_set(manager: &SessionManager, params: serde_json::Value) -> Outcome {
    let snapshot: GitSnapshot = match serde_json::from_value(params) {
        Ok(s) => s,
        Err(e) => return bad_params(e),
    };
    let digest = match digest_of(&snapshot) {
        Ok(d) => d,
        Err(e) => return internal(e),
    };
    let store = manager.store();
    let stale = |revision| {
        ok(SnapshotAck {
            stored: false,
            revision,
            checks: Vec::new(),
        })
    };
    match store
        .git_snapshot_compare(snapshot.revision, digest.clone())
        .await
    {
        Ok(Some(SnapshotResult::Stale { revision })) => return stale(revision),
        Ok(Some(SnapshotResult::Same)) => {
            return match store.git_project_checks().await {
                Ok(checks) => ok(SnapshotAck {
                    stored: true,
                    revision: snapshot.revision,
                    checks,
                }),
                Err(e) => internal(e),
            };
        }
        Ok(_) => {}
        Err(e) => return internal(e),
    }
    let policy = manager.policy();
    let options = GitOptions::isolated();
    let checks = futures_util::future::join_all(
        snapshot
            .projects
            .iter()
            .map(|view| check_one(&policy, &options, view)),
    )
    .await;
    let revision = snapshot.revision;
    match store
        .git_apply_snapshot(snapshot, digest, checks.clone())
        .await
    {
        Ok(SnapshotResult::Stored | SnapshotResult::Same) => ok(SnapshotAck {
            stored: true,
            revision,
            checks,
        }),
        Ok(SnapshotResult::Stale { revision }) => stale(revision),
        Err(e) => internal(e),
    }
}

/// `fleet_git.push_now` (design §5.8): a `due` or `failing` row is due at
/// once. Error codes `unknown_session` and `not_synced`.
pub(super) async fn push_now(manager: &SessionManager, params: serde_json::Value) -> Outcome {
    let query: PushNowParams = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let store = manager.store();
    match store.summary(query.session_id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => return Outcome::Err(WireError::new(code::UNKNOWN_SESSION, UNKNOWN_SESSION)),
        Err(e) => return internal(e),
    }
    match store.git_push_now(query.session_id).await {
        Ok(Some(row)) => ok(PushNowAnswer { state: row.state }),
        Ok(None) => Outcome::Err(WireError::new(code::NOT_SYNCED, NOT_SYNCED)),
        Err(e) => internal(e),
    }
}

#[cfg(test)]
#[path = "intake_fleet_git_tests.rs"]
mod tests;
