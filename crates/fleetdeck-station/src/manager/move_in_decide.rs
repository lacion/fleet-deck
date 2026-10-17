//! The decision of `SessionCommand::MoveIn` (FG-9, design §12.3 step 4, T4.2c):
//! pure database work in the transaction of the command. It makes the new
//! session, its `session_git` row (`created_branch = 0`, no publication ref),
//! the `move_in_runs` row `staged`, the `move_in` hold and the events. It
//! queues no effect: the fetch, the worktree, the publication ref, the import
//! and the launch are the `MoveIn` effect (T4.2d).

use std::path::{Path, PathBuf};

use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, SessionCommand, WorkspaceRequest, normalize_title,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{MoveInState, MoveKind, code};
use fleetdeck_proto::{Event, RejectCode, SessionId};

use super::super::new_callsign;
use super::{check, record, refuse};
use crate::coordinator::Bindings;
use crate::manager::decide::{reject, result};
use crate::manager::land::move_in::DISCARDED_TEXT;
use crate::manager::land::move_session::{check_move_id, check_origin};
use crate::manager::{fleet_control, policy::Policy};
use crate::store::git_state;
use crate::store::move_runs::{self, NewMoveIn, Staging};
use crate::store::move_staging::{self, StagingState};
use crate::store::session_holds::{self, KIND_MOVE_IN};
use crate::store::{DecideError, Decision, NewSession};
use crate::workspace::fleet_git::check_publication_values;
use crate::workspace::provision;

/// The longest station id of the source (the id rule of a station).
const MAX_STATION_ID: usize = 64;
/// The longest callsign hint, in bytes.
const MAX_CALLSIGN: usize = 64;

fn invalid(text: impl std::fmt::Display) -> DecideError {
    reject(
        RejectCode::InvalidCommand,
        format!("{}: {text}", code::INVALID_PARAMS),
    )
}

