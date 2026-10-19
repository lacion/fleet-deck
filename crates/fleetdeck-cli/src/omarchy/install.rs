//! `fleetdeck omarchy install [--yes] [--screensaver]` (R13 P4 task C,
//! `omarchy-p4.md` §3.4 steps 1 to 7), and the repair modes of the same
//! command (`board-gaps.md` §3.4): `--replace-conflicts --plan-stdin` and
//! `--resume`, which share the part after the replacement (`finish`).

use std::io::{BufRead as _, Write as _};
use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, bail};
use fleetdeck_core::ChildEnv;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_station::desktop::detect::omarchy_root;
use fleetdeck_station::desktop::run::on_path;

use super::assets::{Asset, Base, FILES, HYPR_PATH, MENU_PATH, menu_rows};
use super::fsx::Home;
use super::lock::{self, LockGuard, Take};
use super::record::{self, FileEntry, Record, rel, sha256_hex};
use super::{TOOL_LIMIT, hyprland, omarchy_env, open_home, set_summary, tool};
use fleetdeck_proto::omarchy_admin::LOCK_BUSY_EXIT;
use fleetdeck_proto::omarchy_repair::InstallPlanInput;

const RESCAN: &str = "omarchy-shell shell rescanPlugins";
const ENABLE: &str = "omarchy plugin enable fleetdeck.fleet --section right";
const SUMMARY_ON: &str = "fleetdeck settings set --station fleet_summary true";

/// What the install does with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Plan {
    /// No file exists: write it.
    Create,
    /// A file exists with the bytes of this build's asset: adopt it. The
    /// record gets the right hash (a file that the record does not name is
    /// also recorded).
    Adopt,
    /// A recorded file that is as recorded: write the new content.
    Replace,
    /// A recorded file that changed: keep it.
    Keep,
    /// The menu file of the user exists: do not write it.
    SkipMenu,
    /// A file exists that is not recorded and differs.
    Conflict,
}

impl Plan {
    fn label(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Adopt => "adopt",
            Self::Replace => "replace",
            Self::Keep => "kept (changed)",
            Self::SkipMenu => "skip (exists)",
            Self::Conflict => "conflict",
        }
    }
}

pub(super) struct Item {
    pub(super) asset: &'static Asset,
    pub(super) rel: PathBuf,
    pub(super) abs: PathBuf,
    pub(super) plan: Plan,
}

/// Where the human lines of a run go: stdout for the plain install, stderr
/// for the repair modes (their stdout holds only the JSON result).
#[derive(Debug, Clone, Copy)]
pub struct Say {
    stderr: bool,
}

impl Say {
    /// The human lines go to stderr.
    #[must_use]
    pub const fn stderr() -> Self {
        Self { stderr: true }
    }

    fn line(self, text: impl std::fmt::Display) {
        if self.stderr {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    }
}

/// How `install` runs.
#[derive(Debug)]
pub enum Mode {
    /// The plain install: it asks (or `--yes`), and a conflict refuses.
    Plain { yes: bool },
    /// `--replace-conflicts --plan-stdin`: the reviewed plan.
    Replace(InstallPlanInput),
    /// `--resume <repair id>`.
    Resume(String),
}

/// What a file of the install is, from its bytes and its record entry.
pub(super) fn decide(existing: Option<&[u8]>, entry: Option<&FileEntry>, asset: &Asset) -> Plan {
    match (existing, entry) {
        (None, _) => Plan::Create,
        (Some(_), None) if asset.only_if_absent => Plan::SkipMenu,
        (Some(data), None) if data == asset.text.as_bytes() => Plan::Adopt,
        (Some(_), None) => Plan::Conflict,
        (Some(data), Some(_)) if data == asset.text.as_bytes() => Plan::Adopt,
        (Some(data), Some(entry)) if sha256_hex(data) == entry.sha256 => Plan::Replace,
        (Some(_), Some(_)) => Plan::Keep,
    }
}

/// Step 3, the read-only part: decides what happens to each file. A link
/// or a foreign owner on a path is an error here, before any write.
pub(super) fn classify(home: &Home, record: &Record) -> anyhow::Result<Vec<Item>> {
    let mut items = Vec::new();
    for asset in FILES {
        let rel = rel(asset.base, asset.path);
        let existing = home.read(&rel)?;
        let entry = record.entry(asset.base, asset.path);
        let plan = decide(existing.as_ref().map(|d| d.bytes.as_slice()), entry, asset);
        items.push(Item {
            asset,
            abs: home.abs(&rel),
            rel,
            plan,
        });
    }
    Ok(items)
}

fn warn(text: &str) {
    eprintln!("fleetdeck: warning: {text}");
}

/// Step 2: the question on a terminal, or `--yes`.
fn confirm(yes: bool) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    let stdin = std::io::stdin();
    if !rustix::termios::isatty(stdin.as_fd()) {
        bail!("Run with --yes to confirm.");
    }
    print!("Continue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        bail!("Cancelled; nothing was changed.")
    }
}

