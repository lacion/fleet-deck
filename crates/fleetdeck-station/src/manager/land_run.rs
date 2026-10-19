//! The run of one land, steps 1 to 4 of design §11.5: the authority check
//! (a), the capture, the temporary worktree, the rebase and the check. Each
//! phase change is its own store transaction with its `session.land` event
//! (`store/land_runs.rs`); a transaction that finds the run fenced or ended
//! writes nothing, and the task stops.
//!
//! The run ends here with `conflict`, `check_failed`, `check_mutated` or
//! `failed`; after `checked` the publication goes on (`land_publish.rs`,
//! steps 5 to 7), and attempt 2 starts again at step 1 (FG-8).

use std::path::{Path, PathBuf};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::fleet_git::{
    GitSettings, HomeCheckState, LandPhase, LandState, check_settings, code,
};

use super::fleet_control::{self, Refusal};
use super::land::{Shared, land_text, oid7};
use crate::store::land_runs::{Change, End, LandRunRow};
use crate::store::{Effect, EffectId, EffectPayload, EffectState};
use crate::workspace::fleet_git::{
    self, Rebased, add_worktree, capture, check_home, ensure_dir, has_identity, rebase, redact,
    temp_path, temp_root, tested_unchanged,
};
use crate::workspace::setup;

/// The most lines of the check output in a failure text (O26).
const TAIL_LINES: usize = 50;
/// The most bytes of the check output in a failure text (O26).
const TAIL_BYTES: usize = 16 * 1024;

/// How a step ended without a result: the run ended (or was fenced), or the
/// station is stopping.
pub(super) enum Stop {
    /// The run ended in this step, or the fence stopped it: nothing more.
    Done,
}

pub(super) type Step<T> = Result<T, Stop>;

/// Runs one admitted land: steps 1 to 4, then the publication (steps 5 to 7).
pub(super) async fn run(shared: &Shared, row: LandRunRow) {
    let land_id = row.land_id.clone();
    // `Stop::Done` is the only error: the run ended or was stopped.
    if let Ok(row) = steps(shared, row).await {
        drive_loop(shared, row).await;
    }
    shared.locks.release(&land_id);
    tracing::debug!(land = %land_id, "the land task ended");
}

/// Drives a run from its stored state; attempt 2 runs steps 1 to 4 again
/// (design §11.5 step 7).
pub(super) async fn drive_loop(shared: &Shared, row: LandRunRow) {
    let mut next = super::land_publish::drive(shared, row).await;
    while let super::land_publish::Next::Again(row) = next {
        let row = *row;
        let Ok(row) = steps(shared, row).await else {
            return;
        };
        next = super::land_publish::drive(shared, row).await;
    }
}

/// Steps 1 to 4. The row that it returns is in phase `checked`.
pub(super) async fn steps(shared: &Shared, row: LandRunRow) -> Step<LandRunRow> {
    if !authority(shared, &row).await? {
        return Err(Stop::Done);
    }
    let (row, home) = home(shared, row).await?;
    let (row, captured) = captured(shared, row, &home).await?;
    let (row, temp) = worktree(shared, row, &home).await?;
    let row = rebased(shared, row, &temp, &captured.base_oid).await?;
    checked(shared, row, &temp).await
}

fn stopping(shared: &Shared) -> bool {
    *shared.shutdown.borrow()
}

/// Ends the run: one transaction with the terminal state. `Err` always: the
/// task has nothing more to do.
async fn end<T>(
    shared: &Shared,
    row: &LandRunRow,
    state: LandState,
    code: Option<&str>,
    first: &str,
    tail: Option<&str>,
    files: Option<Vec<String>>,
) -> Step<T> {
    let text = land_text(row, first, tail);
    let change = Change {
        files,
        end: Some(End {
            state,
            code: code.map(str::to_owned),
            text,
        }),
        ..Change::default()
    };
    match shared.store.land_advance(row.land_id.clone(), change).await {
        Ok(true) => {}
        Ok(false) => tracing::info!(land = %row.land_id, "the land was fenced; its end is dropped"),
        Err(e) => tracing::error!(land = %row.land_id, error = %e, "a land end was not stored"),
    }
    Err(Stop::Done)
}

