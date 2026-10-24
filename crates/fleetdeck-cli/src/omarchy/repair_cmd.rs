//! The commands of the installer repair (`board-gaps.md` §3.4): `omarchy
//! install-plan`, `install --replace-conflicts --plan-stdin`, `install
//! --resume`, `repair-status`, `restore-plan` and `restore --plan-stdin`.
//! Stdout holds only JSON; every human line goes to stderr. Exit 0 done,
//! 1 a failure (a stderr line), 3 the Omarchy lock is busy, 4 a coded refusal
//! or stop (the JSON has the code).

use std::io::Read as _;
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_proto::omarchy_repair::{
    InstallPlanFacts, InstallPlanInput, PlanClassification, PlanFile, RepairFile, RepairFileState,
    RepairResult, RepairStatusView, RestorePlanInput, code,
};
use serde::Serialize;

use super::assets::asset_for;
use super::fsx::Home;
use super::install::{self, Say, Session, busy_exit};
use super::journal::{self, Journal};
use super::plan;
use super::record::{self, FileEntry};
use super::repair::{self, Halt, Replacement, Run};

/// The most bytes of a plan on stdin.
const MAX_STDIN: u64 = 64 * 1024;
/// The exit code of a coded refusal or stop.
const REFUSED: u8 = 4;
/// The journals that `repair-status` shows.
const STATUS_COUNT: usize = 8;

/// The stderr line of a repair ID that is not 32 lower-case hex characters.
pub const BAD_ID: &str = "the repair ID must be 32 lowercase hexadecimal characters.";

fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

/// Reads one JSON object from stdin (at most 64 KiB).
fn read_stdin<T: serde::de::DeserializeOwned>() -> anyhow::Result<T> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_STDIN + 1)
        .read_to_end(&mut bytes)
        .context("cannot read the plan from stdin")?;
    if bytes.len() as u64 > MAX_STDIN {
        bail!("the plan on stdin is larger than 64 KiB.");
    }
    serde_json::from_slice(&bytes).context("the plan on stdin is not valid")
}

fn check_id(id: &str) -> anyhow::Result<()> {
    if journal::valid_id(id) {
        Ok(())
    } else {
        bail!(BAD_ID)
    }
}

/// The result of a refusal that the JSON reports (exit 4).
fn refused(
    code: &str,
    repair_id: Option<String>,
    files: Vec<RepairFile>,
) -> anyhow::Result<ExitCode> {
    print_json(&RepairResult {
        code: Some(code.to_owned()),
        repair_id,
        files,
    })?;
    Ok(ExitCode::from(REFUSED))
}

/// Takes the lock for a read-only command. `None` when it is busy.
fn read_session() -> anyhow::Result<Option<(install::Found, super::lock::LockGuard)>> {
    let found = install::find()?;
    let Some(guard) = install::take_lock(&found.home)? else {
        return Ok(None);
    };
    Ok(Some((found, guard)))
}

fn load_journal(home: &Home, id: &str) -> anyhow::Result<Journal> {
    match journal::load(home, id) {
        Ok(Some(journal)) => Ok(journal),
        Ok(None) => bail!("there is no repair journal with this ID."),
        Err(e) => bail!("the repair journal {id} cannot be used: {e}"),
    }
}

/// `fleetdeck omarchy install-plan --json`.
///
/// # Errors
/// No Omarchy, an unreadable record or config home.
pub fn install_plan() -> anyhow::Result<ExitCode> {
    let Some((found, _guard)) = read_session()? else {
        return Ok(busy_exit());
    };
    let record = record::load(&found.home)?.unwrap_or_default();
    print_json(&plan::facts(&found.home, &record)?)?;
    Ok(ExitCode::SUCCESS)
}

/// The files of the replacement set: every unrecorded conflict and the
/// selected recorded edits that the facts hold, in the order of the facts.
fn replacement_set(input: &InstallPlanInput) -> Vec<&PlanFile> {
    input
        .facts
        .files
        .iter()
        .filter(|f| match f.classification {
            PlanClassification::UnrecordedConflict => true,
            PlanClassification::RecordedEdit => input.replace_recorded_edits.contains(&f.asset_id),
        })
        .collect()
}

fn plan_files(files: &[&PlanFile]) -> Vec<RepairFile> {
    files
        .iter()
        .map(|f| RepairFile {
            asset_id: f.asset_id,
            path: f.path.clone(),
            backup_path: None,
            state: RepairFileState::Unchanged,
            kept_path: None,
        })
        .collect()
}

/// The error of a stop that only a test can cause.
fn injected() -> anyhow::Error {
    anyhow::anyhow!("internal error: a test stop reached the command")
}

