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

/// The canonical text of a path (a temp dir can be a link, like `/tmp` on
/// macOS).
fn canon(path: impl AsRef<Path>) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// True when the temp dir and its parents are safe for the user. A test that
/// expects a kept directory returns early when this is false.
fn tmp_is_safe(tmp: &Path) -> bool {
    let uid = std::fs::metadata(tmp).unwrap().uid();
    safe_path(&tmp.to_string_lossy(), uid).0.is_some()
}

#[test]
fn safe_path_keeps_a_private_directory_of_the_user_and_root() {
    let tmp = tempfile::tempdir().unwrap();
    // The parents of the temp dir are not safe on this machine.
    if !tmp_is_safe(tmp.path()) {
        return;
    }
    let mine = dir(tmp.path(), "mine", 0o755);
    let uid = my_uid(&mine);
    let (path, notes) = safe_path(&format!("{mine}:/usr/bin"), uid);
    assert_eq!(
        path,
        Some(format!("{}:{}", canon(&mine), canon("/usr/bin")))
    );
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn safe_path_drops_a_directory_that_others_can_write() {
    let tmp = tempfile::tempdir().unwrap();
    let open = dir(tmp.path(), "open", 0o777);
    let group = dir(tmp.path(), "group", 0o775);
    let uid = my_uid(&open);
    let (path, notes) = safe_path(&format!("{open}:{group}:/usr/bin"), uid);
    assert_eq!(path, Some(canon("/usr/bin")));
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
    assert_eq!(path, Some(canon("/usr/bin")));
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
    // The parents of the temp dir are not safe on this machine.
    if !tmp_is_safe(tmp.path()) {
        return;
    }
    let a = dir(tmp.path(), "a", 0o755);
    let b = dir(tmp.path(), "b", 0o700);
    let uid = my_uid(&a);
    let (path, notes) = safe_path(&format!(":{b}::/usr/bin:{a}:"), uid);
    assert_eq!(
        path,
        Some(format!("{}:{}:{}", canon(&b), canon("/usr/bin"), canon(&a)))
    );
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn safe_path_keeps_the_canonical_directory_of_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    // The parents of the temp dir are not safe on this machine.
    if !tmp_is_safe(tmp.path()) {
        return;
    }
    let safe = dir(tmp.path(), "safe", 0o755);
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&safe, &link).unwrap();
    let uid = my_uid(&safe);
    let (path, notes) = safe_path(&link.to_string_lossy(), uid);
    assert_eq!(path, Some(canon(&safe)));
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn safe_path_drops_a_link_whose_target_holds_a_colon() {
    let tmp = tempfile::tempdir().unwrap();
    // The parents of the temp dir are not safe on this machine.
    if !tmp_is_safe(tmp.path()) {
        return;
    }
    let target = dir(tmp.path(), "safe:evil", 0o755);
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let uid = my_uid(&target);
    let (path, notes) = safe_path(&link.to_string_lossy(), uid);
    assert_eq!(path, None);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].ends_with("its directory name holds a `:`"),
        "{notes:?}"
    );
}

#[test]
fn safe_path_drops_a_directory_with_a_parent_that_others_can_write() {
    let tmp = tempfile::tempdir().unwrap();
    // The parents of the temp dir are not safe on this machine.
    if !tmp_is_safe(tmp.path()) {
        return;
    }
    let open = dir(tmp.path(), "open", 0o777);
    let bin = dir(Path::new(&open), "bin", 0o755);
    let uid = my_uid(&bin);
    let (path, notes) = safe_path(&bin, uid);
    assert_eq!(path, None);
    assert_eq!(
        notes,
        [format!(
            "{bin}: a parent directory is not safe: {}",
            canon(&open)
        )]
    );
}

#[test]
fn safe_path_keeps_a_directory_once_when_two_entries_have_the_same_target() {
    // Only on a machine where `/bin` links to `/usr/bin`.
    if std::fs::canonicalize("/bin").ok() != std::fs::canonicalize("/usr/bin").ok() {
        return;
    }
    let (path, notes) = safe_path("/usr/bin:/bin", 0);
    assert_eq!(path, Some(canon("/usr/bin")));
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
