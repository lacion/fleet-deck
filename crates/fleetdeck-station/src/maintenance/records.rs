//! The records of the maintenance runs and their states (`board-gaps.md`
//! §2.9): a record is a valid spec and its state. The station never stops,
//! kills or restarts a unit; it only reads, and it writes the state `lost`.

use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use fleetdeck_proto::desktop_tools::{MaintenanceInfo, MaintenanceState, PostCheck, is_hex_id};

use super::files::{
    self, RunState, RunStateKind, Spec, lock_path, output_path, read_spec, read_state, spec_path,
    state_path,
};
use super::service::{Host, Unit};

/// A run with a spec and no state is `admitted` for this long at least:
/// the runner needs a moment to start.
pub(super) const STALE_ADMISSION_MS: i64 = 15_000;
/// The runs that `maintenance.list` answers and keeps.
pub(super) const KEEP: usize = 16;

/// Unix milliseconds now.
pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The ID of a spec file name (`<32 hex>.spec.json`), or `None`.
fn spec_id(name: &str) -> Option<&str> {
    name.strip_suffix(".spec.json").filter(|id| is_hex_id(id))
}

/// Every valid spec in `dir`, newest `created_at` first. A file that is not
/// a valid spec is skipped with a warning.
///
/// # Errors
/// The folder cannot be listed.
pub(super) fn list_specs(dir: &Path) -> io::Result<Vec<Spec>> {
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(id) = name.to_str().and_then(spec_id) else {
            continue;
        };
        match read_spec(dir, id) {
            Ok(spec) => specs.push(spec),
            Err(e) => tracing::warn!(error = %e, "a maintenance spec is skipped"),
        }
    }
    specs.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.maintenance_id.cmp(&a.maintenance_id))
    });
    Ok(specs)
}

/// The recorded state of `id`. A state file that fails its checks is
/// reported and counts as no state file.
fn recorded(dir: &Path, id: &str) -> Option<RunState> {
    match read_state(dir, id) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(error = %e, "a maintenance state file is not valid");
            None
        }
    }
}

/// The result of a recorded final state, or `None` for `running`.
fn final_of(state: &RunState) -> Option<(MaintenanceState, Option<i32>, Option<PostCheck>)> {
    match state.state {
        RunStateKind::Exited => Some((
            MaintenanceState::Exited,
            state.exit_code,
            state.post_check.clone(),
        )),
        RunStateKind::Lost => Some((MaintenanceState::Lost, None, None)),
        RunStateKind::Running => None,
    }
}

/// The unit is gone. A recorded final state wins (the runner writes it
/// before it ends, so after the answer `Gone` it is on disk). Else the run
/// is lost: the state `lost` is written.
fn settle_gone(dir: &Path, id: &str) -> (MaintenanceState, Option<i32>, Option<PostCheck>) {
    if let Some(done) = recorded(dir, id).as_ref().and_then(final_of) {
        return done;
    }
    let lost = RunState {
        v: 1,
        state: RunStateKind::Lost,
        exit_code: None,
        post_check: None,
        at: now_ms(),
    };
    if let Err(e) = files::write_state(dir, id, &lost) {
        tracing::warn!(error = %e, "the state lost was not written");
    }
    (MaintenanceState::Lost, None, None)
}

/// The state of a run, with its exit code and its post-check.
///
/// - The state file `exited` wins over everything; `lost` stays lost.
/// - `running`: the unit `Gone` makes the run `lost`; any other answer
///   leaves it `running`.
/// - No state file: the unit `Active` is `admitted`; `Gone` for a spec older
///   than 15 s is `lost`; anything else is `admitted`.
///
/// An unanswered query (`Unknown`) never makes a run `lost`.
pub(super) async fn state_of(
    host: &dyn Host,
    dir: &Path,
    spec: &Spec,
) -> (MaintenanceState, Option<i32>, Option<PostCheck>) {
    let id = &spec.maintenance_id;
    let state = recorded(dir, id);
    if let Some(done) = state.as_ref().and_then(final_of) {
        return done;
    }
    let unit = host.unit(id).await;
    if state.is_some() {
        // `running`.
        return if unit == Unit::Gone {
            settle_gone(dir, id)
        } else {
            (MaintenanceState::Running, None, None)
        };
    }
    if unit == Unit::Gone && now_ms().saturating_sub(spec.created_at) > STALE_ADMISSION_MS {
        return settle_gone(dir, id);
    }
    (MaintenanceState::Admitted, None, None)
}

/// The record of `spec` for the wire.
pub(super) async fn info_of(host: &dyn Host, dir: &Path, spec: &Spec) -> MaintenanceInfo {
    let (state, exit_code, post_check) = state_of(host, dir, spec).await;
    MaintenanceInfo {
        maintenance_id: spec.maintenance_id.clone(),
        request_id: spec.request_id.clone(),
        state,
        exit_code,
        post_check,
        created_at: spec.created_at,
        tools: spec.tools.clone(),
    }
}

/// Removes the files of a finished run: the output, the state and the lock
/// first, the spec last (a spec that stays is found again by the next
/// pruning). A socket path is never touched.
pub(super) fn remove_files(dir: &Path, id: &str) {
    for path in [
        output_path(dir, id),
        state_path(dir, id),
        lock_path(dir, id),
        spec_path(dir, id),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, "a maintenance file was not removed"),
        }
    }
}
