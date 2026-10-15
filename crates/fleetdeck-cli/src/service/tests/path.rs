//! The safe `PATH` of the service (NV-22).

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use super::Rig;
use crate::cli::ServiceCommand;
use crate::service::Kind;
use crate::service::path::{local_accounts_only, lost_programs, private_gid, safe_path};

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
    safe_path(&tmp.to_string_lossy(), uid, None).kept.is_some()
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
    let check = safe_path(&format!("{mine}:/usr/bin"), uid, None);
    assert_eq!(
        check.kept,
        Some(format!("{}:{}", canon(&mine), canon("/usr/bin")))
    );
    assert!(check.dropped.is_empty(), "{:?}", check.dropped);
}

#[test]
fn safe_path_drops_a_directory_that_others_can_write() {
    let tmp = tempfile::tempdir().unwrap();
    let open = dir(tmp.path(), "open", 0o777);
    let group = dir(tmp.path(), "group", 0o775);
    let uid = my_uid(&open);
    let check = safe_path(&format!("{open}:{group}:/usr/bin"), uid, None);
    assert_eq!(check.kept, Some(canon("/usr/bin")));
    assert_eq!(
        check.dropped,
        [
            (open, "the group or others can write it".to_owned()),
            (group, "the group or others can write it".to_owned()),
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
    let check = safe_path(&format!("bin:.:{missing}:{file}:/usr/bin"), 0, None);
    assert_eq!(check.kept, Some(canon("/usr/bin")));
    assert_eq!(
        check.dropped,
        [
            ("bin".to_owned(), "not an absolute path".to_owned()),
            (".".to_owned(), "not an absolute path".to_owned()),
            (missing, "not a directory".to_owned()),
            (file, "not a directory".to_owned()),
        ]
    );
}

#[test]
fn safe_path_drops_a_directory_of_another_user() {
    let tmp = tempfile::tempdir().unwrap();
    let mine = dir(tmp.path(), "mine", 0o755);
    // A uid that is neither the owner nor root.
    let other = my_uid(&mine) + 1;
    let check = safe_path(&mine, other, None);
    assert_eq!(check.kept, None);
    assert_eq!(check.dropped, [(mine, "another user owns it".to_owned())]);
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
    let check = safe_path(&format!(":{b}::/usr/bin:{a}:"), uid, None);
    assert_eq!(
        check.kept,
        Some(format!("{}:{}:{}", canon(&b), canon("/usr/bin"), canon(&a)))
    );
    assert!(check.dropped.is_empty(), "{:?}", check.dropped);
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
    let check = safe_path(&link.to_string_lossy(), uid, None);
    assert_eq!(check.kept, Some(canon(&safe)));
    assert!(check.dropped.is_empty(), "{:?}", check.dropped);
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
    let check = safe_path(&link.to_string_lossy(), uid, None);
    assert_eq!(check.kept, None);
    assert_eq!(check.dropped.len(), 1, "{:?}", check.dropped);
    assert_eq!(check.dropped[0].1, "its directory name holds a `:`");
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
    let check = safe_path(&bin, uid, None);
    assert_eq!(check.kept, None);
    let reason = format!("a parent directory is not safe: {}", canon(&open));
    assert_eq!(check.dropped, [(bin, reason)]);
}

#[test]
fn safe_path_keeps_a_directory_once_when_two_entries_have_the_same_target() {
    // Only on a machine where `/bin` links to `/usr/bin`.
    if std::fs::canonicalize("/bin").ok() != std::fs::canonicalize("/usr/bin").ok() {
        return;
    }
    let check = safe_path("/usr/bin:/bin", 0, None);
    assert_eq!(check.kept, Some(canon("/usr/bin")));
    assert!(check.dropped.is_empty(), "{:?}", check.dropped);
}

#[test]
fn safe_path_gives_none_when_every_entry_is_dropped() {
    let check = safe_path("rel:/definitely/missing/dir", 0, None);
    assert_eq!(check.kept, None);
    assert_eq!(check.dropped.len(), 2);
}

fn gid_of(path: &str) -> u32 {
    std::fs::metadata(path).unwrap().gid()
}

#[test]
fn safe_path_keeps_a_group_writable_directory_of_the_private_group() {
    let tmp = tempfile::tempdir().unwrap();
    // The parents of the temp dir are not safe on this machine.
    if !tmp_is_safe(tmp.path()) {
        return;
    }
    let shared = dir(tmp.path(), "shared", 0o775);
    let (uid, gid) = (my_uid(&shared), gid_of(&shared));
    let check = safe_path(&shared, uid, Some(gid));
    assert_eq!(check.kept, Some(canon(&shared)));
    assert!(check.dropped.is_empty(), "{:?}", check.dropped);
    let check = safe_path(&shared, uid, None);
    assert_eq!(check.kept, None);
    assert_eq!(
        check.dropped,
        [(shared, "the group or others can write it".to_owned())]
    );
}

#[test]
fn safe_path_drops_a_directory_that_others_can_write_in_the_private_group() {
    let tmp = tempfile::tempdir().unwrap();
    let open = dir(tmp.path(), "open", 0o777);
    let (uid, gid) = (my_uid(&open), gid_of(&open));
    let check = safe_path(&open, uid, Some(gid));
    assert_eq!(check.kept, None);
    assert_eq!(
        check.dropped,
        [(open, "the group or others can write it".to_owned())]
    );
}

#[test]
fn safe_path_gives_a_dropped_entry_once() {
    let check = safe_path("/usr/bin:/nope:/nope", 0, None);
    assert_eq!(check.kept, Some(canon("/usr/bin")));
    assert_eq!(
        check.dropped,
        [("/nope".to_owned(), "not a directory".to_owned())]
    );
}

const GROUP: &str = "root:x:0:\nluis:x:1000:\nteam:x:1001:luis,ann\n";
const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\nluis:x:1000:1000::/home/luis:/bin/sh\n\
                      ann:x:1002:1001::/home/ann:/bin/sh\n";

#[test]
fn private_gid_accepts_a_group_with_no_member() {
    assert_eq!(private_gid(GROUP, PASSWD, "luis", 1000), Some(1000));
}

#[test]
fn private_gid_accepts_a_group_with_only_the_user() {
    let group = "luis:x:1000:luis\n";
    assert_eq!(private_gid(group, PASSWD, "luis", 1000), Some(1000));
}

#[test]
fn private_gid_refuses_a_group_with_another_member() {
    let group = "luis:x:1000:ann\n";
    assert_eq!(private_gid(group, PASSWD, "luis", 1000), None);
    assert_eq!(private_gid(GROUP, PASSWD, "luis", 1001), None);
}

#[test]
fn private_gid_refuses_a_group_that_is_the_primary_group_of_another_user() {
    let passwd = format!("{PASSWD}bob:x:1003:1000::/home/bob:/bin/sh\n");
    assert_eq!(private_gid(GROUP, &passwd, "luis", 1000), None);
}

#[test]
fn private_gid_refuses_a_gid_that_is_not_in_the_group_file() {
    assert_eq!(private_gid(GROUP, PASSWD, "luis", 4242), None);
}

/// A folder with an executable `gh` (or one with the given mode).
fn program(parent: &Path, name: &str, mode: u32) -> String {
    let folder = dir(parent, name, 0o755);
    let file = Path::new(&folder).join("gh");
    std::fs::write(&file, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
    folder
}

#[test]
fn lost_programs_names_a_program_that_only_a_dropped_folder_has() {
    let tmp = tempfile::tempdir().unwrap();
    let kept = dir(tmp.path(), "kept", 0o755);
    let dropped = program(tmp.path(), "dropped", 0o755);
    let shell = format!("{kept}:{dropped}");
    assert_eq!(
        lost_programs(&shell, Some(&kept)),
        [("gh".to_owned(), dropped)]
    );
    assert_eq!(lost_programs(&shell, None).len(), 1);
}

#[test]
fn lost_programs_is_empty_when_the_kept_path_has_the_program() {
    let tmp = tempfile::tempdir().unwrap();
    let kept = program(tmp.path(), "kept", 0o755);
    let dropped = program(tmp.path(), "dropped", 0o755);
    let shell = format!("{dropped}:{kept}");
    assert!(lost_programs(&shell, Some(&kept)).is_empty());
}

#[test]
fn lost_programs_ignores_a_file_without_an_execute_bit() {
    let tmp = tempfile::tempdir().unwrap();
    let dropped = program(tmp.path(), "dropped", 0o644);
    assert!(lost_programs(&dropped, None).is_empty());
}

fn three_dropped(rig: &mut Rig) {
    rig.ctx.path_dropped = vec![
        (
            "/tmp/x".to_owned(),
            "the group or others can write it".to_owned(),
        ),
        ("bin".to_owned(), "not an absolute path".to_owned()),
        ("/nope".to_owned(), "not a directory".to_owned()),
    ];
    rig.sockets(Kind::Both, &[false, false, true]);
}

fn install_with(rig: &Rig, verbose: bool) -> String {
    let (result, out) = rig.run(&ServiceCommand::Install {
        kind: Kind::Both,
        dry_run: false,
        verbose,
    });
    result.unwrap();
    out
}

#[test]
fn install_prints_one_path_line_for_the_dropped_entries() {
    let mut rig = Rig::systemd();
    three_dropped(&mut rig);
    let out = install_with(&rig, false);
    let line = "PATH: the service keeps 2 folders and leaves out 3 that other users can change \
                or that do not exist. Run `fleetdeck service install --verbose` to list them.\n";
    assert!(out.starts_with(line), "{out}");
    assert_eq!(out.lines().filter(|l| l.starts_with("PATH:")).count(), 1);
    assert!(!out.contains("warning: PATH entry"), "{out}");
    assert!(!out.contains("/nope"), "{out}");
}

#[test]
fn install_says_folder_for_one_kept_entry() {
    let mut rig = Rig::systemd();
    three_dropped(&mut rig);
    rig.ctx.path = Some("/usr/bin".to_owned());
    let out = install_with(&rig, false);
    assert!(
        out.starts_with("PATH: the service keeps 1 folder and leaves"),
        "{out}"
    );
}

#[test]
fn install_with_verbose_lists_the_dropped_entries() {
    let mut rig = Rig::systemd();
    three_dropped(&mut rig);
    let out = install_with(&rig, true);
    assert!(
        out.contains(
            "to list them.\n  /tmp/x: the group or others can write it\n  bin: not an \
             absolute path\n  /nope: not a directory\nInstalled the both service"
        ),
        "{out}"
    );
}

#[test]
fn a_dry_run_prints_the_same_path_line() {
    let mut rig = Rig::systemd();
    three_dropped(&mut rig);
    let (result, out) = rig.dry_run(Kind::Both);
    result.unwrap();
    assert!(
        out.starts_with("PATH: the service keeps 2 folders and leaves out 3"),
        "{out}"
    );
}

#[test]
fn install_prints_nothing_about_the_path_when_nothing_is_dropped() {
    let rig = Rig::systemd();
    rig.sockets(Kind::Both, &[false, false, true]);
    let out = install_with(&rig, true);
    assert!(out.starts_with("Installed the both service"), "{out}");
}

#[test]
fn install_warns_when_a_program_is_only_in_a_dropped_folder() {
    let mut rig = Rig::systemd();
    three_dropped(&mut rig);
    let dropped = program(rig.tmp.path(), "tools", 0o755);
    let kept = dir(rig.tmp.path(), "kept", 0o755);
    rig.ctx.path = Some(kept);
    rig.ctx.shell_path = Some(dropped.clone());
    rig.ctx.path_dropped.push((
        dropped.clone(),
        "the group or others can write it".to_owned(),
    ));
    let out = install_with(&rig, false);
    let line = format!(
        "warning: the service cannot run `gh`: it is in {dropped}, which the service leaves out \
         (the group or others can write it).\nPATH: "
    );
    assert!(out.starts_with(&line), "{out}");
}

#[test]
fn install_warns_with_a_default_reason_for_a_folder_that_is_not_listed() {
    let mut rig = Rig::systemd();
    rig.sockets(Kind::Both, &[false, false, true]);
    let tools = program(rig.tmp.path(), "tools", 0o755);
    let kept = dir(rig.tmp.path(), "kept", 0o755);
    rig.ctx.path = Some(kept);
    rig.ctx.shell_path = Some(tools.clone());
    let out = install_with(&rig, false);
    let line = format!(
        "warning: the service cannot run `gh`: it is in {tools}, which the service leaves out \
         (not on the service PATH).\nInstalled the both service"
    );
    assert!(out.starts_with(&line), "{out}");
}

#[test]
fn install_ends_with_the_single_install_again_line() {
    let rig = Rig::systemd();
    rig.sockets(Kind::Both, &[false, false, true]);
    let out = install_with(&rig, false);
    assert!(
        out.ends_with(
            "Run `fleetdeck service install` again after you install a new Fleet Deck version \
             or a provider CLI in a new folder.\n"
        ),
        "{out}"
    );
    assert!(!out.contains("uses the PATH of this shell"), "{out}");
}

#[test]
fn the_private_group_rule_needs_local_account_sources_only() {
    let local = "passwd: files systemd\ngroup: files systemd\nhosts: files dns\n";
    assert!(local_accounts_only(local));
    assert!(local_accounts_only(
        "passwd: files\ngroup: files [SUCCESS=merge] systemd\n"
    ));
    // Another source can hold a user that shares the group.
    assert!(!local_accounts_only(
        "passwd: files sss\ngroup: files sss\n"
    ));
    assert!(!local_accounts_only("passwd: files\ngroup: files ldap\n"));
    assert!(!local_accounts_only("passwd: compat\ngroup: compat\n"));
    // A missing line is not proof.
    assert!(!local_accounts_only("passwd: files\n"));
    assert!(!local_accounts_only("# group: files\npasswd: files\n"));
}
