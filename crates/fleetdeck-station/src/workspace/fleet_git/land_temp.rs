//! The temporary folder of a land (FG-8, design §11.6): where it lies, how
//! the station makes its parent folder `.fleetdeck-land` without following a
//! link, and how it identifies a folder. The Git work in it is in `land.rs`;
//! the store rows are `store/land_temps.rs`.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use fleetdeck_proto::fleet_git::code;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, mkdirat, open, statat};
use rustix::io::Errno;

use super::identity::Refusal;

/// The folder of the temporary worktrees, inside an allowed root.
pub const TEMP_DIR: &str = ".fleetdeck-land";

/// The canonical allowed root that holds `home`: the longest one. `None` when
/// no root holds it (design §11.6, location).
#[must_use]
pub fn temp_root(roots: &[PathBuf], home: &Path) -> Option<PathBuf> {
    roots
        .iter()
        .filter(|root| home.starts_with(root))
        .max_by_key(|root| root.as_os_str().len())
        .cloned()
}

/// `<root>/.fleetdeck-land/<first 16 hex characters of SHA-256(land_id)>-<attempt>`.
/// A land id is `land-<ULID>`: a prefix of it holds the time and no
/// randomness, so the name uses a digest of the whole id (H16).
#[must_use]
pub fn temp_path(root: &Path, land_id: &str, attempt: u32) -> PathBuf {
    use std::fmt::Write as _;

    let digest = ring::digest::digest(&ring::digest::SHA256, land_id.as_bytes());
    let mut short = String::with_capacity(16);
    for byte in digest.as_ref().iter().take(8) {
        // Writing to a `String` cannot fail.
        let _ = write!(short, "{byte:02x}");
    }
    root.join(TEMP_DIR).join(format!("{short}-{attempt}"))
}

fn unsafe_dir(root: &Path) -> Refusal {
    Refusal::new(
        code::LAND_TEMP_UNSAFE,
        format!(
            "The folder {}/{TEMP_DIR} is not a private folder of the station user.",
            root.display()
        ),
    )
}

/// Makes `<root>/.fleetdeck-land` (mode 0700, no link followed) or checks the
/// existing one: a real folder of the station user with mode 0700. It runs
/// blocking calls: call it from a blocking context.
///
/// # Errors
/// `land_temp_unsafe`.
pub fn ensure_dir(root: &Path) -> Result<(), Refusal> {
    let fd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| unsafe_dir(root))?;
    match mkdirat(&fd, TEMP_DIR, Mode::from_bits_truncate(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err(unsafe_dir(root)),
    }
    let st = statat(&fd, TEMP_DIR, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| unsafe_dir(root))?;
    let is_dir = FileType::from_raw_mode(st.st_mode) == FileType::Directory;
    let private = st.st_mode & 0o777 == 0o700;
    let mine = st.st_uid == rustix::process::geteuid().as_raw();
    if is_dir && private && mine {
        Ok(())
    } else {
        Err(unsafe_dir(root))
    }
}

/// The device and inode of `path`, without following a link.
#[must_use]
pub fn identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

#[cfg(test)]
#[path = "land_temp_tests.rs"]
mod tests;