/// A step that failed with a code (design §11.10).
async fn fail<T>(shared: &Shared, row: &LandRunRow, code: &str, text: &str) -> Step<T> {
    end(shared, row, LandState::Failed, Some(code), text, None, None).await
}

/// One phase change. `false` on `Ok`: the run is fenced or ended.
async fn advance(shared: &Shared, row: &LandRunRow, change: Change) -> Step<()> {
    match shared.store.land_advance(row.land_id.clone(), change).await {
        Ok(true) => {
            shared.cleanup.notify_one();
            Ok(())
        }
        Ok(false) => Err(Stop::Done),
        Err(e) => {
            tracing::error!(land = %row.land_id, error = %e, "a land phase was not stored");
            Err(Stop::Done)
        }
    }
}

async fn reload(shared: &Shared, row: &LandRunRow) -> Step<LandRunRow> {
    match shared.store.land_run(row.land_id.clone()).await {
        Ok(Some(row)) => Ok(row),
        _ => Err(Stop::Done),
    }
}

/// The synthetic effect of a run with authority (decision Q3, H3).
fn effect_of(row: &LandRunRow) -> Option<Effect> {
    let authority = row.authority.clone()?;
    Some(Effect {
        effect_id: EffectId::from(format!("land-{}", row.land_id)),
        command_id: Some(CommandId::from(row.command_id.clone())),
        session_id: row.session_id.clone(),
        process_incarnation: None,
        payload: EffectPayload::Land {
            land_id: row.land_id.clone(),
            authority: Some(authority),
        },
        state: EffectState::Claimed,
        fence_token: None,
        reason: None,
    })
}

/// The authority check of a run (points (a) to (d) of design §11.5): the
/// `coordinator.authority_check` round trip, then the station's own live
/// state. A run without authority passes.
///
/// # Errors
/// The [`Refusal`]: the authority ended, or the deck did not confirm it.
pub(super) async fn recheck_run(
    policy: &std::sync::Arc<super::policy_hub::PolicyHub>,
    row: &LandRunRow,
) -> Result<(), Refusal> {
    let Some(effect) = effect_of(row) else {
        return Ok(());
    };
    match policy.fleet_bindings() {
        Some(bindings) => {
            let hub = std::sync::Arc::clone(policy);
            let live = move || hub.current().allow_fleet_control();
            fleet_control::recheck(bindings, &live, &effect, None).await
        }
        None => Err(Refusal::Lost),
    }
}

/// The first sentence of an end by a lost or unconfirmed authority (design
/// §11.10).
pub(super) fn authority_first(refusal: Refusal) -> String {
    match refusal {
        Refusal::Lost => format!(
            "The coordinator lost its permission during the land ({}).",
            refusal.message()
        ),
        Refusal::Unknown => {
            "Fleet Deck could not confirm the coordinator's permission, so the land stopped."
                .to_owned()
        }
    }
}

/// Authority check (a) at the dispatch, and (c) before attempt 2 (design
/// §11.5): the synthetic effect of the run goes through `recheck`. `false`:
/// the run ended.
async fn authority(shared: &Shared, row: &LandRunRow) -> Step<bool> {
    let Err(refusal) = recheck_run(&shared.policy, row).await else {
        return Ok(true);
    };
    fail::<()>(shared, row, refusal.code(), &authority_first(refusal))
        .await
        .map(|()| false)
}

