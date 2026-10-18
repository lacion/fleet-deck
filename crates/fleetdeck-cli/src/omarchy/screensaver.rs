//! The ambient screensaver (R13 P4 task G, `omarchy-p4.md` §3.8; the station
//! runs it as short ticks, `omarchy-admin.md` §3.4 to §3.7).
//!
//! The hidden command `fleetdeck omarchy screensaver-tick` renders the fleet
//! summary of the local station into `<omarchy>/branding/screensaver.txt`,
//! once for each call, and writes only when the text changed. The first
//! write copies the original file to `screensaver.txt.fleetdeck-backup` (an
//! exclusive create; an existing backup is never replaced, and the tick
//! writes over a file only when that file is identical to an existing
//! backup). `omarchy screensaver on | off | rebaseline` change the switch
//! and the backup under the Omarchy lock. `omarchy off` and `omarchy
//! uninstall` call [`restore`], which keeps the backup file.
//!
//! The text is ASCII only (any other character is `?`), at most 40 lines of
//! 100 columns, and every line ends with `\n`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_proto::event::RequestKind;
use fleetdeck_proto::fleet_summary::{DeckState, FleetSummary, NeedsYouItem};
use fleetdeck_station::local::socket_path;

use super::assets::Base;
use super::fsx::{FsError, Home};
use super::lock::{self, LockGuard, Take};
use super::record::{self, LoadError, Record, Restored, Screensaver, rel, sha256_hex};
use super::{STATION_LIMIT, digits, omarchy_env, open_home, tool};
use crate::desktop_feed::{Answer, ask, corrected};

/// The screensaver file, relative to `<omarchy>`.
pub const FILE: &str = "branding/screensaver.txt";
/// The backup of the original file, relative to `<omarchy>`.
pub const BACKUP: &str = "branding/screensaver.txt.fleetdeck-backup";
/// The time limit of `omarchy ascii FLEET`.
const ASCII_LIMIT: Duration = Duration::from_secs(2);
/// The most lines of the whole text.
const MAX_LINES: usize = 40;
/// The most lines of the `omarchy ascii` output.
const MAX_BANNER_LINES: usize = 18;
/// The most columns of a line.
const MAX_COLUMNS: usize = 100;
/// The text for no data.
const NO_DATA: &str = "Fleet Deck: no data\n";
/// The banner when `omarchy ascii` is not usable.
const FALLBACK_BANNER: &str = "FLEET DECK";

fn ascii(text: &str) -> String {
    text.chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .collect()
}