/// Step 3, the write part. `after_prompt` runs first (the test seam of the
/// time after the question). Every target is checked again just before its
/// publish: a new file is created without replacing a name (a file that
/// appeared meanwhile is a conflict and stops the run); a recorded file is
/// replaced only when it still has the recorded hash, else it is kept.
/// `Ok(false)` when a conflict stopped the run.
#[cfg(test)]
fn publish(
    home: &Home,
    record: &mut Record,
    items: &[Item],
    after_prompt: impl FnOnce(),
) -> anyhow::Result<bool> {
    publish_say(home, record, items, Say { stderr: false }, after_prompt)
}

fn publish_say(
    home: &Home,
    record: &mut Record,
    items: &[Item],
    say: Say,
    after_prompt: impl FnOnce(),
) -> anyhow::Result<bool> {
    after_prompt();
    for item in items {
        let asset = item.asset;
        match item.plan {
            Plan::Create => {
                if !home.create_new(&item.rel, asset.text.as_bytes(), asset.mode)? {
                    eprintln!("fleetdeck: conflict: {}", item.abs.display());
                    return Ok(false);
                }
                upsert_written(record, asset);
            }
            Plan::Replace => {
                let recorded = record.entry(asset.base, asset.path).map(|e| &e.sha256);
                let unchanged = match (home.read(&item.rel)?, recorded) {
                    (Some(data), Some(hash)) => &sha256_hex(&data.bytes) == hash,
                    _ => false,
                };
                if unchanged {
                    home.write(&item.rel, asset.text.as_bytes(), asset.mode)?;
                    upsert_written(record, asset);
                } else {
                    say.line(format_args!("kept (changed): {}", item.abs.display()));
                }
            }
            Plan::Adopt => {
                // A recorded file keeps its `existed_before`; only its hash
                // is repaired. An unrecorded file counts as existing before.
                let existed_before = record
                    .entry(asset.base, asset.path)
                    .is_none_or(|e| e.existed_before);
                record.upsert(FileEntry {
                    path: asset.path.to_owned(),
                    base: asset.base,
                    sha256: sha256_hex(asset.text.as_bytes()),
                    existed_before,
                });
            }
            Plan::Keep => say.line(format_args!("kept (changed): {}", item.abs.display())),
            Plan::SkipMenu | Plan::Conflict => {}
        }
        record.menu_file_written = record.entry(Base::Omarchy, MENU_PATH).is_some();
        record::save(home, record)?;
    }
    Ok(true)
}

/// Records a file that this install wrote.
fn upsert_written(record: &mut Record, asset: &Asset) {
    let existed_before = record
        .entry(asset.base, asset.path)
        .is_some_and(|e| e.existed_before);
    record.upsert(FileEntry {
        path: asset.path.to_owned(),
        base: asset.base,
        sha256: sha256_hex(asset.text.as_bytes()),
        existed_before,
    });
}

/// Step 1: Omarchy, found, and the config home, opened.
pub(super) struct Found {
    pub(super) env: ChildEnv,
    pub(super) root: PathBuf,
    pub(super) version: Option<String>,
    pub(super) home: Home,
}