/// The text of a home that cannot be verified (design §5.7, §3.8).
fn home_refusal(
    row: &LandRunRow,
    state: HomeCheckState,
    check_code: Option<&str>,
) -> (String, String) {
    let remote = &row.plan.fetch_remote;
    match (state, check_code) {
        (HomeCheckState::Refused, Some(code::REMOTE_REWRITTEN)) => (
            code::REMOTE_REWRITTEN.to_owned(),
            format!(
                "The Git configuration on this station rewrites {} to another destination.",
                row.plan.remote.dest
            ),
        ),
        (HomeCheckState::Refused, Some(code::GIT_TOO_OLD)) => (
            code::GIT_TOO_OLD.to_owned(),
            "Fleet Git needs Git 2.45 or newer on this station.".to_owned(),
        ),
        _ => (
            code::REMOTE_MISMATCH.to_owned(),
            format!(
                "The remote {remote} of the home no longer points to {}.",
                row.plan.remote.repo
            ),
        ),
    }
}

/// The home folder of the run, verified (design §3.8; decision Q6). The home
/// check comes before the fetch.
async fn home(shared: &Shared, row: LandRunRow) -> Step<(LandRunRow, PathBuf)> {
    let project = shared.store.git_project(row.plan.project_id.clone()).await;
    let project = match project {
        Ok(Some(p)) => p,
        Ok(None) => {
            return fail(
                shared,
                &row,
                code::NOT_A_HOME,
                "This folder is not a verified home of the project.",
            )
            .await;
        }
        Err(e) => {
            return fail(
                shared,
                &row,
                code::INTERNAL,
                &format!("The land failed: {e}"),
            )
            .await;
        }
    };
    if project.home_generation != row.plan.home_generation {
        return fail(
            shared,
            &row,
            code::STALE_GENERATION,
            "The project home changed. Land again.",
        )
        .await;
    }
    let policy = shared.policy.current();
    let options = (shared.options)();
    let folder = PathBuf::from(
        project
            .canonical_path
            .as_ref()
            .unwrap_or(&project.home_path),
    );
    let check = check_home(
        &options,
        &folder,
        &project.view,
        policy.allow_local_remotes(),
    )
    .await;
    if check.state != HomeCheckState::Verified
        || row.plan.fetch_remote != project.view.settings.push_remote
    {
        let (code, text) = home_refusal(&row, check.state, check.code.as_deref());
        return fail(shared, &row, &code, &text).await;
    }
    Ok((row, folder))
}

/// Step 1: the capture (design §11.5). The values are stored in one
/// transaction with the phase `captured`.
async fn captured(
    shared: &Shared,
    row: LandRunRow,
    home: &Path,
) -> Step<(LandRunRow, fleet_git::Captured)> {
    let options = (shared.options)();
    let got = capture(
        &options,
        home,
        &row.plan.fetch_remote,
        row.plan.default_branch.as_deref(),
        &row.plan.branch_ref,
        row.source_oid.as_deref(),
    )
    .await;
    let got = match got {
        Ok(got) => got,
        Err(refusal) => {
            return fail(shared, &row, refusal.code, &refusal.text).await;
        }
    };
    let lease = match &row.branch_published_oid {
        Some(oid) => oid.clone(),
        None => match shared.store.land_last_pushed(row.session_id.clone()).await {
            Ok(oid) => oid.unwrap_or_default(),
            Err(e) => {
                return fail(
                    shared,
                    &row,
                    code::INTERNAL,
                    &format!("The land failed: {e}"),
                )
                .await;
            }
        },
    };
    let change = Change {
        phase: Some(LandPhase::Captured),
        default_ref: Some(got.default_ref.clone()),
        source_oid: Some(got.source_oid.clone()),
        base_oid: Some(got.base_oid.clone()),
        branch_lease: Some(lease),
        ..Change::default()
    };
    advance(shared, &row, change).await?;
    Ok((reload(shared, &row).await?, got))
}

