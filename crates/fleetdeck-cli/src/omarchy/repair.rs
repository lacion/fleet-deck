//! The journaled replacement, the resume and the restore of the installer
//! repair (`board-gaps.md` §3.5.1 and §3.6). The decision for each file
//! comes from the files on disk (the table of §3.5.1, with the correction
//! O12-28: a restore copies the kept copy and never renames it away), never
//! from the journal alone. Every action is idempotent. A staging file is
//! removed only under the name of the journal.

use std::os::fd::OwnedFd;

use fleetdeck_proto::omarchy_repair::{
    JournalState, ObservedState, RepairFile, RepairFileState, RepairJournalView, RepairStatusFile,
    RestoreAction, RestorePlanFacts, RestorePlanFile, code,
};

use super::assets::Asset;
use super::fsx::{Home, Kind};
use super::journal::{self, Journal, JournalFile, Step};
use super::record::{now_ms, sha256_hex};

/// Why a step stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Halt {
    /// The command stops with this code.
    Stop(&'static str),
    /// A test stopped the run at a seam (a simulated crash).
    #[cfg_attr(not(test), allow(dead_code))]
    Injected,
}

type Res<T> = Result<T, Halt>;

#[cfg(test)]
thread_local! {
    static STOP_AT: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    static HITS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The test seam: a crash can happen at every call. Outside the tests it does
/// nothing.
#[cfg(test)]
fn seam() -> Res<()> {
    let hit = HITS.with(|h| {
        h.set(h.get() + 1);
        h.get()
    });
    if STOP_AT.with(std::cell::Cell::get) == Some(hit) {
        Err(Halt::Injected)
    } else {
        Ok(())
    }
}

#[cfg(not(test))]
#[allow(clippy::unnecessary_wraps, reason = "the test build can stop here")]
fn seam() -> Res<()> {
    Ok(())
}

/// What a file of the target path holds now.
enum Seen {
    Absent,
    Hash(String),
    /// Not a regular file of the user, or not readable.
    Bad,
}

fn seen(home: &Home, rel: &std::path::Path) -> Seen {
    match home.read(rel) {
        Ok(None) => Seen::Absent,
        Ok(Some(data)) => Seen::Hash(sha256_hex(&data.bytes)),
        Err(_) => Seen::Bad,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Absent,
    Original,
    Asset,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backup {
    Absent,
    Original,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Staging {
    Absent,
    Present,
    Bad,
}

/// The rows of the decision table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Row {
    /// Target `original_hash`, no backup.
    NothingMoved,
    /// Target absent, backup `original_hash`.
    BackedUp,
    /// Target `asset_hash`, backup `original_hash`.
    Published,
    /// Target `asset_hash`, no backup, and `original_hash` is `none`.
    PublishedNoBackup,
    /// Target `asset_hash`, no backup, and `original_hash` is not `none`.
    BackupGone,
    /// Target absent, no backup, and `original_hash` is `none`.
    NothingPublished,
    /// Target absent, no backup, and `original_hash` is not `none`.
    Lost,
    /// The target has another hash.
    Conflict,
    /// The backup has another hash.
    BackupChanged,
    /// The staging name is not a regular file of the user.
    StagingBad,
    /// Target `original_hash` and backup `original_hash`.
    Restored,
}

/// What the files say about one file of a journal.
pub(super) struct Observed {
    target: Target,
    backup: Backup,
    staging: Staging,
}

fn observe(home: &Home, file: &JournalFile) -> Observed {
    let original = file.original_hash.as_deref();
    let target = match seen(home, &file.target_rel()) {
        Seen::Absent => Target::Absent,
        Seen::Bad => Target::Other,
        Seen::Hash(h) if Some(h.as_str()) == original => Target::Original,
        Seen::Hash(h) if h == file.asset_hash => Target::Asset,
        Seen::Hash(_) => Target::Other,
    };
    let backup = match seen(home, &file.backup_rel()) {
        Seen::Absent => Backup::Absent,
        Seen::Hash(h) if Some(h.as_str()) == original => Backup::Original,
        Seen::Hash(_) | Seen::Bad => Backup::Other,
    };
    let staging = match home.lstat(&file.staging_rel()) {
        Ok(None) => Staging::Absent,
        Ok(Some(meta)) if meta.kind == Kind::Regular && meta.is_mine() => Staging::Present,
        Ok(Some(_)) | Err(_) => Staging::Bad,
    };
    Observed {
        target,
        backup,
        staging,
    }
}

fn row(file: &JournalFile, o: &Observed) -> Row {
    if o.target == Target::Other {
        return Row::Conflict;
    }
    if o.backup == Backup::Other {
        return Row::BackupChanged;
    }
    if o.staging == Staging::Bad {
        return Row::StagingBad;
    }
    let none = file.original_hash.is_none();
    match (o.target, o.backup) {
        (Target::Original, Backup::Absent) => Row::NothingMoved,
        (Target::Original, _) => Row::Restored,
        (Target::Absent, Backup::Original) => Row::BackedUp,
        (Target::Asset, Backup::Original) => Row::Published,
        (Target::Asset, _) if none => Row::PublishedNoBackup,
        (Target::Asset, _) => Row::BackupGone,
        (_, _) if none => Row::NothingPublished,
        (_, _) => Row::Lost,
    }
}

fn state_of(row: Row) -> ObservedState {
    match row {
        Row::NothingMoved | Row::NothingPublished => ObservedState::NothingMoved,
        Row::BackedUp => ObservedState::BackedUp,
        Row::Published | Row::PublishedNoBackup => ObservedState::Published,
        Row::Restored => ObservedState::Restored,
        Row::Conflict => ObservedState::Conflict,
        Row::BackupGone | Row::Lost | Row::BackupChanged | Row::StagingBad => {
            ObservedState::Incomplete
        }
    }
}

/// The "Resume" column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resume {
    /// The steps still to do (back up, stage, publish).
    Steps {
        backup: bool,
    },
    /// Remove the staging file; the file is done.
    Done,
    /// Skip: the file was restored.
    Skip,
    Stop(&'static str),
}

fn resume_of(row: Row) -> Resume {
    match row {
        Row::NothingMoved => Resume::Steps { backup: true },
        Row::BackedUp | Row::NothingPublished => Resume::Steps { backup: false },
        Row::Published | Row::PublishedNoBackup => Resume::Done,
        Row::Restored => Resume::Skip,
        Row::Conflict => Resume::Stop(code::RESTORE_CONFLICT),
        Row::BackupGone | Row::Lost | Row::BackupChanged | Row::StagingBad => {
            Resume::Stop(code::REPAIR_INCOMPLETE)
        }
    }
}

/// The "Restore" column.
fn restore_of(row: Row) -> RestoreAction {
    match row {
        Row::NothingMoved | Row::NothingPublished | Row::Restored => RestoreAction::Nothing,
        Row::BackedUp => RestoreAction::CopyBackup,
        Row::Published => RestoreAction::MoveAsideThenCopyBackup,
        Row::PublishedNoBackup => RestoreAction::RemovePublished,
        Row::Conflict => RestoreAction::StopRestoreConflict,
        Row::BackupGone | Row::Lost | Row::BackupChanged | Row::StagingBad => {
            RestoreAction::StopRepairIncomplete
        }
    }
}

/// One file to replace.
pub struct Replacement {
    pub asset: &'static Asset,
    pub id: fleetdeck_proto::omarchy_repair::OmarchyAssetId,
    /// The approved hash of the target content; `None` for a target that
    /// did not exist.
    pub original_hash: Option<String>,
}

/// Makes the journal of a repair (write-ahead, before any file operation).
///
/// # Errors
/// `backup_failed` when the journal cannot be made (nothing has moved).
pub fn start(home: &Home, files: &[Replacement], build_hash: &str) -> Res<Journal> {
    let stop = Halt::Stop(code::BACKUP_FAILED);
    let id = journal::new_id().map_err(|_| Halt::Stop(code::BACKUP_FAILED))?;
    let secs = now_ms() / 1000;
    let mut entries = Vec::new();
    for f in files {
        entries.push(
            journal::new_file(f.asset, f.id, f.original_hash.clone(), secs)
                .map_err(|_| Halt::Stop(code::BACKUP_FAILED))?,
        );
    }
    let journal = journal::new_journal(id, build_hash.to_owned(), now_ms(), entries);
    journal::make_dir(home).map_err(|_| Halt::Stop(code::BACKUP_FAILED))?;
    // The first write makes the file (a name that exists is an error).
    let mut text =
        serde_json::to_vec_pretty(&journal).map_err(|_| Halt::Stop(code::BACKUP_FAILED))?;
    text.push(b'\n');
    match home.create_new(&journal::path_of(&journal.repair_id), &text, 0o600) {
        Ok(true) => Ok(journal),
        Ok(false) | Err(_) => Err(stop),
    }
}

/// The staging file that this run wrote and still holds open: the file that
/// the publication must find again under its name.
struct Staged {
    index: usize,
    fd: OwnedFd,
    /// The hash of the bytes that were written.
    hash: String,
}

/// A repair that runs on a journal.
pub struct Run<'a> {
    home: &'a Home,
    pub journal: Journal,
    /// True once a file of this repair moved.
    moved: bool,
    staged: Option<Staged>,
}

impl<'a> Run<'a> {
    /// `moved` starts from the files: a file that is past "nothing moved"
    /// counts.
    pub fn new(home: &'a Home, journal: Journal) -> Self {
        let moved = journal.files.iter().any(|f| {
            !matches!(
                row(f, &observe(home, f)),
                Row::NothingMoved | Row::NothingPublished
            )
        });
        Self {
            home,
            journal,
            moved,
            staged: None,
        }
    }

    /// The code of an unexpected failure: before any file moved the copy
    /// was not kept, after it the repair is incomplete.
    fn io_stop(&self) -> Halt {
        Halt::Stop(if self.moved {
            code::REPAIR_INCOMPLETE
        } else {
            code::BACKUP_FAILED
        })
    }

    fn save(&self) -> Res<()> {
        journal::save(self.home, &self.journal).map_err(|_| self.io_stop())
    }

    /// A target or a directory of its path that a group or other user can
    /// write is unsafe: the step does not begin (`unsafe_path`).
    fn check_path(&self, i: usize) -> Res<()> {
        match self
            .home
            .others_can_write(&self.journal.files[i].target_rel())
        {
            Ok(false) => Ok(()),
            Ok(true) => Err(Halt::Stop(code::UNSAFE_PATH)),
            Err(_) => Err(self.io_stop()),
        }
    }

    fn begin(&mut self, i: usize, step: Step) -> Res<()> {
        self.check_path(i)?;
        self.journal.files[i].intent = Some(step);
        self.save()?;
        seam()
    }

    fn end(&mut self, i: usize, done: Step) -> Res<()> {
        seam()?;
        let file = &mut self.journal.files[i];
        file.intent = None;
        file.completed.push(done);
        self.save()?;
        seam()
    }

    fn remove_staging(&mut self, i: usize, o: &Observed) -> Res<()> {
        if o.staging == Staging::Present {
            let rel = self.journal.files[i].staging_rel();
            self.home.remove(&rel).map_err(|_| self.io_stop())?;
        }
        Ok(())
    }

    fn backup(&mut self, i: usize) -> Res<()> {
        self.begin(i, Step::BackingUp)?;
        let file = self.journal.files[i].clone();
        match self.home.rename_new(&file.target_rel(), &file.backup_rel()) {
            Ok(true) => {}
            Ok(false) => return Err(Halt::Stop(code::BACKUP_FAILED)),
            Err(_) => {
                // The rename may have happened before its directory sync failed.
                if !matches!(self.home.lstat(&file.backup_rel()), Ok(Some(_))) {
                    return Err(Halt::Stop(code::BACKUP_FAILED));
                }
            }
        }
        self.moved = true;
        seam()?;
        match seen(self.home, &file.backup_rel()) {
            Seen::Hash(h) if Some(h.as_str()) == file.original_hash.as_deref() => {}
            _ => return Err(Halt::Stop(code::REPAIR_INCOMPLETE)),
        }
        self.end(i, Step::BackedUp)
    }

    fn stage(&mut self, i: usize, bytes: &[u8], mode: u32, step: (Step, Step)) -> Res<()> {
        self.begin(i, step.0)?;
        let rel = self.journal.files[i].staging_rel();
        match self.home.create_new_held(&rel, bytes, mode) {
            Ok(Some(fd)) => {
                self.staged = Some(Staged {
                    index: i,
                    fd,
                    hash: sha256_hex(bytes),
                });
            }
            Ok(None) | Err(_) => return Err(Halt::Stop(code::REPAIR_INCOMPLETE)),
        }
        self.end(i, step.1)
    }

    /// The staging file of `i` is the file that this run wrote (same device
    /// and inode), still has one name, is yours, has no group or other
    /// write bit, and holds the bytes that were written; else the run
    /// stops (`repair_incomplete`) and nothing is published.
    fn check_staging(&mut self, i: usize) -> Res<()> {
        let incomplete = Halt::Stop(code::REPAIR_INCOMPLETE);
        let Some(staged) = self.staged.take().filter(|s| s.index == i) else {
            return Err(incomplete);
        };
        match self
            .home
            .read_same(&self.journal.files[i].staging_rel(), &staged.fd)
        {
            Ok(Some(data)) if sha256_hex(&data.bytes) == staged.hash => Ok(()),
            Ok(_) | Err(_) => Err(incomplete),
        }
    }

    fn publish(&mut self, i: usize, step: (Step, Step)) -> Res<()> {
        self.begin(i, step.0)?;
        self.check_staging(i)?;
        let file = self.journal.files[i].clone();
        match self
            .home
            .rename_new(&file.staging_rel(), &file.target_rel())
        {
            Ok(true) => {}
            Ok(false) => return Err(Halt::Stop(code::RESTORE_CONFLICT)),
            Err(_) => return Err(Halt::Stop(code::REPAIR_INCOMPLETE)),
        }
        self.end(i, step.1)
    }

    /// The "Resume" column for one file.
    fn advance(&mut self, i: usize) -> Res<()> {
        self.reconcile_aside(i)?;
        let file = self.journal.files[i].clone();
        let o = observe(self.home, &file);
        match resume_of(row(&file, &o)) {
            Resume::Stop(c) => Err(Halt::Stop(c)),
            Resume::Skip => Ok(()),
            Resume::Done => self.remove_staging(i, &o),
            Resume::Steps { backup } => {
                self.remove_staging(i, &o)?;
                if backup {
                    self.backup(i)?;
                }
                let Some(asset) = file.asset() else {
                    return Err(Halt::Stop(code::REPAIR_INCOMPLETE));
                };
                self.stage(
                    i,
                    asset.text.as_bytes(),
                    asset.mode,
                    (Step::Staging, Step::Staged),
                )?;
                self.publish(i, (Step::Publishing, Step::Published))
            }
        }
    }

    /// Applies the "Resume" column to each file. With `stop_first` the run
    /// ends at the first stopped file; else a stopped file stops only itself.
    /// The code of the first stopped file, in journal order.
    ///
    /// # Errors
    /// Only a simulated crash of a test.
    pub fn advance_all(&mut self, stop_first: bool) -> Res<Option<&'static str>> {
        let mut first = None;
        for i in 0..self.journal.files.len() {
            match self.advance(i) {
                Ok(()) => {}
                Err(Halt::Stop(c)) => {
                    first.get_or_insert(c);
                    if stop_first {
                        break;
                    }
                }
                Err(Halt::Injected) => return Err(Halt::Injected),
            }
        }
        Ok(first)
    }

    fn copy_backup(&mut self, i: usize) -> Res<()> {
        let file = self.journal.files[i].clone();
        // A displaced file under the kept name that is not the published
        // asset is an edit of the owner: it is never covered by a copy.
        if let Some(kept) = file.kept_rel() {
            match seen(self.home, &kept) {
                Seen::Absent => {}
                Seen::Hash(h) if h == file.asset_hash => {}
                Seen::Hash(_) | Seen::Bad => return Err(Halt::Stop(code::RESTORE_CONFLICT)),
            }
        }
        let data = match self.home.read(&file.backup_rel()) {
            Ok(Some(data)) => data,
            _ => return Err(Halt::Stop(code::REPAIR_INCOMPLETE)),
        };
        if Some(sha256_hex(&data.bytes)) != file.original_hash {
            return Err(Halt::Stop(code::REPAIR_INCOMPLETE));
        }
        self.stage(i, &data.bytes, data.mode, (Step::Restoring, Step::Staged))?;
        // The staging record above ends a step; the publish below is the
        // last step of the file.
        self.publish(i, (Step::Restoring, Step::Restored))
    }

    fn clear_intent(&mut self, i: usize) -> Res<()> {
        self.journal.files[i].intent = None;
        self.save()
    }

    /// Moves the published target to a new kept name. The name is in the
    /// journal before the move. A file under the earlier name of the journal
    /// that is not the asset is an edit of the owner: it stays, and the new
    /// name would hide it, so the file stops with `restore_conflict`.
    fn move_aside(&mut self, i: usize) -> Res<()> {
        let file = &self.journal.files[i];
        if let Some(earlier) = file.kept_rel() {
            match seen(self.home, &earlier) {
                Seen::Absent => {}
                Seen::Hash(h) if h == file.asset_hash => {}
                Seen::Hash(_) | Seen::Bad => return Err(Halt::Stop(code::RESTORE_CONFLICT)),
            }
        }
        let kept = journal::new_kept_name(&file.target, now_ms() / 1000)
            .map_err(|_| Halt::Stop(code::REPAIR_INCOMPLETE))?;
        self.journal.files[i].kept_name = Some(kept);
        self.begin(i, Step::MovingAside)?;
        let file = self.journal.files[i].clone();
        let Some(to) = file.kept_rel() else {
            return Err(Halt::Stop(code::REPAIR_INCOMPLETE));
        };
        match self.home.rename_new(&file.target_rel(), &to) {
            Ok(true) => {}
            Ok(false) => return Err(Halt::Stop(code::REPAIR_INCOMPLETE)),
            Err(_) => {
                // The rename may have happened before its directory sync failed.
                if !matches!(self.home.lstat(&to), Ok(Some(_))) {
                    return Err(Halt::Stop(code::REPAIR_INCOMPLETE));
                }
            }
        }
        seam()?;
        self.settle_aside(i)
    }

    /// The displaced file must be the asset that the plan observed. An edit
    /// that came in between is put back at the target when the target is
    /// still absent (a file that cannot be hashed counts as an edit); a
    /// refused or failed rename leaves it under the kept name, which the
    /// result reports. The file stops with `restore_conflict`.
    fn settle_aside(&mut self, i: usize) -> Res<()> {
        let file = self.journal.files[i].clone();
        let Some(to) = file.kept_rel() else {
            return Err(Halt::Stop(code::REPAIR_INCOMPLETE));
        };
        if matches!(seen(self.home, &to), Seen::Hash(h) if h == file.asset_hash) {
            return self.clear_intent(i);
        }
        let _ = self.home.rename_new(&to, &file.target_rel());
        self.clear_intent(i)?;
        Err(Halt::Stop(code::RESTORE_CONFLICT))
    }

    /// Settles what an interrupted move aside left, before a resume or a
    /// restore chooses its action for file `i`; then refuses to go on while a
    /// displaced edit of the owner sits under the kept name.
    fn reconcile_aside(&mut self, i: usize) -> Res<()> {
        if self.journal.files[i].intent == Some(Step::MovingAside) {
            let kept = self.journal.files[i].kept_rel();
            match kept.as_ref().map(|k| seen(self.home, k)) {
                // The move did not happen.
                None | Some(Seen::Absent) => self.clear_intent(i)?,
                // The asset moved, or an edit moved: settle_aside decides.
                Some(_) => self.settle_aside(i)?,
            }
        }
        let file = &self.journal.files[i];
        if let Some(kept) = file.kept_rel() {
            match seen(self.home, &kept) {
                Seen::Absent => {}
                Seen::Hash(h) if h == file.asset_hash => {}
                Seen::Hash(_) | Seen::Bad => return Err(Halt::Stop(code::RESTORE_CONFLICT)),
            }
        }
        Ok(())
    }

    /// The "Restore" column for one file. `Ok(true)` when it changed the
    /// file.
    fn restore_one(&mut self, i: usize) -> Res<bool> {
        self.reconcile_aside(i)?;
        let file = self.journal.files[i].clone();
        let o = observe(self.home, &file);
        let action = restore_of(row(&file, &o));
        match action {
            RestoreAction::StopRestoreConflict => Err(Halt::Stop(code::RESTORE_CONFLICT)),
            RestoreAction::StopRepairIncomplete => Err(Halt::Stop(code::REPAIR_INCOMPLETE)),
            RestoreAction::Nothing => {
                self.remove_staging(i, &o)?;
                Ok(false)
            }
            RestoreAction::CopyBackup => {
                self.remove_staging(i, &o)?;
                self.copy_backup(i)?;
                Ok(true)
            }
            RestoreAction::MoveAsideThenCopyBackup => {
                self.remove_staging(i, &o)?;
                self.move_aside(i)?;
                seam()?;
                self.copy_backup(i)?;
                Ok(true)
            }
            RestoreAction::RemovePublished => {
                self.remove_staging(i, &o)?;
                self.begin(i, Step::Restoring)?;
                self.home
                    .remove(&file.target_rel())
                    .map_err(|_| Halt::Stop(code::REPAIR_INCOMPLETE))?;
                self.end(i, Step::Restored)?;
                Ok(true)
            }
        }
    }

    /// Applies the "Restore" column to the files at `selected` (indexes of
    /// the journal). A stopped file stops only itself. Returns the code of
    /// the first stopped file and the indexes that this run changed.
    ///
    /// # Errors
    /// Only a simulated crash of a test.
    pub fn restore_all(&mut self, selected: &[usize]) -> Res<(Option<&'static str>, Vec<usize>)> {
        self.moved = true;
        let mut first = None;
        let mut changed = Vec::new();
        for &i in selected {
            match self.restore_one(i) {
                Ok(true) => changed.push(i),
                Ok(false) => {}
                Err(Halt::Stop(c)) => {
                    first.get_or_insert(c);
                }
                Err(Halt::Injected) => return Err(Halt::Injected),
            }
        }
        Ok((first, changed))
    }
}

/// The absolute path of a file that a restore displaced and that is not the
/// published asset (a later edit of the owner); `None` when there is none.
fn displaced_edit(home: &Home, file: &JournalFile) -> Option<String> {
    let rel = file.kept_rel()?;
    match seen(home, &rel) {
        Seen::Absent => None,
        Seen::Hash(h) if h == file.asset_hash => None,
        Seen::Hash(_) | Seen::Bad => Some(home.abs(&rel).display().to_string()),
    }
}

/// The state of each file now, for the result of an install or a resume.
#[must_use]
pub fn result_files(home: &Home, journal: &Journal, only: Option<&[usize]>) -> Vec<RepairFile> {
    journal
        .files
        .iter()
        .enumerate()
        .filter(|(i, _)| only.is_none_or(|sel| sel.contains(i)))
        .map(|(_, f)| {
            let o = observe(home, f);
            let state = match state_of(row(f, &o)) {
                ObservedState::BackedUp => RepairFileState::BackedUp,
                ObservedState::Published => RepairFileState::Replaced,
                ObservedState::Restored => RepairFileState::Restored,
                _ => RepairFileState::Unchanged,
            };
            RepairFile {
                asset_id: f.asset_id,
                path: home.abs(&f.target_rel()).display().to_string(),
                backup_path: (o.backup != Backup::Absent)
                    .then(|| home.abs(&f.backup_rel()).display().to_string()),
                state,
                kept_path: displaced_edit(home, f),
            }
        })
        .collect()
}

/// The files of `journal` with no change and no kept copy (the result of a
/// refusal before any file operation).
#[must_use]
pub fn unchanged_files(home: &Home, journal: &Journal, only: Option<&[usize]>) -> Vec<RepairFile> {
    journal
        .files
        .iter()
        .enumerate()
        .filter(|(i, _)| only.is_none_or(|sel| sel.contains(i)))
        .map(|(_, f)| RepairFile {
            asset_id: f.asset_id,
            path: home.abs(&f.target_rel()).display().to_string(),
            backup_path: None,
            state: RepairFileState::Unchanged,
            kept_path: None,
        })
        .collect()
}

/// True when every file of the journal is `published`.
#[must_use]
pub fn all_published(home: &Home, journal: &Journal) -> bool {
    journal
        .files
        .iter()
        .all(|f| state_of(row(f, &observe(home, f))) == ObservedState::Published)
}

/// The view of one journal for `repair-status`.
#[must_use]
pub fn view(home: &Home, journal: &Journal) -> RepairJournalView {
    let observed: Vec<(ObservedState, Option<String>)> = journal
        .files
        .iter()
        .map(|f| {
            let o = observe(home, f);
            let backup = (o.backup != Backup::Absent)
                .then(|| home.abs(&f.backup_rel()).display().to_string());
            (state_of(row(f, &o)), backup)
        })
        .collect();
    let published = observed.iter().all(|(s, _)| *s == ObservedState::Published);
    let state = if published && journal.finished {
        JournalState::Complete
    } else if observed.iter().all(|(s, _)| *s == ObservedState::Restored) {
        JournalState::Restored
    } else {
        JournalState::Incomplete
    };
    RepairJournalView {
        repair_id: journal.repair_id.clone(),
        created_at: journal.created_at,
        state,
        files: journal
            .files
            .iter()
            .zip(observed)
            .map(|(f, (state, backup_path))| RepairStatusFile {
                asset_id: f.asset_id,
                state,
                backup_path,
            })
            .collect(),
    }
}

/// The facts of a restore plan for `journal`.
#[must_use]
pub fn restore_facts(home: &Home, journal: &Journal) -> RestorePlanFacts {
    let hash = |rel: &std::path::Path| match seen(home, rel) {
        Seen::Hash(h) => Some(h),
        Seen::Absent | Seen::Bad => None,
    };
    RestorePlanFacts {
        repair_id: journal.repair_id.clone(),
        files: journal
            .files
            .iter()
            .map(|f| RestorePlanFile {
                asset_id: f.asset_id,
                path: home.abs(&f.target_rel()).display().to_string(),
                target_sha256: hash(&f.target_rel()),
                backup_path: home.abs(&f.backup_rel()).display().to_string(),
                backup_sha256: hash(&f.backup_rel()),
                action: restore_of(row(f, &observe(home, f))),
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "repair_r1_tests.rs"]
mod r1_tests;

#[cfg(test)]
#[path = "repair_r2_tests.rs"]
mod r2_tests;