/// Records the files that a repair published, then runs the rest of the
/// install (stderr for the human lines). Prints the result on success.
async fn finish_repair(
    dirs: &NodeDirs,
    session: &mut Session,
    journal: &mut Journal,
) -> anyhow::Result<ExitCode> {
    for file in &journal.files {
        if let Some(asset) = file.asset() {
            session.record.upsert(FileEntry {
                path: asset.path.to_owned(),
                base: asset.base,
                sha256: file.asset_hash.clone(),
                existed_before: true,
            });
        }
    }
    let items = install::classify(&session.found.home, &session.record)?;
    if install::refuse_conflicts(&items) {
        return Ok(ExitCode::from(1));
    }
    let say = Say::stderr();
    let done = install::finish(dirs, session, &items, say, false).await?;
    if done != ExitCode::SUCCESS {
        return Ok(done);
    }
    // The journal is finished only now: until then a resume runs this part.
    journal.finished = true;
    journal::save(&session.found.home, journal)
        .context("cannot record that the repair finished")?;
    print_json(&RepairResult {
        code: None,
        repair_id: Some(journal.repair_id.clone()),
        files: repair::result_files(&session.found.home, journal, None),
    })?;
    Ok(ExitCode::SUCCESS)
}

/// `fleetdeck omarchy install --yes --replace-conflicts --plan-stdin`.
///
/// # Errors
/// No Omarchy, a failure that the JSON does not report, or an install step
/// after the replacement that failed.
pub async fn replace(dirs: &NodeDirs, input: InstallPlanInput) -> anyhow::Result<ExitCode> {
    let Some(mut session) = install::begin()? else {
        return Ok(busy_exit());
    };
    let set = replacement_set(&input);
    let unchanged = plan_files(&set);
    let facts: InstallPlanFacts = plan::facts(&session.found.home, &session.record)?;
    // 1 and 2. A premise of the plan changed: nothing is written.
    let selection_known = input.replace_recorded_edits.iter().all(|id| {
        input
            .facts
            .files
            .iter()
            .any(|f| f.asset_id == *id && f.classification == PlanClassification::RecordedEdit)
    });
    if facts != input.facts || !selection_known {
        return refused(code::PLAN_CHANGED, None, unchanged);
    }
    if set.iter().any(|f| !f.replaceable) {
        return refused(code::UNSAFE_PATH, None, unchanged);
    }
    // A file that the replacement does not cover must not stop the install
    // after the replacement: classify now, as the plain install does (an
    // unsafe path stops here, before any write).
    install::classify(&session.found.home, &session.record)?;
    // 3 and 4. The journal, then each file.
    if set.is_empty() {
        let items = install::classify(&session.found.home, &session.record)?;
        let done = install::finish(dirs, &mut session, &items, Say::stderr(), false).await?;
        if done == ExitCode::SUCCESS {
            print_json(&RepairResult {
                code: None,
                repair_id: None,
                files: Vec::new(),
            })?;
        }
        return Ok(done);
    }
    let mut reps = Vec::new();
    for f in &set {
        let Some(asset) = asset_for(f.asset_id) else {
            bail!("internal error: no asset for {}", f.asset_id.as_str());
        };
        reps.push(Replacement {
            asset,
            id: f.asset_id,
            original_hash: f.sha256.clone(),
        });
    }
    let journal = match repair::start(&session.found.home, &reps, &facts.build_hash) {
        Ok(journal) => journal,
        Err(Halt::Stop(c)) => return refused(c, None, unchanged),
        Err(Halt::Injected) => return Err(injected()),
    };
    let mut run = Run::new(&session.found.home, journal);
    let stopped = match run.advance_all(true) {
        Ok(code) => code,
        Err(_) => return Err(injected()),
    };
    if let Some(c) = stopped {
        return refused(
            c,
            Some(run.journal.repair_id.clone()),
            repair::result_files(&session.found.home, &run.journal, None),
        );
    }
    let mut journal = run.journal;
    finish_repair(dirs, &mut session, &mut journal).await
}