/// Detects Omarchy and opens the config home.
///
/// # Errors
/// No Omarchy, or the config home cannot be opened.
pub(super) fn find() -> anyhow::Result<Found> {
    let env = omarchy_env();
    let Some((root, version)) = omarchy_root(&env) else {
        bail!("Omarchy was not found on this machine.");
    };
    let home = open_home()?;
    Ok(Found {
        env,
        root,
        version,
        home,
    })
}

/// Takes the Omarchy lock with the normal wait (the folder and the file are
/// made when they are missing). `None` when another change holds it: the
/// busy line is printed to stderr and the caller exits with
/// [`BUSY_EXIT`].
///
/// # Errors
/// A link or a foreign owner on the lock path, or an I/O error.
pub(super) fn take_lock(home: &Home) -> anyhow::Result<Option<LockGuard>> {
    match lock::take(home, lock::WAIT, true)? {
        Take::Held(guard) => Ok(Some(guard)),
        Take::Busy | Take::NoLockFile => {
            eprintln!("{}", lock::BUSY);
            Ok(None)
        }
    }
}

/// The exit code of a busy lock.
pub(super) fn busy_exit() -> ExitCode {
    ExitCode::from(LOCK_BUSY_EXIT)
}

/// An install that holds the lock.
pub(super) struct Session {
    pub(super) found: Found,
    pub(super) record: Record,
    line: String,
    _guard: LockGuard,
}

/// Steps 1 and 3 (the start) for the writing modes: detect, compute the
/// Hyprland line, take the lock, load the record (a version 1 record is
/// converted in memory). `Ok(None)` when the lock is busy.
///
/// # Errors
/// No Omarchy, a path that Lua cannot take, an unreadable record.
pub(super) fn begin() -> anyhow::Result<Option<Session>> {
    let found = find()?;
    let hypr_abs = found.home.abs(&rel(Base::Fleetdeck, HYPR_PATH));
    let line = hyprland::line_for(&hypr_abs).ok_or_else(|| {
        anyhow!("the config path holds characters that Lua cannot take; change nothing.")
    })?;
    let Some(guard) = take_lock(&found.home)? else {
        return Ok(None);
    };
    let mut record = record::load(&found.home)?.unwrap_or_default();
    if record.v == 1 {
        record.v = 2;
        record.converted_at = Some(record::now_ms());
    }
    Ok(Some(Session {
        found,
        record,
        line,
        _guard: guard,
    }))
}

/// Prints the conflicts of `items` to stderr. `true` when there is one.
pub(super) fn refuse_conflicts(items: &[Item]) -> bool {
    let conflicts: Vec<&Item> = items.iter().filter(|i| i.plan == Plan::Conflict).collect();
    for item in &conflicts {
        eprintln!("fleetdeck: conflict: {}", item.abs.display());
    }
    !conflicts.is_empty()
}

