//! The ambient screensaver (R13 P4 task G, `omarchy-p4.md` §3.8).
//!
//! The hidden job `fleetdeck desktop screensaver-run` renders the fleet
//! summary of the local station into `<omarchy>/branding/screensaver.txt`
//! every 60 seconds, and writes only when the text changed. The first write
//! copies the original file to `screensaver.txt.fleetdeck-backup` (an
//! exclusive create; an existing backup is never replaced, and the job
//! writes over a file only when that file is identical to an existing
//! backup). `omarchy uninstall` calls [`restore`].
//!
//! The text is ASCII only (any other character is `?`), at most 40 lines of
//! 100 columns, and every line ends with `\n`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_proto::event::RequestKind;
use fleetdeck_proto::fleet_summary::{DeckState, FleetSummary, NeedsYouItem};
use fleetdeck_station::local::socket_path;

use super::assets::Base;
use super::fsx::{FsError, Home};
use super::record::{self, Record, Screensaver, rel, sha256_hex};
use super::{STATION_LIMIT, digits, omarchy_env, open_home, tool};
use crate::desktop_feed::{Answer, ask, corrected};

/// The screensaver file, relative to `<omarchy>`.
pub const FILE: &str = "branding/screensaver.txt";
/// The backup of the original file, relative to `<omarchy>`.
pub const BACKUP: &str = "branding/screensaver.txt.fleetdeck-backup";
/// The time between two renders.
pub const CYCLE: Duration = Duration::from_secs(60);
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

fn enabled(home: &Home) -> anyhow::Result<Option<Record>> {
    Ok(record::load(home)?.filter(|r| r.screensaver.enabled))
}

/// The line when an old backup blocks the first write.
const OLD_BACKUP: &str = "fleetdeck: an old screensaver backup exists and differs from the current file; the screensaver stays off.";

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

/// One cycle: write `text` when it differs from the last written text.
/// `Ok(false)` when an old backup blocks the first write (the job ends).
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
    record::save(home, record)?;
    Ok(true)
}

/// The loop of the job. It ends with `Ok` when the record is gone or
/// `screensaver.enabled` is false at the start of a cycle, or when an old
/// backup blocks the first write (one line on stderr).
///
/// # Errors
/// The record cannot be read or is not valid.
pub async fn job<F, Fut>(
    home: &Home,
    env: &ChildEnv,
    cycle: Duration,
    mut fetch: F,
) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<FleetSummary>>,
{
    loop {
        if enabled(home)?.is_none() {
            return Ok(());
        }
        let summary = fetch().await;
        let banner = match &summary {
            Some(_) => banner(env).await,
            None => Vec::new(),
        };
        let text = render(summary.as_ref(), &banner);
        // The record again: an uninstall during the call ends the job.
        let Some(mut record) = enabled(home)? else {
            return Ok(());
        };
        match write_if_changed(home, &mut record, &text) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("{OLD_BACKUP}");
                return Ok(());
            }
            Err(e) => warn(&format!("{e:#}")),
        }
        tokio::time::sleep(cycle).await;
    }
}

/// `fleetdeck desktop screensaver-run`.
///
/// # Errors
/// The config home cannot be opened, or the record is not valid.
pub async fn run(dirs: &NodeDirs) -> anyhow::Result<()> {
    let home = open_home()?;
    if enabled(&home)?.is_none() {
        return Ok(());
    }
    let socket: PathBuf = socket_path(&dirs.station());
    let env = omarchy_env();
    job(&home, &env, CYCLE, || fetch(&socket)).await
}

/// Puts the original back (or removes the written file) and prints one line.
fn restore_file(home: &Home, screensaver: &Screensaver, shown: &str) -> Result<(), FsError> {
    let file = rel(Base::Omarchy, FILE);
    let backup = rel(Base::Omarchy, BACKUP);
    if screensaver.had_original {
        let Some(original) = home.read(&backup)? else {
            warn(&format!("the backup is missing, so {shown} was kept"));
            return Ok(());
        };
        home.write(&file, &original.bytes, 0o644)?;
        home.remove(&backup)?;
        println!("Screensaver: restored {shown}");
    } else {
        home.remove(&file)?;
        println!("Screensaver: removed {shown}");
    }
    Ok(())
}

/// Restores the screensaver at `omarchy uninstall`. It acts only when the
/// record says that the screensaver was on, and the current file is still
/// what the job wrote. Then the record field is reset (also after a kept
/// file), so the job ends at its next cycle.
pub fn restore(home: &Home, record: &mut Record) {
    if !record.screensaver.enabled {
        return;
    }
    let screensaver = std::mem::take(&mut record.screensaver);
    let Some(written) = screensaver.last_written_sha256.as_deref() else {
        return;
    };
    let file = rel(Base::Omarchy, FILE);
    let shown = home.abs(&file).display().to_string();
    match home.read(&file) {
        Ok(None) => {}
        Ok(Some(current)) if sha256_hex(&current.bytes) == written => {
            if let Err(e) = restore_file(home, &screensaver, &shown) {
                warn(&format!("{shown} was kept: {e}"));
            }
        }
        Ok(Some(_)) => println!("kept (changed): {shown}"),
        Err(e) => warn(&format!("{shown} was kept: {e}")),
    }
}

#[cfg(test)]
#[path = "screensaver_tests.rs"]
mod tests;