/// `fleetdeck omarchy install --resume <repair id>`.
///
/// # Errors
/// A bad ID, no journal, an invalid journal, or an install step after the
/// repair that failed.
pub async fn resume(dirs: &NodeDirs, id: &str) -> anyhow::Result<ExitCode> {
    check_id(id)?;
    let Some(mut session) = install::begin()? else {
        return Ok(busy_exit());
    };
    let journal = load_journal(&session.found.home, id)?;
    if journal.build_hash != plan::build_hash() {
        let files = repair::unchanged_files(&session.found.home, &journal, None);
        return refused(code::BUILD_MISMATCH, Some(id.to_owned()), files);
    }
    let mut run = Run::new(&session.found.home, journal);
    let stopped = match run.advance_all(false) {
        Ok(code) => code,
        Err(_) => return Err(injected()),
    };
    if let Some(c) = stopped {
        return refused(
            c,
            Some(id.to_owned()),
            repair::result_files(&session.found.home, &run.journal, None),
        );
    }
    if repair::all_published(&session.found.home, &run.journal) {
        let mut journal = run.journal;
        return finish_repair(dirs, &mut session, &mut journal).await;
    }
    // Every file was restored: there is nothing to finish.
    print_json(&RepairResult {
        code: None,
        repair_id: Some(id.to_owned()),
        files: repair::result_files(&session.found.home, &run.journal, None),
    })?;
    Ok(ExitCode::SUCCESS)
}

/// `fleetdeck omarchy repair-status --json`.
///
/// # Errors
/// No Omarchy, an unreadable config home or journal folder.
pub fn status() -> anyhow::Result<ExitCode> {
    let Some((found, _guard)) = read_session()? else {
        return Ok(busy_exit());
    };
    let home = &found.home;
    let mut journals = Vec::new();
    for name in home
        .list(std::path::Path::new(journal::DIR))?
        .unwrap_or_default()
    {
        let Some(id) = journal::id_of_name(&name) else {
            continue;
        };
        match journal::load(home, id) {
            Ok(Some(journal)) => journals.push(journal),
            Ok(None) => {}
            Err(e) => eprintln!("fleetdeck: warning: the repair journal {id} is skipped: {e}"),
        }
    }
    journals.sort_by(|a, b| (b.created_at, &b.repair_id).cmp(&(a.created_at, &a.repair_id)));
    journals.truncate(STATUS_COUNT);
    print_json(&RepairStatusView {
        journals: journals.iter().map(|j| repair::view(home, j)).collect(),
    })?;
    Ok(ExitCode::SUCCESS)
}

/// `fleetdeck omarchy restore-plan --repair <id> --json`.
///
/// # Errors
/// A bad ID, no journal, or an invalid journal.
pub fn restore_plan(id: &str) -> anyhow::Result<ExitCode> {
    check_id(id)?;
    let Some((found, _guard)) = read_session()? else {
        return Ok(busy_exit());
    };
    let journal = load_journal(&found.home, id)?;
    print_json(&repair::restore_facts(&found.home, &journal))?;
    Ok(ExitCode::SUCCESS)
}

/// `fleetdeck omarchy restore --plan-stdin`.
///
/// # Errors
/// A bad ID, no journal, or an invalid journal.
pub fn restore(input: &RestorePlanInput) -> anyhow::Result<ExitCode> {
    check_id(&input.facts.repair_id)?;
    let Some((found, _guard)) = read_session()? else {
        return Ok(busy_exit());
    };
    let home = &found.home;
    let id = input.facts.repair_id.clone();
    let journal = load_journal(home, &id)?;
    let mut selected = Vec::new();
    let mut unknown = false;
    for asset_id in &input.asset_ids {
        match journal.files.iter().position(|f| f.asset_id == *asset_id) {
            Some(i) if input.facts.files.iter().any(|f| f.asset_id == *asset_id) => {
                if !selected.contains(&i) {
                    selected.push(i);
                }
            }
            _ => unknown = true,
        }
    }
    selected.sort_unstable();
    if unknown || repair::restore_facts(home, &journal) != input.facts {
        let files = repair::unchanged_files(home, &journal, Some(&selected));
        return refused(code::PLAN_CHANGED, Some(id), files);
    }
    let mut run = Run::new(home, journal);
    let (stopped, changed) = run.restore_all(&selected).map_err(|_| injected())?;
    let mut files = repair::result_files(home, &run.journal, Some(&selected));
    for (file, index) in files.iter_mut().zip(&selected) {
        if changed.contains(index) {
            file.state = RepairFileState::Restored;
        }
    }
    print_json(&RepairResult {
        code: stopped.map(str::to_owned),
        repair_id: Some(id),
        files,
    })?;
    Ok(if stopped.is_some() {
        ExitCode::from(REFUSED)
    } else {
        ExitCode::SUCCESS
    })
}

/// Reads the stdin input of `install --replace-conflicts --plan-stdin`.
///
/// # Errors
/// The input is too large or not a valid plan.
pub fn read_install_input() -> anyhow::Result<InstallPlanInput> {
    read_stdin()
}

/// Reads the stdin input of `restore --plan-stdin`.
///
/// # Errors
/// The input is too large or not a valid plan.
pub fn read_restore_input() -> anyhow::Result<RestorePlanInput> {
    read_stdin()
}
