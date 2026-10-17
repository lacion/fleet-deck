//! Station self-upgrade (PLAN Phase 7, M3 7-3; ADR-005 §3.3 and §5.3;
//! DL-128).
//!
//! The steps of one update, each with its own update id:
//!
//! 1. **Drain** the running station (`station drain`), then stop it.
//! 2. **Snapshot** `station.db` with its `-wal` and `-shm` files while no
//!    station runs ([`snapshot`]).
//! 3. **Trial**: the new build migrates a disposable copy and checks that it
//!    speaks the protocol of every unresolved shim row ([`trial`]). A trial
//!    never attaches, acknowledges or reconciles.
//! 4. **Activate**: the stable launcher link points to the new release
//!    ([`Launcher::activate`]). Only a trialed update activates.
//! 5. The new station marks the update `live_advanced` before it opens the
//!    database ([`mark_live`]).
//!
//! A rollback ([`rollback_plan`]) restores the snapshot only when no live
//! state advanced and no shim is live or unresolved; else it swaps the
//! binary only. A restored snapshot gets a new incarnation, so that a deck
//! drops its cursor (`incarnation_mismatch`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The suffixes of the SQLite files of one database.
const SQLITE_FILES: [&str; 3] = ["", "-wal", "-shm"];
/// The name of the stable launcher link in the launcher root.
pub const CURRENT: &str = "current";
/// The name of the previous release link in the launcher root.
pub const PREVIOUS: &str = "previous";

/// Why an upgrade step failed.
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the database copy did not open or migrate: {0}")]
    Database(String),
    #[error(
        "shim `{shim_id}` speaks shim protocol {protocol}; this build speaks {supported:?}: \
         the upgrade is not activated"
    )]
    ShimIncompatible {
        shim_id: String,
        protocol: u32,
        supported: Vec<u32>,
    },
    #[error("update `{0}` is not trialed; only a trialed update activates")]
    NotTrialed(String),
    #[error("update record {path}: {message}")]
    Record { path: PathBuf, message: String },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> UpgradeError + '_ {
    move |source| UpgradeError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The state of one update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    Prepared,
    Trialed,
    Activated,
    Failed,
    RolledBack,
}

/// The durable record of one update: `<upgrades>/<update_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRecord {
    pub update_id: String,
    pub from_version: String,
    pub to_version: String,
    pub state: UpdateState,
    /// True after the new build opened the live database: from then on a
    /// rollback never restores the snapshot (ADR-005 §3.3).
    pub live_advanced: bool,
    pub snapshot: PathBuf,
}

impl UpdateRecord {
    fn path(dir: &Path, update_id: &str) -> PathBuf {
        dir.join(format!("{update_id}.json"))
    }

    /// Writes the record (temp file, then rename).
    ///
    /// # Errors
    /// [`UpgradeError::Io`] or [`UpgradeError::Record`].
    pub fn save(&self, dir: &Path) -> Result<(), UpgradeError> {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
        let path = Self::path(dir, &self.update_id);
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(self).map_err(|e| UpgradeError::Record {
            path: path.clone(),
            message: e.to_string(),
        })?;
        fs::write(&tmp, body).map_err(io_err(&tmp))?;
        fs::rename(&tmp, &path).map_err(io_err(&path))
    }

    /// Reads the record of `update_id`.
    ///
    /// # Errors
    /// [`UpgradeError::Io`] or [`UpgradeError::Record`].
    pub fn load(dir: &Path, update_id: &str) -> Result<Self, UpgradeError> {
        let path = Self::path(dir, update_id);
        let body = fs::read(&path).map_err(io_err(&path))?;
        serde_json::from_slice(&body).map_err(|e| UpgradeError::Record {
            path,
            message: e.to_string(),
        })
    }
}

/// Every update record in `dir` (none when `dir` does not exist).
///
/// # Errors
/// [`UpgradeError::Io`] or [`UpgradeError::Record`].
pub fn records(dir: &Path) -> Result<Vec<UpdateRecord>, UpgradeError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(dir)(e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.map_err(io_err(dir))?.path();
        if path.extension().is_some_and(|e| e == "json")
            && let Some(id) = path.file_stem().and_then(|s| s.to_str())
        {
            out.push(UpdateRecord::load(dir, id)?);
        }
    }
    out.sort_by(|a, b| a.update_id.cmp(&b.update_id));
    Ok(out)
}

