//! Pre-upgrade backup of the deck and station databases (Fleet Git design
//! §3.9).
//!
//! Before `prepare()` runs pending migrations on an existing database, it
//! writes a consistent copy with `VACUUM INTO` to `<data>/backups/`. No
//! backup, no migration: a failure gives [`DbError::Backup`] and the
//! migrations do not run. `FLEETDECK_SKIP_UPGRADE_BACKUP=1` is the only bypass.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use rusqlite_migration::Migrations;

use crate::db::DbError;
use crate::paths::ensure_private_dir;

/// The number of backups of one database that stay (the newest ones).
const KEEP: usize = 3;

/// What the caller decides for one backup run.
pub(crate) struct BackupOptions {
    /// `FLEETDECK_SKIP_UPGRADE_BACKUP=1`: upgrade without a backup.
    pub skip: bool,
    /// The clock for the file name (unix ms).
    pub now_ms: i64,
}

/// Writes the pre-upgrade backup when `db_path` is `<data>/deck/deck.db` or
/// `<data>/station/station.db`, the database exists (`user_version > 0`) and
/// migrations are pending. Returns the backup file, or `None` when no backup
/// was needed or allowed.
///
/// # Errors
/// [`DbError::Backup`] when the backup cannot be written or the free space
/// is too small; the caller must then not migrate.
pub(crate) fn backup_before_upgrade(
    conn: &Connection,
    db_path: &Path,
    migrations: &Migrations<'_>,
    opts: &BackupOptions,
) -> Result<Option<PathBuf>, DbError> {
    backup_with_free(conn, db_path, migrations, opts, &free_bytes)
}

/// The same, with the free-space probe injected (tests).
pub(crate) fn backup_with_free(
    conn: &Connection,
    db_path: &Path,
    migrations: &Migrations<'_>,
    opts: &BackupOptions,
    free: &dyn Fn(&Path) -> Result<u64, String>,
) -> Result<Option<PathBuf>, DbError> {
    let Some((kind, dir)) = layout(db_path) else {
        return Ok(None);
    };
    let from: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let pending = migrations.pending_migrations(conn)?;
    if from <= 0 || pending <= 0 {
        return Ok(None);
    }
    if opts.skip {
        tracing::warn!(
            "upgrading the {kind} database without a pre-upgrade backup (FLEETDECK_SKIP_UPGRADE_BACKUP=1)"
        );
        return Ok(None);
    }
    let to = from + i64::from(pending);
    let file = dir.join(format!("{kind}-v{from}-to-v{to}-{}.db", opts.now_ms));
    let fail = |reason: String| DbError::Backup {
        path: file.clone(),
        reason,
    };
    ensure_private_dir(&dir).map_err(|e| fail(e.to_string()))?;
    // The main file and the WAL, twice.
    let size = file_len(db_path) + file_len(&wal_path(db_path));
    let needed = size.saturating_mul(2);
    let have = free(&dir).map_err(&fail)?;
    if have < needed {
        return Err(fail(format!("needs {needed} bytes free, has {have}")));
    }
    let target = file
        .to_str()
        .ok_or_else(|| fail("the backup path is not valid UTF-8".to_owned()))?
        .to_owned();
    // `VACUUM INTO` accepts an empty file: create it owner-only first.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .map_err(|e| fail(e.to_string()))?;
    let copied = conn
        .execute("VACUUM INTO ?1", [&target])
        .map_err(|e| e.to_string())
        .and_then(|_| {
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())
        });
    if let Err(reason) = copied {
        if let Err(e) = fs::remove_file(&file) {
            tracing::warn!(error = %e, path = %file.display(), "cannot remove the partial backup");
        }
        return Err(fail(reason));
    }
    prune(&dir, kind);
    Ok(Some(file))
}

/// `(kind, backups folder)` for `<data>/<kind>/<kind>.db`, else `None`.
fn layout(db_path: &Path) -> Option<(&'static str, PathBuf)> {
    let kind = match db_path.file_stem()?.to_str()? {
        "deck" => "deck",
        "station" => "station",
        _ => return None,
    };
    if db_path.extension()? != "db" {
        return None;
    }
    let parent = db_path.parent()?;
    if parent.file_name()? != kind {
        return None;
    }
    Some((kind, parent.parent()?.join("backups")))
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

fn wal_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

fn free_bytes(dir: &Path) -> Result<u64, String> {
    let st = rustix::fs::statvfs(dir).map_err(|e| format!("statvfs: {e}"))?;
    Ok(st.f_bavail.saturating_mul(st.f_frsize))
}

/// The unix ms of `<kind>-v<from>-to-v<to>-<ms>.db`, else `None`.
fn backup_ms(name: &str, kind: &str) -> Option<u64> {
    let rest = name
        .strip_prefix(kind)?
        .strip_prefix("-v")?
        .strip_suffix(".db")?;
    let (from, tail) = rest.split_once("-to-v")?;
    let (to, ms) = tail.split_once('-')?;
    from.parse::<u64>().ok()?;
    to.parse::<u64>().ok()?;
    ms.parse().ok()
}

/// Keeps the newest [`KEEP`] backups of `kind` (by the ms in the name). Only
/// a file with a matching name is deleted; a failed delete is logged.
fn prune(dir: &Path, kind: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let ms = backup_ms(e.file_name().to_str()?, kind)?;
            Some((ms, e.path()))
        })
        .collect();
    found.sort();
    let old = found.len().saturating_sub(KEEP);
    for (_, path) in found.into_iter().take(old) {
        if let Err(e) = fs::remove_file(&path) {
            tracing::warn!(error = %e, path = %path.display(), "cannot remove an old backup");
        }
    }
}

#[cfg(test)]
#[path = "db_backup_tests.rs"]
mod tests;