/// One line: ASCII only, cut at 100 columns.
fn clean(text: &str) -> String {
    ascii(text).chars().take(MAX_COLUMNS).collect()
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$`, the name rule of the notifications.
fn name_ok(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The callsign, else the first 8 characters of the session id, else
/// `session`.
fn name_of(item: &NeedsYouItem) -> String {
    match &item.callsign {
        Some(callsign) if name_ok(callsign) => callsign.clone(),
        _ => {
            let prefix: String = item.session.chars().take(8).collect();
            if name_ok(&prefix) {
                prefix
            } else {
                "session".to_owned()
            }
        }
    }
}

fn age_text(age_s: u32) -> String {
    if age_s < 60 {
        format!("{age_s} s")
    } else if age_s < 3600 {
        format!("{} min", age_s / 60)
    } else {
        format!("{} h", age_s / 3600)
    }
}

/// The oldest item: the largest `age_s`, the first one on a tie.
fn oldest(items: &[NeedsYouItem]) -> Option<&NeedsYouItem> {
    items.iter().fold(None, |best, item| match best {
        Some(b) if b.age_s >= item.age_s => Some(b),
        _ => Some(item),
    })
}

fn item_line(item: &NeedsYouItem) -> String {
    let kind = match item.kind {
        RequestKind::Command | RequestKind::FileChange | RequestKind::Tool => "Approval",
        RequestKind::Question => "Question",
    };
    format!("{} - {kind} - {}", name_of(item), age_text(item.age_s))
}

fn usage_line(summary: &FleetSummary) -> Option<String> {
    let usage = summary.usage.as_ref()?;
    let label = usage.label.replace('\u{b7}', "-");
    let mut line = format!("{label} - {}%", usage.pct);
    if let Some(s) = usage.resets_in_s {
        line.push_str(&format!(
            " - resets in {} h {} min",
            s / 3600,
            (s % 3600) / 60
        ));
    }
    Some(line)
}

/// The text of the screensaver. `None` is no data. `banner` is the word
/// `FLEET` (its lines, from `omarchy ascii`) or `FLEET DECK`.
#[must_use]
pub fn render(summary: Option<&FleetSummary>, banner: &[String]) -> String {
    let Some(summary) = summary else {
        return NO_DATA.to_owned();
    };
    let mut lines: Vec<String> = banner.iter().take(MAX_BANNER_LINES).cloned().collect();
    lines.push(String::new());
    lines.extend(
        digits::rows(summary.counts.needs_you)
            .into_iter()
            .map(|row| row.trim_end().to_owned()),
    );
    lines.push("need you".to_owned());
    let c = &summary.counts;
    lines.push(format!(
        "working {}   idle {}   blocked {}   lost {}",
        c.working, c.idle, c.blocked, c.lost
    ));
    if let Some(item) = oldest(&summary.needs_you) {
        lines.push(item_line(item));
    }
    lines.extend(usage_line(summary));
    let mut text = String::new();
    for line in lines.iter().take(MAX_LINES) {
        text.push_str(&clean(line));
        text.push('\n');
    }
    text
}

/// The banner: the lines of `omarchy ascii FLEET`, else `FLEET DECK`.
async fn banner(env: &ChildEnv) -> Vec<String> {
    let fallback = || vec![FALLBACK_BANNER.to_owned()];
    match tool(env, "omarchy", &["ascii", "FLEET"], ASCII_LIMIT).await {
        Ok(out) if out.code == 0 && !out.stdout.trim().is_empty() => out
            .stdout
            .lines()
            .take(MAX_BANNER_LINES)
            .map(clean)
            .collect(),
        _ => fallback(),
    }
}

/// The summary of the local station, or `None` (no data): no answer, no
/// summary yet, the setting off, or a deck that is not `up`.
async fn fetch(socket: &Path) -> Option<FleetSummary> {
    match ask(socket, 0, STATION_LIMIT).await {
        Answer::Summary {
            link_up,
            held_s,
            summary: Some(summary),
            ..
        } => {
            let summary = corrected(summary, link_up, held_s);
            (summary.deck == DeckState::Up).then_some(summary)
        }
        _ => None,
    }
}

fn warn(text: &str) {
    eprintln!("fleetdeck: warning: screensaver: {text}");
}

/// The line when an old backup blocks the first write.
const OLD_BACKUP: &str = "fleetdeck: an old screensaver backup exists and differs from the current file; the screensaver stays off.";
/// The line when no record (or no lock file) exists.
const NOT_INSTALLED: &str = "Install the Omarchy integration first.";

/// A backup that exists is never replaced. It is accepted only when it is
/// byte-identical to the current file. Records `had_original` and
/// `backup_sha256` then; `false` when the backup differs.
fn accept_backup(record: &mut Record, current: &[u8], backup: &[u8]) -> bool {
    if current != backup {
        return false;
    }
    record.screensaver.had_original = true;
    record.screensaver.backup_sha256 = Some(sha256_hex(backup));
    true
}

/// The backup before the first write. Records `had_original` and
/// `backup_sha256`, and saves the record. `Ok(false)` when an old backup
/// exists and differs from the current file (or there is no current file):
/// nothing is written or changed then.
fn first_write(home: &Home, record: &mut Record) -> anyhow::Result<bool> {
    let file = rel(Base::Omarchy, FILE);
    let backup = rel(Base::Omarchy, BACKUP);
    let shown = home.abs(&backup).display().to_string();
    let current = home.read(&file)?;
    let old = home.read(&backup)?;
    match (current, old) {
        (None, None) => {
            record.screensaver.had_original = false;
            record.screensaver.backup_sha256 = None;
        }
        (None, Some(_)) => return Ok(false),
        (Some(current), Some(old)) => {
            if !accept_backup(record, &current.bytes, &old.bytes) {
                return Ok(false);
            }
        }
        (Some(current), None) => {
            if home.create_new(&backup, &current.bytes, 0o644)? {
                record.screensaver.backup_sha256 = Some(sha256_hex(&current.bytes));
                record.screensaver.had_original = true;
            } else {
                // A backup appeared meanwhile; it is never replaced.
                let Some(old) = home.read(&backup)? else {
                    anyhow::bail!("the backup {shown} disappeared");
                };
                if !accept_backup(record, &current.bytes, &old.bytes) {
                    return Ok(false);
                }
            }
        }
    }
    record::save(home, record)?;
    Ok(true)
}

/// One tick: write `text` when it differs from the last written text.
/// `Ok(false)` when an old backup blocks the first write. After a first
/// write, `first_write_refused` is false again.
fn write_if_changed(home: &Home, record: &mut Record, text: &str) -> anyhow::Result<bool> {
    let hash = sha256_hex(text.as_bytes());
    if record.screensaver.last_written_sha256.as_deref() == Some(hash.as_str()) {
        return Ok(true);
    }
    if record.screensaver.last_written_sha256.is_none() && !first_write(home, record)? {
        return Ok(false);
    }
    home.write(&rel(Base::Omarchy, FILE), text.as_bytes(), 0o644)?;
    record.screensaver.last_written_sha256 = Some(hash);
    record.screensaver.first_write_refused = false;
    record::save(home, record)?;
    Ok(true)
}

/// The lock and the version 2 record of `on`, `off` and `rebaseline`.
/// `Err(code)` inside: the line is printed and the command ends with `code`.
fn begin(home: &Home, wait: Duration) -> anyhow::Result<Result<(LockGuard, Record), ExitCode>> {
    let guard = match lock::take(home, wait, false)? {
        Take::Held(guard) => guard,
        Take::Busy => {
            eprintln!("{}", lock::BUSY);
            return Ok(Err(ExitCode::from(1)));
        }
        Take::NoLockFile => {
            eprintln!("{NOT_INSTALLED}");
            return Ok(Err(ExitCode::from(1)));
        }
    };
    match record::load_current(home) {
        Ok(Some(record)) => Ok(Ok((guard, record))),
        Ok(None) => {
            eprintln!("{NOT_INSTALLED}");
            Ok(Err(ExitCode::from(1)))
        }
        Err(LoadError::Outdated) => {
            eprintln!("{}", record::OUTDATED);
            Ok(Err(ExitCode::from(1)))
        }
        Err(e) => Err(e.into()),
    }
}

/// `fleetdeck omarchy screensaver on`.
///
/// # Errors
/// The record is not valid or cannot be written.
pub fn on(home: &Home, wait: Duration) -> anyhow::Result<ExitCode> {
    let (_guard, mut record) = match begin(home, wait)? {
        Ok(started) => started,
        Err(code) => return Ok(code),
    };
    record.screensaver.enabled = true;
    record.screensaver.restored = None;
    record::save(home, &record)?;
    Ok(ExitCode::SUCCESS)
}

/// `fleetdeck omarchy screensaver off`. Exit 1 when the restore failed (the
/// record is not changed then).
///
/// # Errors
/// The record is not valid or cannot be written.
pub fn off(home: &Home, wait: Duration) -> anyhow::Result<ExitCode> {
    let (_guard, mut record) = match begin(home, wait)? {
        Ok(started) => started,
        Err(code) => return Ok(code),
    };
    if record.screensaver.enabled {
        if restore(home, &mut record).is_err() {
            return Ok(ExitCode::from(1));
        }
        report_backup(home);
    }
    record.screensaver.enabled = false;
    record::save(home, &record)?;
    Ok(ExitCode::SUCCESS)
}

/// `fleetdeck omarchy screensaver rebaseline`: the kept backup gets a new
/// name (it is never deleted or replaced), so that the next tick takes a new
/// backup of the current file.
///
/// # Errors
/// The record is not valid, or a file cannot be read, renamed or written.
pub fn rebaseline(home: &Home, wait: Duration) -> anyhow::Result<ExitCode> {
    let (_guard, mut record) = match begin(home, wait)? {
        Ok(started) => started,
        Err(code) => return Ok(code),
    };
    if let Some(kept) = rename_backup(home, record::now_ms() / 1000)? {
        println!("The previous original is kept in {}.", kept.display());
    }
    clear_cache(&mut record.screensaver);
    record::save(home, &record)?;
    Ok(ExitCode::SUCCESS)
}

/// Renames the backup file to `<backup>.<secs>`, or `<backup>.<secs>-1`,
/// `-2`, ... when the name exists (a file is never replaced or deleted).
/// Returns the absolute path of the new name; `None` when there is no
/// backup.
fn rename_backup(home: &Home, secs: i64) -> anyhow::Result<Option<PathBuf>> {
    let backup = rel(Base::Omarchy, BACKUP);
    if home.read(&backup)?.is_none() {
        return Ok(None);
    }
    let mut n = 0_u32;
    loop {
        let name = if n == 0 {
            format!("{BACKUP}.{secs}")
        } else {
            format!("{BACKUP}.{secs}-{n}")
        };
        let to = rel(Base::Omarchy, &name);
        if home.rename_new(&backup, &to)? {
            return Ok(Some(home.abs(&to)));
        }
        n += 1;
    }
}

/// `fleetdeck omarchy screensaver-tick`: one cycle. It exits 0 in every case
/// except an unexpected error.
///
/// # Errors
/// The config home cannot be opened, or a file cannot be read or written.
pub async fn tick(dirs: &NodeDirs) -> anyhow::Result<ExitCode> {
    let home = open_home()?;
    let socket: PathBuf = socket_path(&dirs.station());
    let env = omarchy_env();
    // The slow part runs without the lock.
    let summary = fetch(&socket).await;
    let banner = match &summary {
        Some(_) => banner(&env).await,
        None => Vec::new(),
    };
    tick_with(&home, summary.as_ref(), &banner)
}

/// The part of the tick that holds the lock. With no lock (held by another
/// change, or no lock file), no record, a version 1 or invalid record, or the
/// switch off, nothing is written and the exit code is 0.
fn tick_with(
    home: &Home,
    summary: Option<&FleetSummary>,
    banner: &[String],
) -> anyhow::Result<ExitCode> {
    let _guard = match lock::take(home, Duration::ZERO, false)? {
        Take::Held(guard) => guard,
        Take::Busy | Take::NoLockFile => return Ok(ExitCode::SUCCESS),
    };
    let mut record = match record::load_current(home) {
        Ok(Some(record)) => record,
        Ok(None) | Err(LoadError::Outdated | LoadError::Invalid(_)) => {
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(e.into()),
    };
    if !record.screensaver.enabled {
        return Ok(ExitCode::SUCCESS);
    }
    let text = render(summary, banner);
    if !write_if_changed(home, &mut record, &text)? {
        record.screensaver.first_write_refused = true;
        record::save(home, &record)?;
        eprintln!("{OLD_BACKUP}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Clears what the tick caches about the file: a later first write follows
/// the first-write rules again.
fn clear_cache(screensaver: &mut Screensaver) {
    screensaver.last_written_sha256 = None;
    screensaver.backup_sha256 = None;
    screensaver.had_original = false;
    screensaver.first_write_refused = false;
}

/// Puts the original back (or removes the written file) and prints one line.
/// `Ok(None)` when the file was kept.
fn restore_step(
    home: &Home,
    screensaver: &Screensaver,
    written: &str,
) -> Result<Option<Restored>, FsError> {
    let file = rel(Base::Omarchy, FILE);
    let backup = rel(Base::Omarchy, BACKUP);
    let shown = home.abs(&file).display().to_string();
    let Some(current) = home.read(&file)? else {
        return Ok(None);
    };
    if sha256_hex(&current.bytes) != written {
        println!("kept (changed): {shown}");
        return Ok(None);
    }
    if screensaver.had_original {
        let Some(original) = home.read(&backup)? else {
            eprintln!("fleetdeck: warning: the backup is missing, so {shown} was kept");
            return Ok(None);
        };
        home.write(&file, &original.bytes, 0o644)?;
        println!("Screensaver: restored {shown}");
        Ok(Some(Restored::Backup))
    } else {
        home.remove(&file)?;
        println!("Screensaver: removed {shown}");
        Ok(Some(Restored::Removed))
    }
}

/// Restores the screensaver file (`off` and `uninstall`). It reads the
/// screensaver part without clearing it, and does nothing when no text was
/// written. A file that is absent or differs from the last written text is
/// kept. Else the backup is copied over the file (the backup file stays), or
/// the written file is removed when there was no original. Only after that
/// the cache fields are cleared, and `restored` is set. On an error no field
/// of the record changes, one line goes to stderr, and the error is returned.
///
/// # Errors
/// A file cannot be read, written or removed.
pub fn restore(home: &Home, record: &mut Record) -> anyhow::Result<()> {
    let Some(written) = record.screensaver.last_written_sha256.clone() else {
        return Ok(());
    };
    match restore_step(home, &record.screensaver, &written) {
        Ok(done) => {
            if done.is_some() {
                record.screensaver.restored = done;
            }
            clear_cache(&mut record.screensaver);
            Ok(())
        }
        Err(e) => {
            eprintln!("fleetdeck: the screensaver could not be restored: {e}");
            Err(anyhow::Error::new(e))
        }
    }
}

/// Prints where the kept original is, when the backup file exists.
pub fn report_backup(home: &Home) {
    let backup = rel(Base::Omarchy, BACKUP);
    match home.read(&backup) {
        Ok(Some(_)) => println!(
            "The original screensaver is kept in {}.",
            home.abs(&backup).display()
        ),
        Ok(None) => {}
        Err(e) => warn(&format!("the backup could not be read: {e}")),
    }
}

#[cfg(test)]
#[path = "screensaver_cmd_tests.rs"]
mod cmd_tests;
#[cfg(test)]
#[path = "screensaver_tests.rs"]
mod tests;