/// Steps 3 (the write), 4, 5, 6 and 7: the part of the install that follows
/// the question or the replacement. Every human line goes to `say`.
pub(super) async fn finish(
    dirs: &NodeDirs,
    session: &mut Session,
    items: &[Item],
    say: Say,
    screensaver: bool,
) -> anyhow::Result<ExitCode> {
    let Session {
        found,
        record,
        line,
        ..
    } = session;
    let (home, env) = (&found.home, &found.env);

    // 3. Write each file; the record follows each one.
    record.fleetdeck_version = fleetdeck_core::build::version().to_owned();
    if !publish_say(home, record, items, say, || {})? {
        return Ok(ExitCode::from(1));
    }

    // 4. Rescan the plugins; enable the plugin once.
    let rescanned = matches!(
        tool(env, "omarchy-shell", &["shell", "rescanPlugins"], TOOL_LIMIT).await,
        Ok(out) if out.code == 0 && out.stdout.trim() == "ok"
    );
    if !rescanned {
        warn(&format!("the plugins were not rescanned; run: {RESCAN}"));
    }
    if !record.plugin_enabled {
        let enabled = matches!(
            tool(
                env,
                "omarchy",
                &["plugin", "enable", "fleetdeck.fleet", "--section", "right"],
                TOOL_LIMIT
            )
            .await,
            Ok(out) if out.code == 0
        );
        if enabled {
            record.plugin_enabled = true;
            record::save(home, record)?;
        } else {
            warn(&format!("the plugin is not enabled; run: {ENABLE}"));
        }
    }

    // 5. Turn on fleet_summary.
    match set_summary(dirs, true).await {
        Ok(changed) => {
            if changed {
                record.fleet_summary_set_by_install = true;
                record::save(home, record)?;
            }
        }
        Err(_) => warn(&format!(
            "the station is not running or did not accept the change, so fleet_summary stays off; run: {SUMMARY_ON}"
        )),
    }

    // 6. The screensaver switch (the station runs the ticks).
    if screensaver {
        record.screensaver.enabled = true;
    }

    // 7. The managed Hyprland line, and what the user adds by hand.
    let done = hyprland::install(home, line, &record.hyprland_line);
    record.hyprland_line = done.record;
    record::save(home, record)?;
    say.line(&done.message);
    if done.print_line {
        say.line(format_args!(
            "Add this line at the end of {}:",
            home.abs(Path::new(hyprland::FILE)).display()
        ));
        say.line(&*line);
    }
    if !record.menu_file_written {
        say.line(format_args!(
            "Add these rows inside the object of {}:",
            home.abs(&rel(Base::Omarchy, MENU_PATH)).display()
        ));
        for row in menu_rows() {
            say.line(row);
        }
    }
    if !on_path(env, "fleetdeck") {
        warn("fleetdeck is not on PATH; the hotkeys and the hooks will not find it");
    }
    say.line("Fleet Deck is installed for Omarchy.");
    Ok(ExitCode::SUCCESS)
}

/// `fleetdeck omarchy install`.
///
/// # Errors
/// No Omarchy, a path that is a link or not yours, an unreadable record, a
/// failed write, or no confirmation.
pub async fn run(dirs: &NodeDirs, mode: Mode, screensaver: bool) -> anyhow::Result<ExitCode> {
    let yes = match mode {
        Mode::Plain { yes } => yes,
        Mode::Replace(input) => return super::repair_cmd::replace(dirs, input).await,
        Mode::Resume(id) => return super::repair_cmd::resume(dirs, &id).await,
    };
    // 1. Detect Omarchy. Compute the file set.
    let Some(mut session) = begin()? else {
        return Ok(busy_exit());
    };
    let items = classify(&session.found.home, &session.record)?;

    // 3 (first part). A conflict refuses before the question and before any write.
    if refuse_conflicts(&items) {
        return Ok(ExitCode::from(1));
    }

    // 2. Print the list. Ask.
    let say = Say { stderr: false };
    let home = &session.found.home;
    say.line(format_args!(
        "Omarchy {} found at {}.",
        session
            .found
            .version
            .as_deref()
            .unwrap_or("(version unknown)"),
        session.found.root.display()
    ));
    say.line("Fleet Deck will write these files:");
    for item in &items {
        say.line(format_args!(
            "  {:04o}  {}  [{}]",
            item.asset.mode,
            item.abs.display(),
            item.plan.label()
        ));
    }
    say.line(format_args!(
        "  0600  {}  [record]",
        home.abs(Path::new(record::RECORD_PATH)).display()
    ));
    say.line("It will also:");
    if !session.record.plugin_enabled {
        say.line(format_args!("  - run: {ENABLE}"));
    }
    say.line("  - turn on the station setting fleet_summary");
    say.line(format_args!(
        "  - add one managed line to {}",
        home.abs(Path::new(hyprland::FILE)).display()
    ));
    if screensaver {
        say.line("  - record that the screensaver is on");
    }
    confirm(yes)?;

    finish(dirs, &mut session, &items, say, screensaver).await
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
