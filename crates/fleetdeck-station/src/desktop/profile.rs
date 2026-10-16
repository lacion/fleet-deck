//! Deleting a browser profile folder (R13 §5B item 8): only a plain folder
//! below a parent that is itself not a link, and no link is followed.

use std::path::Path;

use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags};

/// Why a profile folder was not removed.
#[derive(Debug)]
pub enum ProfileError {
    /// The parent or the folder is a link or not a folder.
    NotPlain,
    Io(std::io::Error),
}

/// Removes `parent/name` with everything inside. A missing folder is fine.
///
/// `parent` is opened with `O_NOFOLLOW`, and the entry is checked with
/// `AT_SYMLINK_NOFOLLOW` before the removal. `name` is one plain file name.
///
/// # Errors
/// [`ProfileError::NotPlain`] when the parent or the entry is a link or not
/// a folder; [`ProfileError::Io`] for other failures.
pub fn remove(parent: &Path, name: &str) -> Result<(), ProfileError> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(ProfileError::NotPlain);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let dir = match rustix::fs::openat(CWD, parent, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(e) if e == rustix::io::Errno::NOENT => return Ok(()),
        Err(e) if e == rustix::io::Errno::LOOP || e == rustix::io::Errno::NOTDIR => {
            return Err(ProfileError::NotPlain);
        }
        Err(e) => return Err(ProfileError::Io(e.into())),
    };
    let stat = match rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(e) if e == rustix::io::Errno::NOENT => return Ok(()),
        Err(e) => return Err(ProfileError::Io(e.into())),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(ProfileError::NotPlain);
    }
    // `remove_dir_all` does not follow a link inside the folder.
    std::fs::remove_dir_all(parent.join(name)).map_err(ProfileError::Io)
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
}
