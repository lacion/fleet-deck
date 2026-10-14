//! The safe `PATH` of the service (NV-22).

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use super::Rig;
use crate::service::{Kind, safe_path};

fn dir(parent: &Path, name: &str, mode: u32) -> String {
    let path = parent.join(name);
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path.to_string_lossy().into_owned()
}

fn my_uid(path: &str) -> u32 {
    std::fs::metadata(path).unwrap().uid()
}

#[test]
fn safe_path_keeps_a_private_directory_of_the_user_and_root() {
    let tmp = tempfile::tempdir().unwrap();
    let mine = dir(tmp.path(), "mine", 0o755);
    let uid = my_uid(&mine);
    let (path, notes) = safe_path(&format!("{mine}:/usr/bin"), uid);
    assert_eq!(path, Some(format!("{mine}:/usr/bin")));
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn safe_path_drops_a_directory_that_others_can_write() {
    let tmp = tempfile::tempdir().unwrap();
    let open = dir(tmp.path(), "open", 0o777);
    let group = dir(tmp.path(), "group", 0o775);
    let uid = my_uid(&open);
    let (path, notes) = safe_path(&format!("{open}:{group}:/usr/bin"), uid);
    assert_eq!(path.as_deref(), Some("/usr/bin"));
    assert_eq!(
        notes,
        [
            format!("{open}: the group or others can write it"),
            format!("{group}: the group or others can write it"),
        ]
    );
}

#[test]
fn safe_path_drops_a_relative_entry_and_a_missing_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing").to_string_lossy().into_owned();
    let file = tmp.path().join("file");
    std::fs::write(&file, "x").unwrap();
    let file = file.to_string_lossy().into_owned();
    let (path, notes) = safe_path(&format!("bin:.:{missing}:{file}:/usr/bin"), 0);
    assert_eq!(path.as_deref(), Some("/usr/bin"));
    assert_eq!(
        notes,
        [
            "bin: not an absolute path".to_owned(),
            ".: not an absolute path".to_owned(),
            format!("{missing}: not a directory"),
            format!("{file}: not a directory"),
        ]
    );
}

#[test]
fn safe_path_drops_a_directory_of_another_user() {
    let tmp = tempfile::tempdir().unwrap();
    let mine = dir(tmp.path(), "mine", 0o755);
    // A uid that is neither the owner nor root.
    let other = my_uid(&mine) + 1;
    let (path, notes) = safe_path(&mine, other);
    assert_eq!(path, None);
    assert_eq!(notes, [format!("{mine}: another user owns it")]);
}

#[test]
fn safe_path_drops_empty_entries_without_a_note_and_keeps_the_order() {
    let tmp = tempfile::tempdir().unwrap();
    let a = dir(tmp.path(), "a", 0o755);
    let b = dir(tmp.path(), "b", 0o700);
    let uid = my_uid(&a);
    let (path, notes) = safe_path(&format!(":{b}::/usr/bin:{a}:"), uid);
    assert_eq!(path, Some(format!("{b}:/usr/bin:{a}")));
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn safe_path_gives_none_when_every_entry_is_dropped() {
    let (path, notes) = safe_path("rel:/definitely/missing/dir", 0);
    assert_eq!(path, None);
    assert_eq!(notes.len(), 2);
}

#[test]
fn install_prints_one_warning_for_each_dropped_path_entry() {
    let mut rig = Rig::systemd();
    rig.ctx.path_dropped = vec![
        "/tmp/x: the group or others can write it".to_owned(),
        "bin: not an absolute path".to_owned(),
    ];
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert!(
        out.starts_with(
            "warning: PATH entry /tmp/x: the group or others can write it; the service leaves it out\n\
             warning: PATH entry bin: not an absolute path; the service leaves it out\n"
        ),
        "{out}"
    );
}