/// Marks every activated update `live_advanced`. The station calls this
/// before it opens the live database.
///
/// # Errors
/// See [`records`] and [`UpdateRecord::save`].
pub fn mark_live(dir: &Path) -> Result<usize, UpgradeError> {
    let mut marked = 0;
    for mut record in records(dir)? {
        if record.state == UpdateState::Activated && !record.live_advanced {
            record.live_advanced = true;
            record.save(dir)?;
            marked += 1;
        }
    }
    Ok(marked)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Copies the main, WAL and SHM files of `db` into `dest` (a directory).
/// The caller makes sure that no station runs. Returns the copied files.
///
/// # Errors
/// [`UpgradeError::Io`]; a missing main file is an error.
pub fn snapshot(db: &Path, dest: &Path) -> Result<Vec<PathBuf>, UpgradeError> {
    fs::create_dir_all(dest).map_err(io_err(dest))?;
    let name = db.file_name().ok_or_else(|| UpgradeError::Io {
        path: db.to_owned(),
        source: io::Error::from(io::ErrorKind::InvalidInput),
    })?;
    let mut copied = Vec::new();
    for suffix in SQLITE_FILES {
        let from = with_suffix(db, suffix);
        let to = with_suffix(&dest.join(name), suffix);
        match fs::copy(&from, &to) {
            Ok(_) => copied.push(to),
            Err(e) if e.kind() == io::ErrorKind::NotFound && !suffix.is_empty() => {}
            Err(e) => return Err(io_err(&from)(e)),
        }
    }
    Ok(copied)
}

/// What the trial found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialReport {
    /// The schema version of the copy after the migration.
    pub schema_version: i64,
    /// The unresolved shim rows: `(shim_id, protocol)`.
    pub shims: Vec<(String, u32)>,
}

/// The trial of a new build: copies the snapshot `db` into `work`, migrates
/// the copy, and checks that `protocols` contains the protocol of every
/// unresolved shim row. It never touches the live database or a shim.
///
/// # Errors
/// [`UpgradeError::ShimIncompatible`] when a shim speaks a protocol that
/// this build does not; [`UpgradeError::Database`] when the copy does not
/// migrate.
pub fn trial(db: &Path, work: &Path, protocols: &[u32]) -> Result<TrialReport, UpgradeError> {
    let copied = snapshot(db, work)?;
    let copy = copied.first().cloned().ok_or_else(|| UpgradeError::Io {
        path: db.to_owned(),
        source: io::Error::from(io::ErrorKind::NotFound),
    })?;
    let database = |e: &dyn std::fmt::Display| UpgradeError::Database(e.to_string());
    let db = fleetdeck_core::db::Db::open(&copy, crate::store::schema::migrations())
        .map_err(|e| database(&e))?;
    drop(db);
    let conn = rusqlite::Connection::open(&copy).map_err(|e| database(&e))?;
    let schema_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| database(&e))?;
    let mut stmt = conn
        .prepare(
            "SELECT shim_id, protocol FROM shims
             WHERE state IN ('starting', 'running')
                OR (state = 'exited' AND COALESCE(reason, 'exited') <> 'exited')
             ORDER BY shim_id",
        )
        .map_err(|e| database(&e))?;
    let shims = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))
        .and_then(Iterator::collect::<rusqlite::Result<Vec<_>>>)
        .map_err(|e| database(&e))?;
    if let Some((shim_id, protocol)) = shims.iter().find(|(_, p)| !protocols.contains(p)) {
        return Err(UpgradeError::ShimIncompatible {
            shim_id: shim_id.clone(),
            protocol: *protocol,
            supported: protocols.to_vec(),
        });
    }
    Ok(TrialReport {
        schema_version,
        shims,
    })
}

/// How a failed or unwanted update goes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackPlan {
    /// Restore the snapshot and the old binary; the database gets a new
    /// incarnation.
    RestoreSnapshot,
    /// Swap the binary only; the live database stays as it is. `reason`:
    /// `live_advanced` or `shim_live`.
    BinaryOnly { reason: &'static str },
}

/// The rollback rule of ADR-005 §3.3: a historical restore only when no live
/// state advanced and no shim is live or unresolved.
#[must_use]
pub fn rollback_plan(record: &UpdateRecord, live_shims: usize) -> RollbackPlan {
    if record.live_advanced {
        RollbackPlan::BinaryOnly {
            reason: "live_advanced",
        }
    } else if live_shims > 0 {
        RollbackPlan::BinaryOnly {
            reason: "shim_live",
        }
    } else {
        RollbackPlan::RestoreSnapshot
    }
}