/// Step 2: the temporary worktree (design §11.6).
async fn worktree(shared: &Shared, row: LandRunRow, home: &Path) -> Step<(LandRunRow, PathBuf)> {
    let policy = shared.policy.current();
    let Some(root) = temp_root(policy.allowed_roots(), home) else {
        return fail(
            shared,
            &row,
            code::INTERNAL,
            "The land failed: the home folder is outside the allowed roots.",
        )
        .await;
    };
    let dir = root.clone();
    let made = tokio::task::spawn_blocking(move || ensure_dir(&dir)).await;
    match made {
        Ok(Ok(())) => {}
        Ok(Err(refusal)) => return fail(shared, &row, refusal.code, &refusal.text).await,
        Err(e) => {
            return fail(
                shared,
                &row,
                code::INTERNAL,
                &format!("The land failed: {e}"),
            )
            .await;
        }
    }
    let temp = temp_path(&root, &row.land_id, row.attempt);
    let reserved = shared
        .store
        .land_temp_reserve(
            row.land_id.clone(),
            row.attempt,
            root.to_string_lossy().into_owned(),
            temp.to_string_lossy().into_owned(),
        )
        .await;
    if let Err(e) = reserved {
        return fail(
            shared,
            &row,
            code::INTERNAL,
            &format!("The land failed: {e}"),
        )
        .await;
    }
    let options = (shared.options)();
    let source = row.source_oid.clone().unwrap_or_default();
    let facts = match add_worktree(&options, home, &temp, &source).await {
        Ok(facts) => facts,
        Err(refusal) => return fail(shared, &row, refusal.code, &refusal.text).await,
    };
    let present = shared
        .store
        .land_temp_present(
            row.land_id.clone(),
            row.attempt,
            facts.identity,
            facts.admin_dir,
        )
        .await;
    if let Err(e) = present {
        return fail(
            shared,
            &row,
            code::INTERNAL,
            &format!("The land failed: {e}"),
        )
        .await;
    }
    let change = Change {
        phase: Some(LandPhase::WorktreeReady),
        ..Change::default()
    };
    advance(shared, &row, change).await?;
    Ok((reload(shared, &row).await?, temp))
}

/// Step 3: the rebase (design §11.5).
async fn rebased(
    shared: &Shared,
    row: LandRunRow,
    temp: &Path,
    base_oid: &str,
) -> Step<LandRunRow> {
    let options = (shared.options)();
    if !has_identity(&options, temp).await {
        return fail(
            shared,
            &row,
            code::LAND_NO_IDENTITY,
            "Git has no user.name or user.email on this station. Set them with git config --global.",
        )
        .await;
    }
    let done = match rebase(&options, temp, base_oid).await {
        Ok(done) => done,
        Err(refusal) => return fail(shared, &row, refusal.code, &refusal.text).await,
    };
    match done {
        Rebased::Done { rebased_oid } => {
            let change = Change {
                phase: Some(LandPhase::Rebased),
                rebased_oid: Some(rebased_oid),
                ..Change::default()
            };
            advance(shared, &row, change).await?;
            reload(shared, &row).await
        }
        Rebased::Conflict { files } => {
            let first = format!(
                "Conflict in {} files with {}/{} ({}){}.",
                files.len(),
                row.plan.fetch_remote,
                row.default_ref.as_deref().unwrap_or("the default branch"),
                oid7(base_oid),
                if row.attempt == 2 {
                    " on the second attempt"
                } else {
                    ""
                },
            );
            end(
                shared,
                &row,
                LandState::Conflict,
                None,
                &first,
                None,
                Some(files),
            )
            .await
        }
    }
}

/// The last 50 lines of the output, each redacted, at most 16 KiB (O26).
fn output_tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let from = lines.len().saturating_sub(TAIL_LINES);
    let mut tail = lines[from..]
        .iter()
        .map(|line| redact(line))
        .collect::<Vec<_>>()
        .join("\n");
    if tail.len() > TAIL_BYTES {
        let mut cut = TAIL_BYTES;
        while !tail.is_char_boundary(cut) {
            cut -= 1;
        }
        tail.truncate(cut);
    }
    tail
}

