//! Deleting a browser profile folder (R13 §5B item 8): only a plain folder
//! below a parent that is itself not a link, and no link is followed. After
//! the parent is open, no path is resolved again: the check and the whole
//! removal work relative to descriptors, so a swap of the parent cannot
//! redirect the removal.

use std::ffi::{OsStr, OsString};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};

/// The deepest folder level that the removal follows down.
const MAX_DEPTH: u32 = 64;

/// Why a profile folder was not removed.
#[derive(Debug)]
pub enum ProfileError {
    /// The parent or the folder is a link or not a folder.
    NotPlain,
    Io(std::io::Error),
}

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn plain_name(name: &str) -> Result<(), ProfileError> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        Err(ProfileError::NotPlain)
    } else {
        Ok(())
    }
}

/// Opens `parent` without following a link. `Ok(None)` when it is missing.
fn open_parent(parent: &Path) -> Result<Option<OwnedFd>, ProfileError> {
    match rustix::fs::openat(CWD, parent, DIR_FLAGS, Mode::empty()) {
        Ok(fd) => Ok(Some(fd)),
        Err(e) if e == rustix::io::Errno::NOENT => Ok(None),
        Err(e) if e == rustix::io::Errno::LOOP || e == rustix::io::Errno::NOTDIR => {
            Err(ProfileError::NotPlain)
        }
        Err(e) => Err(ProfileError::Io(e.into())),
    }
}

/// Removes `parent/name` with everything inside. A missing folder is fine.
///
/// `parent` is opened with `O_NOFOLLOW`; the rest works on that descriptor.
/// `name` is one plain file name.
///
/// # Errors
/// [`ProfileError::NotPlain`] when the parent or the entry is a link or not
/// a folder; [`ProfileError::Io`] for other failures.
pub fn remove(parent: &Path, name: &str) -> Result<(), ProfileError> {
    plain_name(name)?;
    match open_parent(parent)? {
        Some(dir) => remove_in(&dir, name),
        None => Ok(()),
    }
}

/// Removes `name` below the already open `parent`: the entry is checked with
/// `AT_SYMLINK_NOFOLLOW`, then removed relative to descriptors.
///
/// # Errors
/// As [`remove`].
pub fn remove_in(parent: &OwnedFd, name: &str) -> Result<(), ProfileError> {
    plain_name(name)?;
    let stat = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(e) if e == rustix::io::Errno::NOENT => return Ok(()),
        Err(e) => return Err(ProfileError::Io(e.into())),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(ProfileError::NotPlain);
    }
    remove_tree_at(parent, OsStr::new(name), 0).map_err(ProfileError::Io)
}

/// Removes the folder `name` of `parent`: its entries first (a folder by
/// recursion, anything else, a link included, with `unlinkat`), then the
/// folder. No link is followed and no path is resolved again.
fn remove_tree_at(parent: &OwnedFd, name: &OsStr, depth: u32) -> std::io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(std::io::Error::other("the profile folder is too deep"));
    }
    let child = rustix::fs::openat(parent, name, DIR_FLAGS, Mode::empty())?;
    // Read every name first: the folder changes while it is emptied.
    let mut entries: Vec<(OsString, bool)> = Vec::new();
    for entry in Dir::read_from(&child)? {
        let entry = entry?;
        let raw = entry.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        let entry_name = OsStr::from_bytes(raw).to_owned();
        let is_dir = match entry.file_type() {
            FileType::Directory => true,
            FileType::Unknown => {
                let stat = rustix::fs::statat(&child, &entry_name, AtFlags::SYMLINK_NOFOLLOW)?;
                FileType::from_raw_mode(stat.st_mode) == FileType::Directory
            }
            _ => false,
        };
        entries.push((entry_name, is_dir));
    }
    for (entry_name, is_dir) in entries {
        if is_dir {
            remove_tree_at(&child, &entry_name, depth + 1)?;
        } else {
            rustix::fs::unlinkat(&child, &entry_name, AtFlags::empty())?;
        }
    }
    rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_folder_is_removed_and_a_missing_one_is_fine() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("wall-a");
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        std::fs::write(profile.join("Default/Cookies"), b"x").unwrap();
        remove(tmp.path(), "wall-a").unwrap();
        assert!(!profile.exists());
        remove(tmp.path(), "wall-a").unwrap();
        remove(&tmp.path().join("missing"), "wall-a").unwrap();
    }

    #[test]
    fn a_link_in_place_of_the_folder_is_refused_and_its_target_stays() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("precious");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"x").unwrap();
        let parent = tmp.path().join("fleetdeck");
        std::fs::create_dir(&parent).unwrap();
        std::os::unix::fs::symlink(&target, parent.join("wall-a")).unwrap();
        assert!(matches!(
            remove(&parent, "wall-a"),
            Err(ProfileError::NotPlain)
        ));
        assert!(target.join("keep").exists());
        assert!(parent.join("wall-a").is_symlink());
    }

    #[test]
    fn a_link_in_place_of_the_parent_and_a_file_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("wall-a")).unwrap();
        let link = tmp.path().join("fleetdeck");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(
            remove(&link, "wall-a"),
            Err(ProfileError::NotPlain)
        ));
        assert!(real.join("wall-a").exists());
        std::fs::write(real.join("file"), b"x").unwrap();
        assert!(matches!(remove(&real, "file"), Err(ProfileError::NotPlain)));
        assert!(matches!(remove(&real, "../x"), Err(ProfileError::NotPlain)));
    }

    #[test]
    fn a_link_inside_the_folder_is_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("precious");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"x").unwrap();
        let profile = tmp.path().join("wall-a");
        std::fs::create_dir(&profile).unwrap();
        std::os::unix::fs::symlink(&target, profile.join("inner")).unwrap();
        remove(tmp.path(), "wall-a").unwrap();
        assert!(!profile.exists());
        assert!(target.join("keep").exists());
    }

    #[test]
    fn a_parent_swapped_after_the_check_cannot_redirect_the_removal() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("fleetdeck");
        std::fs::create_dir_all(parent.join("wall-a/Default")).unwrap();
        std::fs::write(parent.join("wall-a/Default/Cookies"), b"x").unwrap();
        // A folder outside, with a folder of the same name.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(outside.join("wall-a")).unwrap();
        std::fs::write(outside.join("wall-a/keep"), b"x").unwrap();
        let held = open_parent(&parent).unwrap().unwrap();
        // The swap: the parent moves away, a link takes its place.
        let moved = tmp.path().join("moved");
        std::fs::rename(&parent, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &parent).unwrap();
        remove_in(&held, "wall-a").unwrap();
        assert!(outside.join("wall-a/keep").exists());
        assert!(!moved.join("wall-a").exists(), "the original is gone");
    }

    #[test]
    fn a_very_deep_folder_is_refused_not_followed_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().join("wall-a");
        for _ in 0..70 {
            deep = deep.join("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        assert!(matches!(
            remove(tmp.path(), "wall-a"),
            Err(ProfileError::Io(_))
        ));
    }
}