/// The number of shim rows that are live or unresolved in `db`.
///
/// # Errors
/// [`UpgradeError::Database`].
pub fn live_shims(db: &Path) -> Result<usize, UpgradeError> {
    let conn = rusqlite::Connection::open(db).map_err(|e| UpgradeError::Database(e.to_string()))?;
    conn.query_row(
        "SELECT COUNT(*) FROM shims WHERE state IN ('starting', 'running')
            OR (state = 'exited' AND COALESCE(reason, 'exited') <> 'exited')",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
    .map_err(|e| UpgradeError::Database(e.to_string()))
}

/// Restores the snapshot in `snapshot_dir` over `db` and gives the database
/// the incarnation `incarnation`. Only for [`RollbackPlan::RestoreSnapshot`].
///
/// # Errors
/// [`UpgradeError::Io`] or [`UpgradeError::Database`].
pub fn restore(snapshot_dir: &Path, db: &Path, incarnation: &str) -> Result<(), UpgradeError> {
    let name = db.file_name().ok_or_else(|| UpgradeError::Io {
        path: db.to_owned(),
        source: io::Error::from(io::ErrorKind::InvalidInput),
    })?;
    for suffix in SQLITE_FILES {
        let from = with_suffix(&snapshot_dir.join(name), suffix);
        let to = with_suffix(db, suffix);
        if from.exists() {
            fs::copy(&from, &to).map_err(io_err(&from))?;
        } else if to.exists() {
            fs::remove_file(&to).map_err(io_err(&to))?;
        }
    }
    let conn = rusqlite::Connection::open(db).map_err(|e| UpgradeError::Database(e.to_string()))?;
    conn.execute(
        "UPDATE meta SET incarnation_id = ?1 WHERE id = 1",
        [incarnation],
    )
    .map_err(|e| UpgradeError::Database(e.to_string()))?;
    Ok(())
}

/// The stable launcher: `<root>/current` links to the active release
/// binary, `<root>/previous` to the one before. Service units run
/// `<root>/current`.
#[derive(Debug, Clone)]
pub struct Launcher {
    root: PathBuf,
}

impl Launcher {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The release binary that `current` links to.
    #[must_use]
    pub fn current(&self) -> Option<PathBuf> {
        fs::read_link(self.root.join(CURRENT)).ok()
    }

    /// The release binary that `previous` links to.
    #[must_use]
    pub fn previous(&self) -> Option<PathBuf> {
        fs::read_link(self.root.join(PREVIOUS)).ok()
    }

    /// Copies `binary` to `<root>/releases/<version>-<update_id>/fleetdeck`.
    ///
    /// # Errors
    /// [`UpgradeError::Io`].
    pub fn install(
        &self,
        binary: &Path,
        version: &str,
        update_id: &str,
    ) -> Result<PathBuf, UpgradeError> {
        let to = self.release_path(version, update_id);
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir).map_err(io_err(dir))?;
        }
        fs::copy(binary, &to).map_err(io_err(binary))?;
        Ok(to)
    }

    /// The release binary of `version` from update `update_id`.
    #[must_use]
    pub fn release_path(&self, version: &str, update_id: &str) -> PathBuf {
        self.root
            .join("releases")
            .join(format!("{version}-{update_id}"))
            .join("fleetdeck")
    }

    /// Points `current` at `release` (atomic rename of a new link) and
    /// `previous` at the old target. Only a trialed update activates.
    ///
    /// # Errors
    /// [`UpgradeError::NotTrialed`] or [`UpgradeError::Io`].
    pub fn activate(&self, record: &UpdateRecord, release: &Path) -> Result<(), UpgradeError> {
        if record.state != UpdateState::Trialed {
            return Err(UpgradeError::NotTrialed(record.update_id.clone()));
        }
        self.point(release)
    }

    /// The first update of a node: installs `binary` (the running build) as
    /// the base release and points `current` at it, so that a rollback has
    /// a previous release. Does nothing when `current` exists.
    ///
    /// # Errors
    /// [`UpgradeError::Io`].
    pub fn adopt(&self, binary: &Path, version: &str) -> Result<(), UpgradeError> {
        if self.current().is_some() {
            return Ok(());
        }
        let base = self.install(binary, version, "base")?;
        self.point(&base)
    }

    /// Points `current` back at `previous` (binary-only rollback).
    ///
    /// # Errors
    /// [`UpgradeError::Io`] when no previous release exists.
    pub fn roll_back(&self) -> Result<(), UpgradeError> {
        let previous = self.previous().ok_or_else(|| UpgradeError::Io {
            path: self.root.join(PREVIOUS),
            source: io::Error::from(io::ErrorKind::NotFound),
        })?;
        self.point(&previous)
    }

    /// Points `current` at `release` without an update record (the service
    /// install, design service.md §4). `previous` follows the old target.
    ///
    /// # Errors
    /// [`UpgradeError::Io`].
    pub fn set_current(&self, release: &Path) -> Result<(), UpgradeError> {
        self.point(release)
    }

    fn point(&self, release: &Path) -> Result<(), UpgradeError> {
        fs::create_dir_all(&self.root).map_err(io_err(&self.root))?;
        if let Some(old) = self.current() {
            swap_link(&self.root, PREVIOUS, &old)?;
        }
        swap_link(&self.root, CURRENT, release)
    }
}

fn swap_link(root: &Path, name: &str, target: &Path) -> Result<(), UpgradeError> {
    let link = root.join(name);
    let tmp = root.join(format!(".{name}.tmp"));
    if fs::symlink_metadata(&tmp).is_ok() {
        fs::remove_file(&tmp).map_err(io_err(&tmp))?;
    }
    std::os::unix::fs::symlink(target, &tmp).map_err(io_err(&tmp))?;
    fs::rename(&tmp, &link).map_err(io_err(&link))
}

#[cfg(test)]
#[path = "upgrade_tests.rs"]
mod tests;
