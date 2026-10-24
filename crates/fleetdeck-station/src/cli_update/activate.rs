//! The rollback copy, the activation, the check of the active copy and the
//! local way back (design §3.6 A1, A2, V and §3.6.2).
//!
//! Nothing here uses the network. [`restore`] reads only local files and is
//! idempotent: the start recovery can run it again after a second crash.

use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::cli_update::reason;
use fleetdeck_proto::event::ProviderKind;

use super::detect::Tool;
use super::stage::{Ctx, Failure, free_bytes, probe_checked, sha256_file};
use super::tree_digest::tree_digest;
use crate::store::cli_update_journal::{Activation, MovedEntry, RecoveryRecord};

/// The rollback copy is missing or its checksum is wrong.
pub const ROLLBACK_MISSING: &str = "rollback_missing";
/// A rename or a copy of the way back failed.
pub const RESTORE_FAILED: &str = "restore_failed";
/// The way back finished, but its check failed.
pub const RESTORE_UNVERIFIED: &str = "restore_unverified";

/// The file system calls that a test replaces (`linkat` with `EXDEV`).
pub trait Fs: Send + Sync {
    /// # Errors
    /// The `linkat` error.
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// The real file system.
pub struct RealFs;

impl Fs for RealFs {
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::hard_link(from, to)
    }
}

fn private_dir(p: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(p)
}

/// Removes a file, a link or a folder tree. A missing path is fine.
pub fn remove_path(p: &Path) {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_dir() => {
            let _ = std::fs::remove_dir_all(p);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(p);
        }
        Err(_) => {}
    }
}

/// Removes every entry of `dir` whose name starts with `prefix` and whose
/// rest is not in `keep` (§3.6 "Clean-up").
pub fn clean_stale(dir: &Path, prefix: &str, keep: &[String]) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(prefix)) else {
            continue;
        };
        if !keep.iter().any(|k| k == rest) {
            remove_path(&entry.path());
        }
    }
}

/// `<station dir>/cli-update/rollback/<attempt_id>`.
#[must_use]
pub fn rollback_dir(station_dir: &Path, attempt_id: &str) -> PathBuf {
    station_dir.join("cli-update/rollback").join(attempt_id)
}

/// Replaces the link `link` by a link to `dest`: a new link next to it,
/// then one rename.
///
/// # Errors
/// A file system error.
pub fn swap_link(link: &Path, dest: &Path, attempt_id: &str) -> io::Result<()> {
    let mut name = link.file_name().unwrap_or_default().to_owned();
    name.push(format!(".fd-new-{attempt_id}"));
    let tmp = link.with_file_name(name);
    remove_path(&tmp);
    std::os::unix::fs::symlink(dest, &tmp)?;
    std::fs::rename(&tmp, link)
}

// ---- Native Claude ------------------------------------------------------

/// §3.6 native A1: `rollback/<attempt_id>/claude` is a hard link of `real`
/// (`versions/<from>`), or a copy when the link fails with `EXDEV`. Its
/// SHA-256 must equal `rollback_sha256`. The caller commits the journal row
/// (phase `activating`) with the returned value before A2.
///
/// # Errors
/// `Refused(disk_full)`, `Refused(checksum_mismatch)`.
pub fn rollback_native(
    fs: &dyn Fs,
    station_dir: &Path,
    attempt_id: &str,
    real: &Path,
    rollback_sha256: &str,
) -> Result<Activation, Failure> {
    let dir = rollback_dir(station_dir, attempt_id);
    let copy = dir.join("claude");
    let failed = Failure::Failed(reason::INSTALL_FAILED);
    private_dir(&dir).map_err(|_| failed)?;
    remove_path(&copy);
    if let Err(e) = fs.hard_link(real, &copy) {
        if e.raw_os_error() != Some(rustix::io::Errno::XDEV.raw_os_error()) {
            return Err(failed);
        }
        let size = std::fs::metadata(real).map_err(|_| failed)?.len();
        if free_bytes(&dir).map_or(true, |f| f < size) {
            return Err(Failure::Refused(reason::DISK_FULL));
        }
        std::fs::copy(real, &copy).map_err(|_| failed)?;
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755))
            .map_err(|_| failed)?;
    }
    if sha256_file(&copy).ok().as_deref() != Some(rollback_sha256) {
        return Err(Failure::Refused(reason::CHECKSUM_MISMATCH));
    }
    Ok(Activation {
        rollback_path: copy,
        rollback_sha256: Some(rollback_sha256.to_owned()),
        moved: Vec::new(),
    })
}

