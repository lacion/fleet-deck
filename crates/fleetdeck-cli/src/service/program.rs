//! The program that a service runs (design service.md §4): a managed copy of
//! the running binary under the launcher root, never the build directory.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use fleetdeck_station::upgrade::{CURRENT, Launcher};

/// The mark of a release that the service install made. The releases of the
/// upgrade flow have another mark and are never removed here.
const SVC_MARK: &str = "-svc-";

/// The target of the `current` link of `root`, made absolute.
fn current_target(root: &Path) -> Option<PathBuf> {
    fs::read_link(root.join(CURRENT))
        .ok()
        .map(|target| root.join(target))
}

fn same_bytes(a: &Path, b: &Path) -> anyhow::Result<bool> {
    let len = |p: &Path| {
        fs::metadata(p)
            .map(|m| m.len())
            .with_context(|| format!("cannot read {}", p.display()))
    };
    if len(a)? != len(b)? {
        return Ok(false);
    }
    let read = |p: &Path| fs::read(p).with_context(|| format!("cannot read {}", p.display()));
    Ok(read(a)? == read(b)?)
}

/// True when `ensure` would copy: `current` is missing, broken, or its target
/// file is not byte-equal to `exe`. Reads only.
pub fn needs_copy(root: &Path, exe: &Path) -> anyhow::Result<bool> {
    let Some(target) = current_target(root) else {
        return Ok(true);
    };
    match fs::metadata(&target) {
        Ok(meta) if meta.is_file() => Ok(!same_bytes(&target, exe)?),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", target.display())),
    }
}

/// How this program compares with the copy that the service runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyState {
    Same,
    Newer,
    Older,
    Missing,
}

/// `Same`: the bytes are equal. `Missing`: no copy. Else `Newer` when the
/// modification time of `exe` is later than that of the copy, `Older` when it
/// is not (an older fleetdeck earlier on PATH must never replace the copy).
/// Reads only.
pub fn copy_state(root: &Path, exe: &Path) -> anyhow::Result<CopyState> {
    let Some(target) = current_target(root) else {
        return Ok(CopyState::Missing);
    };
    let copy_meta = match fs::metadata(&target) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Ok(CopyState::Missing),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(CopyState::Missing),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", target.display())),
    };
    if same_bytes(&target, exe)? {
        return Ok(CopyState::Same);
    }
    let modified = |meta: &fs::Metadata, p: &Path| {
        meta.modified()
            .with_context(|| format!("cannot read the time of {}", p.display()))
    };
    let exe_meta = fs::metadata(exe).with_context(|| format!("cannot read {}", exe.display()))?;
    if modified(&exe_meta, exe)? > modified(&copy_meta, &target)? {
        Ok(CopyState::Newer)
    } else {
        Ok(CopyState::Older)
    }
}

/// The dry-run line of the program step; `None` when no copy would be made.
pub fn dry_line(
    root: &Path,
    exe: &Path,
    version: &str,
    ulid: &str,
) -> anyhow::Result<Option<String>> {
    if !needs_copy(root, exe)? {
        return Ok(None);
    }
    let release = Launcher::new(root).release_path(version, &format!("svc-{ulid}"));
    let release = release.parent().unwrap_or(root);
    Ok(Some(format!(
        "$ (copy {} to {} and point {} at it)",
        exe.display(),
        release.display(),
        root.join(CURRENT).display()
    )))
}

/// Makes `<root>/current` the program of the service and returns that path.
/// A copy of `exe` is made when `needs_copy`; then the old `-svc-` releases
/// are pruned.
pub fn ensure(root: &Path, exe: &Path, version: &str, ulid: &str) -> anyhow::Result<PathBuf> {
    let link = root.join(CURRENT);
    if !needs_copy(root, exe)? {
        return Ok(link);
    }
    let launcher = Launcher::new(root);
    let release = launcher
        .install(exe, version, &format!("svc-{ulid}"))
        .context("cannot copy the program")?;
    fs::set_permissions(&release, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("cannot set the mode of {}", release.display()))?;
    launcher
        .set_current(&release)
        .context("cannot point the program link at the copy")?;
    prune(root, &launcher)?;
    Ok(link)
}

/// Removes every `-svc-` release that is neither the target of `current` nor
/// of `previous`.
fn prune(root: &Path, launcher: &Launcher) -> anyhow::Result<()> {
    let keep: Vec<PathBuf> = [launcher.current(), launcher.previous()]
        .into_iter()
        .flatten()
        .map(|target| root.join(target))
        .collect();
    let releases = root.join("releases");
    let entries = match fs::read_dir(&releases) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", releases.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", releases.display()))?;
        let dir = entry.path();
        let is_svc = entry.file_name().to_string_lossy().contains(SVC_MARK);
        if !is_svc || !dir.is_dir() || keep.contains(&dir.join("fleetdeck")) {
            continue;
        }
        fs::remove_dir_all(&dir)
            .with_context(|| format!("cannot remove the old release {}", dir.display()))?;
    }
    Ok(())
}
