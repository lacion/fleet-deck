//! The Fleet Git part of the start decision (FG-1, FG-2, design §3.3, §4.2
//! step 1, §5.3): the checks of `FleetGitStart` against the station, the
//! session branch `fd/<slug>/<callsign>`, and the `session_git` and
//! `git_pushes` rows. All of it runs in the transaction of the command.

use std::path::{Path, PathBuf};

use fleetdeck_proto::command::WorkspaceRequest;
use fleetdeck_proto::fleet_git::{
    FleetGitStart, HomeCheckState, LocalRemotes, StartKind, remote_identity, station_slug,
};
use fleetdeck_proto::{RejectCode, SessionId};

use super::reject;
use crate::manager::policy::Policy;
use crate::store::git_state::{self, NewSessionGit};
use crate::store::{DecideError, Decision, SyncPlan, WorkspacePlan};

/// The decision of `MoveIn` (FG-9). Attached here so that `manager/mod.rs`
/// (799 lines) gets no line.
#[path = "move_in_decide.rs"]
pub(in crate::manager) mod move_in_decide;

/// The refusal of a Fleet Git start: `Reject` with `InvalidState` and the
/// message `<code>: <text>` (design §4.3).
fn refuse(code: &str, text: &str) -> DecideError {
    reject(RejectCode::InvalidState, format!("{code}: {text}"))
}

const UNVERIFIED: &str =
    "This folder is not a verified home of the project. Check it on the Projects page.";

/// Checks `fg` against the station (design §4.2 step 1): the kind, the slug,
/// the snapshot row of the project (revision, generation, home check) and
/// that `cwd` is the verified home or inside it. The first failing check
/// refuses the start. `cwd` is the canonical start folder. The answer is the
/// canonical home path.
///
/// # Errors
/// [`DecideError::Reject`] for a refused start.
pub(in crate::manager) fn check(
    d: &Decision<'_>,
    policy: &Policy,
    fg: &FleetGitStart,
    cwd: &Path,
    workspace: Option<&WorkspaceRequest>,
) -> Result<PathBuf, DecideError> {
    let worktree = matches!(workspace, Some(WorkspaceRequest::Worktree { .. }));
    match fg.kind {
        StartKind::Unknown => {
            return Err(refuse(
                "unsupported_mode",
                "This station does not know this start kind. Update the station.",
            ));
        }
        StartKind::Worktree if !worktree => {
            return Err(reject(
                RejectCode::InvalidCommand,
                "invalid_params: a worktree Fleet Git start needs a worktree request",
            ));
        }
        StartKind::InPlace if workspace.is_some() => {
            return Err(reject(
                RejectCode::InvalidCommand,
                "invalid_params: an in-place Fleet Git start takes no workspace request",
            ));
        }
        _ => {}
    }
    if station_slug(policy.name()) != fg.slug {
        return Err(refuse(
            "slug_mismatch",
            "The station name changed. Try again in one minute.",
        ));
    }
    let Some(row) = git_state::project_row(d.connection(), &fg.project_id)? else {
        return Err(refuse("home_unverified", UNVERIFIED));
    };
    if fg.settings_revision < row.settings_revision {
        return Err(refuse(
            "stale_settings",
            "The project settings changed while the start was on its way. Start again.",
        ));
    }
    if fg.home_generation != row.home_generation {
        return Err(refuse(
            "stale_generation",
            "The home of this project changed. Start again.",
        ));
    }
    let Some(home) = row
        .canonical_path
        .as_deref()
        .filter(|_| row.check_state == HomeCheckState::Verified)
    else {
        return Err(refuse("home_unverified", UNVERIFIED));
    };
    if !cwd.starts_with(home) {
        return Err(refuse(
            "not_a_home",
            "This folder is not a home of the project.",
        ));
    }
    Ok(PathBuf::from(home))
}

/// A start that gets its own branch: no owner branch in the request.
pub(in crate::manager) fn owns_branch(workspace: Option<&WorkspaceRequest>) -> bool {
    matches!(
        workspace,
        Some(WorkspaceRequest::Worktree { branch: None, .. })
    )
}

/// The worktree plan of a synced start (design §4.2 step 1): the new branch is
/// `fd/<slug>/<callsign>` and the plan carries the fetch. An owner branch
/// keeps the plan of the start as it is.
pub(in crate::manager) fn sync_plan(
    fg: &FleetGitStart,
    callsign: &str,
    mut plan: WorkspacePlan,
    owner_branch: bool,
) -> WorkspacePlan {
    if owner_branch {
        return plan;
    }
    if let WorkspacePlan::Worktree { branch, sync, .. } = &mut plan {
        *branch = format!("fd/{}/{callsign}", fg.slug);
        *sync = Some(SyncPlan {
            remote: fg.settings.push_remote.clone(),
            default_branch: fg.settings.default_branch.clone(),
        });
    }
    plan
}

/// Writes the `session_git` row and, for a session with its own branch, the
/// `git_pushes` row (design §3.5, §5.3). `plan` is the plan that the start
/// carries (`None` for a start in place).
///
/// # Errors
/// [`DecideError::Reject`] for a `wip_remote` that has no identity, or a
/// store error.
pub(in crate::manager) fn record(
    d: &Decision<'_>,
    session_id: &SessionId,
    fg: &FleetGitStart,
    plan: Option<&WorkspacePlan>,
    home: &Path,
    owns_branch: bool,
) -> Result<(), DecideError> {
    let (slug, branch) = match plan {
        Some(WorkspacePlan::Worktree { branch, .. }) => {
            (Some(fg.slug.clone()), Some(branch.clone()))
        }
        _ => (None, None),
    };
    let created = owns_branch && branch.is_some();
    let new = NewSessionGit {
        session_id: session_id.clone(),
        project_id: fg.project_id.clone(),
        kind: if branch.is_some() {
            "worktree"
        } else {
            "in_place"
        },
        settings_revision: fg.settings_revision,
        home_generation: fg.home_generation,
        slug,
        branch: branch.clone(),
        publish_ref: created.then(|| branch.clone()).flatten(),
        created_branch: created,
        repo_root: home.to_string_lossy().into_owned(),
        remote_dest: fg.remote.dest.clone(),
    };
    git_state::insert_session_git(d.connection(), &new, d.now())?;
    if let (true, Some(publish_ref)) = (created, &new.publish_ref) {
        // The push target (O9): the WIP URL when set, else the remote name.
        let (target, dest) = match fg.settings.wip_remote.as_deref() {
            Some(url) => {
                let identity = remote_identity(url, LocalRemotes::Allow).map_err(|e| {
                    reject(
                        RejectCode::InvalidCommand,
                        format!("invalid_git_settings: {e}"),
                    )
                })?;
                (url.to_owned(), identity.dest)
            }
            None => (fg.settings.push_remote.clone(), fg.remote.dest.clone()),
        };
        git_state::insert_push(
            d.connection(),
            session_id,
            (&target, &dest),
            publish_ref,
            d.now(),
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "fleet_git_start_tests.rs"]
mod tests;