/// A hint that is 1 to 64 characters of `[a-z0-9-]` (it goes into a folder
/// name and a ref name).
fn callsign_ok(hint: &str) -> bool {
    (1..=MAX_CALLSIGN).contains(&hint.len())
        && hint
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Decides `session.move_in` (design §12.3 step 4, "Decision"). The checks run
/// in this order: origin and authority, the move id, the tombstone, a row of
/// the id, the kind, the provider instance, the Fleet Git snapshot, the
/// publication values, the upload of a native move, the session limit.
///
/// # Errors
/// A stored rejection, or a store error.
pub(in crate::manager) fn decide_in(
    d: &mut Decision<'_>,
    policy: &Policy,
    bindings: &Bindings,
    origin: CommandOrigin,
    command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    check_origin(origin)?;
    fleet_control::check(d, policy, bindings, &command)?;
    let SessionCommand::MoveIn {
        move_id,
        kind,
        provider_instance,
        title,
        callsign_hint,
        from,
        fleet_git,
    } = command
    else {
        return Err(reject(
            RejectCode::Unsupported,
            "this command is not a move in",
        ));
    };
    check_move_id(&move_id)?;
    let conn = d.connection();
    if move_runs::is_tombstoned(conn, &move_id)? {
        return Err(refuse(code::MOVE_DISCARDED, DISCARDED_TEXT));
    }
    if move_runs::get_in(conn, &move_id)?.is_some() {
        return Err(invalid("This move id exists already."));
    }
    if kind == MoveKind::Unknown {
        return Err(refuse(
            "unsupported_mode",
            "This station does not know this move kind. Update the station.",
        ));
    }
    let provider = policy.instance(&provider_instance).ok_or_else(|| {
        reject(
            RejectCode::UnknownProvider,
            format!("`{provider_instance}` is not a provider instance of this station"),
        )
    })?;
    let fg = *fleet_git;
    if fg.kind != fleetdeck_proto::fleet_git::StartKind::Worktree {
        return Err(invalid("a move in needs a worktree Fleet Git start"));
    }
    let publication = &from.publication;
    check_publication_values(publication).map_err(|r| invalid(r.text))?;
    if !(1..=MAX_STATION_ID).contains(&from.station_id.as_str().len()) {
        return Err(invalid("The source station id is not valid."));
    }
    // The Fleet Git snapshot of the station (the checks of a start), with the
    // verified home as the folder and a worktree request for `B`.
    let cwd = git_state::project_row(conn, &fg.project_id)?
        .and_then(|row| row.canonical_path)
        .map_or_else(|| PathBuf::from(&fg.home_path), PathBuf::from);
    let request = WorkspaceRequest::Worktree {
        branch: Some(publication.local_branch.clone()),
        base: None,
    };
    let home = check(d, policy, &fg, &cwd, Some(&request))?;
    let conn = d.connection();
    let native = native_of(conn, &move_id, kind, provider)?;
    let active = d.active_sessions()?;
    if active >= u64::from(policy.max_sessions()) {
        return Err(reject(
            RejectCode::Forbidden,
            format!(
                "this station already runs its maximum of {} sessions",
                policy.max_sessions()
            ),
        ));
    }
    let title = normalize_title(title.as_deref()).map_err(invalid)?;
    let session_id: SessionId = d.new_id();
    let callsign = match callsign_hint.filter(|hint| callsign_ok(hint)) {
        Some(hint) if !d.callsign_taken(&hint)? => hint,
        _ => new_callsign(d, &session_id, active, None)?,
    };
    let inside = |p: &Path| policy.workspace(&p.to_string_lossy()).is_ok();
    let (plan, path) = provision::worktree_plan(
        &home,
        &callsign,
        Some(publication.local_branch.as_str()),
        None,
        inside,
    )
    .map_err(|r| {
        let code = if r.forbidden {
            RejectCode::Forbidden
        } else {
            RejectCode::InvalidCommand
        };
        reject(code, r.message)
    })?;
    let worktree = path.to_string_lossy().into_owned();
    d.create_session(&NewSession {
        session_id: session_id.clone(),
        provider,
        provider_instance,
        cwd: worktree.clone(),
        worktree: Some(worktree.clone()),
        branch: Some(publication.local_branch.clone()),
        mode: None,
        title,
        offline_approval_policy: policy.offline(),
    })?;
    d.append(
        &session_id,
        Event::SessionNamed {
            callsign,
            worktree: Some(worktree),
        }
        .into(),
    )?;
    // `created_branch = 0` and no publication ref until the publication
    // transaction (revision 4, NEW-1): `record` makes no push row for it.
    record(d, &session_id, &fg, Some(&plan), &home, false)?;
    let now = d.now();
    let new = NewMoveIn {
        move_id: move_id.clone(),
        command_id: d.command_id().as_str().to_owned(),
        session_id: session_id.clone(),
        provider,
        native_session_id: native.as_ref().map(|n| n.native_session_id.clone()),
        authority: d.authority().cloned(),
        source_station: Some(from.station_id.to_string()),
    };
    move_runs::insert_in(d.connection(), &new, now)?;
    if let Some(native) = &native {
        move_runs::set_staging(d.connection(), &move_id, &native.staging, now)?;
    }
    session_holds::insert(d.connection(), &session_id, KIND_MOVE_IN, &move_id, now)?;
    if let Some(row) = move_runs::get_in(d.connection(), &move_id)? {
        debug_assert_eq!(row.state, MoveInState::Staged);
        d.append(
            &session_id,
            Event::SessionMoveIn(move_runs::info_of(&row)).into(),
        )?;
    }
    Ok(result(&session_id, None))
}

/// The upload of a native move: its native id and staging folder.
struct Uploaded {
    native_session_id: String,
    staging: Staging,
}

/// A native move needs a staging record in `ready` whose manifest names the
/// provider of the instance (design §12.3 step 3). A branch-only move needs
/// none.
fn native_of(
    conn: &rusqlite::Connection,
    move_id: &str,
    kind: MoveKind,
    provider: ProviderKind,
) -> Result<Option<Uploaded>, DecideError> {
    if kind != MoveKind::Native {
        return Ok(None);
    }
    let ready = move_staging::get(conn, move_id)?.filter(|s| s.state == StagingState::Ready);
    let Some(staged) = ready else {
        return Err(refuse(
            code::MOVE_CHECKSUM,
            "The conversation files did not arrive complete. Try again.",
        ));
    };
    if staged.manifest.provider != provider {
        return Err(invalid(
            "The provider of the move does not match the provider instance.",
        ));
    }
    Ok(Some(Uploaded {
        native_session_id: staged.manifest.native_session_id,
        staging: Staging {
            path: staged.path,
            dev: staged.dev,
            ino: staged.ino,
        },
    }))
}