/// §3.6 native A2: the link `<L>.fd-new-<attempt_id>` to `dest`
/// (`versions/<target>`, absolute), renamed over `L`.
///
/// # Errors
/// A file system error.
pub fn activate_native(link: &Path, dest: &Path, attempt_id: &str) -> io::Result<()> {
    swap_link(link, dest, attempt_id)
}

/// `launcher` resolves to `dest`, and `--version` of the launcher is
/// `version` (the handshake is not part of this check when `handshake` is
/// false).
async fn check_active(
    ctx: &Ctx<'_>,
    kind: ProviderKind,
    launcher: &Path,
    env: &ChildEnv,
    version: &str,
    handshake: bool,
) -> Result<(), Failure> {
    let probe = probe_checked(ctx, kind, launcher, env).await?;
    if probe.version.as_deref() != Some(version) {
        return Err(Failure::Failed(reason::VERSION_MISMATCH));
    }
    if handshake && probe.handshake.is_err() {
        return Err(Failure::Failed(reason::HANDSHAKE_FAILED));
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// §3.6 native V.
///
/// # Errors
/// `Failed(version_mismatch)`, `ProcessLeft`.
pub async fn verify_native(
    ctx: &Ctx<'_>,
    launcher: &Path,
    dest: &Path,
    target: &str,
    env: &ChildEnv,
) -> Result<(), Failure> {
    if !same_file(launcher, dest) {
        return Err(Failure::Failed(reason::VERSION_MISMATCH));
    }
    check_active(ctx, ProviderKind::Claude, launcher, env, target, false).await
}

// ---- Codex by npm or Bun ------------------------------------------------

/// §3.6 Codex A1: the digest of each `modules/<t>` of `moved` (`None` when
/// it does not exist) and `rollback_path = <R>/old`. The originals stay in
/// place until A2.
///
/// # Errors
/// A file system error.
pub fn rollback_codex(modules: &Path, root: &Path, moved: &[String]) -> io::Result<Activation> {
    let old = root.join("old");
    private_dir(&old)?;
    let mut entries = Vec::new();
    for t in moved {
        let at = modules.join(t);
        let digest = if std::fs::symlink_metadata(&at).is_ok() {
            Some(tree_digest(&at)?)
        } else {
            None
        };
        entries.push(MovedEntry {
            name: t.clone(),
            digest,
        });
    }
    Ok(Activation {
        rollback_path: old,
        rollback_sha256: None,
        moved: entries,
    })
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

fn move_to(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)
}

/// §3.6 Codex A2: for each `t` of `moved`, `modules/<t>` goes to
/// `<R>/old/<t>` (when it exists), then `SM/<t>` goes to `modules/<t>`.
///
/// # Errors
/// A file system error.
pub fn activate_codex(
    modules: &Path,
    sm: &Path,
    root: &Path,
    moved: &[MovedEntry],
) -> io::Result<()> {
    for e in moved {
        let at = modules.join(&e.name);
        if exists(&at) {
            move_to(&at, &root.join("old").join(&e.name))?;
        }
        move_to(&sm.join(&e.name), &at)?;
    }
    Ok(())
}

/// §3.6 Codex V: the launcher resolves into `modules/@openai/codex/`, and
/// its `--version` is `target`.
///
/// # Errors
/// `Failed(version_mismatch)`, `ProcessLeft`.
pub async fn verify_codex(
    ctx: &Ctx<'_>,
    launcher: &Path,
    modules: &Path,
    target: &str,
    env: &ChildEnv,
) -> Result<(), Failure> {
    let inside = matches!(
        (std::fs::canonicalize(launcher), std::fs::canonicalize(modules)),
        (Ok(l), Ok(m)) if l.starts_with(m.join("@openai/codex"))
    );
    if !inside {
        return Err(Failure::Failed(reason::VERSION_MISMATCH));
    }
    check_active(ctx, ProviderKind::Codex, launcher, env, target, false).await
}

// ---- The way back (§3.6.2) ----------------------------------------------

/// The way back from the local rollback copy. No network. `env` comes from
/// `probe_env`. `tool` is the detected install (`Native` for Claude, `Npm`
/// or `Bun` for Codex).
///
/// # Errors
/// `Failed(rollback_missing)`, `Failed(restore_failed)`,
/// `Failed(restore_unverified)` and `ProcessLeft`. The caller then writes
/// `RollbackFailed` and the quarantine.
pub async fn restore(
    ctx: &Ctx<'_>,
    rec: &RecoveryRecord,
    tool: &Tool,
    env: &ChildEnv,
) -> Result<(), Failure> {
    match tool {
        Tool::Native { link, versions, .. } => restore_native(ctx, rec, link, versions, env).await,
        Tool::Npm { modules, .. } | Tool::Bun { modules, .. } => {
            restore_codex(ctx, rec, modules, env).await
        }
        Tool::None => Err(Failure::Failed(RESTORE_FAILED)),
    }
}

async fn restore_native(
    ctx: &Ctx<'_>,
    rec: &RecoveryRecord,
    link: &Path,
    versions: &Path,
    env: &ChildEnv,
) -> Result<(), Failure> {
    let missing = Failure::Failed(ROLLBACK_MISSING);
    let failed = Failure::Failed(RESTORE_FAILED);
    let sum = rec.rollback_sha256.as_deref().ok_or(missing)?;
    let from = versions.join(&rec.from_version);
    let good = |p: &Path| sha256_file(p).ok().as_deref() == Some(sum);
    if !(exists(&from) && good(&from)) {
        let copy = rec.rollback_path.as_deref().ok_or(missing)?;
        let tmp = versions.join(format!(".fd-stage-restore-{}", rec.attempt_id));
        remove_path(&tmp);
        std::fs::copy(copy, &tmp).map_err(|_| missing)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|_| failed)?;
        if !good(&tmp) {
            remove_path(&tmp);
            return Err(missing);
        }
        std::fs::rename(&tmp, &from).map_err(|_| failed)?;
    }
    swap_link(link, &from, &rec.attempt_id).map_err(|_| failed)?;
    let unverified = Failure::Failed(RESTORE_UNVERIFIED);
    if !same_file(&rec.launcher, &from) || !good(&from) {
        return Err(unverified);
    }
    check_active(
        ctx,
        rec.provider,
        &rec.launcher,
        env,
        &rec.from_version,
        true,
    )
    .await
    .map_err(|f| {
        if f == Failure::ProcessLeft {
            f
        } else {
            unverified
        }
    })
}

async fn restore_codex(
    ctx: &Ctx<'_>,
    rec: &RecoveryRecord,
    modules: &Path,
    env: &ChildEnv,
) -> Result<(), Failure> {
    let failed = Failure::Failed(RESTORE_FAILED);
    let root = &rec.stage_root;
    let old = rec
        .rollback_path
        .clone()
        .unwrap_or_else(|| root.join("old"));
    let discard = |at: &Path, t: &str| -> io::Result<()> {
        let mut k = 0;
        let to = loop {
            let p = root.join("failed").join(format!("{k}-{t}"));
            if !exists(&p) {
                break p;
            }
            k += 1;
        };
        move_to(at, &to)
    };
    for e in rec.moved.iter().rev() {
        let (at, saved) = (modules.join(&e.name), old.join(&e.name));
        let step = if e.digest.is_some() {
            if exists(&saved) {
                (if exists(&at) {
                    discard(&at, &e.name)
                } else {
                    Ok(())
                })
                .and_then(|()| move_to(&saved, &at))
            } else {
                Ok(())
            }
        } else if exists(&at) {
            discard(&at, &e.name)
        } else {
            Ok(())
        };
        step.map_err(|_| failed)?;
    }
    let unverified = Failure::Failed(RESTORE_UNVERIFIED);
    for e in &rec.moved {
        let now = modules.join(&e.name);
        let same = match &e.digest {
            Some(d) => tree_digest(&now).ok().as_ref() == Some(d),
            None => !exists(&now),
        };
        if !same {
            return Err(unverified);
        }
    }
    check_active(
        ctx,
        rec.provider,
        &rec.launcher,
        env,
        &rec.from_version,
        true,
    )
    .await
    .map_err(|f| {
        if f == Failure::ProcessLeft {
            f
        } else {
            unverified
        }
    })
}

#[cfg(test)]
#[path = "activate_tests.rs"]
mod tests;