/// "the setup command failed (exit 3)" is "The check command failed (exit
/// 3)." (design §11.10).
fn check_sentence(message: &str, attempt: u32) -> String {
    let rest = message
        .strip_prefix("the setup command ")
        .map_or_else(|| format!("did not run ({message})"), str::to_owned);
    let second = if attempt == 2 {
        " on the second attempt"
    } else {
        ""
    };
    format!("The check command {rest}{second}.")
}

/// The environment of the check: the provider instance's setup environment
/// plus the names of `check_env` that the station accepts (decision Q10).
fn check_env(base: &ChildEnv, accepted: &std::collections::BTreeMap<String, String>) -> ChildEnv {
    let mut env = base.clone();
    for (name, value) in accepted.iter().take(32) {
        let one = GitSettings {
            worktree_env: std::iter::once((name.clone(), value.clone())).collect(),
            ..GitSettings::default()
        };
        if check_settings(&one).is_err() {
            tracing::warn!(variable = %name, "check_env: a refused variable is dropped");
        } else if env.get(name).is_none()
            && let Err(e) = env.set(name, value)
        {
            tracing::warn!(variable = %name, error = %e, "check_env: a variable is dropped");
        }
    }
    env
}

/// Step 4: the check (design §11.5). No check command: `checked` at once.
async fn checked(shared: &Shared, row: LandRunRow, temp: &Path) -> Step<LandRunRow> {
    let rebased_oid = row.rebased_oid.clone().unwrap_or_default();
    if let Some(cmd) = row.plan.check_cmd.clone() {
        let summary = shared.store.summary(row.session_id.clone()).await;
        let base = summary
            .ok()
            .flatten()
            .and_then(|s| s.provider_instance)
            .and_then(|instance| shared.policy.current().setup_env(&instance).cloned());
        let Some(base) = base else {
            return fail(
                shared,
                &row,
                code::INTERNAL,
                "The check command has no environment for this provider instance.",
            )
            .await;
        };
        let env = check_env(&base, &row.plan.check_env);
        advance(
            shared,
            &row,
            Change {
                phase: Some(LandPhase::Checking),
                ..Change::default()
            },
        )
        .await?;
        let marker = format!("{}-{}", row.land_id, row.attempt);
        // The longest check command (design §11.5 step 4): the station limit
        // `limits.land_check_minutes`, read once when the check starts.
        let limit = shared.policy.current().timeouts().land_check;
        let ran = setup::run(
            &shared.store,
            &marker,
            &row.session_id,
            &cmd,
            temp,
            &env,
            limit,
            shared.shutdown.clone(),
        )
        .await;
        if let Err(e) = ran {
            // A stop of the station leaves the run as it is.
            if stopping(shared) {
                return Err(Stop::Done);
            }
            let row = reload(shared, &row).await?;
            let first = check_sentence(&e.message, row.attempt);
            let tail = output_tail(&e.output_tail);
            return end(
                shared,
                &row,
                LandState::CheckFailed,
                None,
                &first,
                Some(&tail),
                None,
            )
            .await;
        }
        let options = (shared.options)();
        match tested_unchanged(&options, temp, &rebased_oid).await {
            Ok(true) => {}
            Ok(false) => {
                let row = reload(shared, &row).await?;
                return end(
                    shared,
                    &row,
                    LandState::CheckMutated,
                    Some(code::CHECK_MUTATED),
                    "The check command changed the tested commit or tracked files.",
                    None,
                    None,
                )
                .await;
            }
            Err(refusal) => return fail(shared, &row, refusal.code, &refusal.text).await,
        }
    }
    advance(
        shared,
        &row,
        Change {
            phase: Some(LandPhase::Checked),
            ..Change::default()
        },
    )
    .await?;
    reload(shared, &row).await
}
