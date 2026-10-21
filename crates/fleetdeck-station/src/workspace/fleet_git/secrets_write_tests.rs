//! The write part of the secret files (Fleet Sync FS-3): the replace without a
//! lost edit, the backups and the names of the staging files.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt, symlink};

use fleetdeck_proto::member_deny::MemberDeny;
use serde_json::json;

use super::tests::{Rig, SALT, code_of, mode_of};
use super::write::{backup_folder, backup_key};
use super::*;

#[tokio::test]
async fn a_write_makes_a_new_file_and_its_folders() {
    let Some(rig) = Rig::new().await else { return };
    let backup = rig.put("deep/er/.env", b"A=1\n", None).await.unwrap();
    assert!(!backup);
    let file = rig.home.join("deep/er/.env");
    assert_eq!(fs::read(&file).unwrap(), b"A=1\n");
    assert_eq!(mode_of(&file), 0o600);
    assert_eq!(mode_of(&rig.home.join("deep")), 0o700);
    assert_eq!(mode_of(&rig.home.join("deep/er")), 0o700);
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
    assert!(rig.backups("deep/er/.env").is_empty());
}

#[tokio::test]
async fn a_write_replaces_with_a_backup() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"OLD=1\n");
    fs::set_permissions(rig.home.join(".env"), fs::Permissions::from_mode(0o644)).unwrap();
    let backup = rig
        .put(".env", b"NEW=2\n", Some(&Rig::digest(b"OLD=1\n")))
        .await
        .unwrap();
    assert!(backup);
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"NEW=2\n");
    assert_eq!(mode_of(&rig.home.join(".env")), 0o600);
    let kept = rig.backups(".env");
    assert_eq!(kept.len(), 1);
    assert_eq!(fs::read(&kept[0]).unwrap(), b"OLD=1\n");
    assert_eq!(mode_of(&kept[0]), 0o600);
    assert_eq!(mode_of(kept[0].parent().unwrap()), 0o700);
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
    // The backup is in the data folder, never in the repository.
    assert!(kept[0].starts_with(&rig.data));
}

#[tokio::test]
async fn a_write_refuses_a_changed_file() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"OLD=1\n");
    // A wrong `expect`.
    let e = rig
        .put(".env", b"NEW=2\n", Some(&Rig::digest(b"OTHER\n")))
        .await;
    assert_eq!(code_of(e), "changed_meanwhile");
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"OLD=1\n");
    // `None` with an existing file.
    assert_eq!(
        code_of(rig.put(".env", b"NEW=2\n", None).await),
        "changed_meanwhile"
    );
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"OLD=1\n");
    // `Some` with a missing file.
    let e = rig
        .put(".env.local", b"NEW=2\n", Some(&Rig::digest(b"OLD=1\n")))
        .await;
    assert_eq!(code_of(e), "changed_meanwhile");
    assert!(!rig.home.join(".env.local").exists());
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
    assert!(rig.backups(".env").is_empty());
}

#[tokio::test]
async fn a_write_refuses_a_content_that_does_not_match_its_digest() {
    let Some(rig) = Rig::new().await else { return };
    let mut params = rig.write_params(".env", b"A=1\n", None);
    params["content_b64"] = json!(STANDARD.encode(b"A=2\n"));
    let e = rig.call("project.secret_write", params).await.unwrap_err();
    assert_eq!(e.code, "digest_mismatch");
    assert_eq!(e.text, DIGEST_MISMATCH.text);
    assert!(!rig.home.join(".env").exists());
    // Bad base64 and a too large content.
    let mut params = rig.write_params(".env", b"A=1\n", None);
    params["content_b64"] = json!("***");
    let e = rig.call("project.secret_write", params).await;
    assert_eq!(code_of(e), "invalid_params");
    let big = vec![b'a'; 512 * 1024 + 1];
    let e = rig
        .call("project.secret_write", rig.write_params(".env", &big, None))
        .await;
    assert_eq!(code_of(e), "too_large");
    assert!(!rig.home.join(".env").exists());
}

#[tokio::test]
async fn seven_writes_keep_five_backups() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"V=0\n");
    for n in 0..7 {
        let old = format!("V={n}\n");
        let new = format!("V={}\n", n + 1);
        let made = rig
            .put(".env", new.as_bytes(), Some(&Rig::digest(old.as_bytes())))
            .await
            .unwrap();
        assert!(made);
    }
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"V=7\n");
    let mut kept: Vec<String> = rig
        .backups(".env")
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    kept.sort();
    // The backups hold the 5 newest old contents: V=2 to V=6.
    assert_eq!(kept, ["V=2\n", "V=3\n", "V=4\n", "V=5\n", "V=6\n"]);
}

#[tokio::test]
async fn a_failed_backup_writes_nothing_and_leaves_no_temp_file() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"OLD=1\n");
    let params = rig.write_params(".env", b"NEW=2\n", Some(&Rig::digest(b"OLD=1\n")));
    // No data folder: the station cannot keep a copy.
    let e = rig
        .call_with("project.secret_write", params, None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "backup_failed");
    assert_eq!(e.text, BACKUP_FAILED.text);
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"OLD=1\n");
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
}

/// A `commit` with `expect` in a plain folder, where an editor makes a new
/// `.env` right after the old one moved aside. Returns the error code, the
/// content of `.env` and the names that the station left in the folder.
fn window(expect: &[u8], data: bool) -> (&'static str, Vec<u8>, Vec<String>, Vec<Vec<u8>>) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let data_dir = home.join("data");
    fs::write(home.join(".env"), b"OLD=1\n").unwrap();
    let root = open_root(&home).unwrap();
    let private = PrivatePaths::default();
    let job = Job {
        parent: root.as_fd(),
        name: ".env",
        rel: ".env",
        salt: SALT,
        project_id: "p1",
        private: &private,
        data_dir: data.then_some(data_dir.as_path()),
    };
    let e = commit(
        &job,
        b"NEW=2\n",
        Some(&Rig::digest(expect)),
        || fs::write(home.join(".env"), b"EDITED=3\n").unwrap(),
        || {},
    )
    .unwrap_err();
    let names = fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".fleetdeck-"))
        .collect();
    let folder = backup_folder(&data_dir, "p1", ".env").unwrap();
    let backups = fs::read_dir(folder)
        .map(|d| d.map(|e| fs::read(e.unwrap().path()).unwrap()).collect())
        .unwrap_or_default();
    (e.code, fs::read(home.join(".env")).unwrap(), names, backups)
}

#[test]
fn an_edit_in_the_window_of_a_replace_is_kept() {
    // The old file is the expected one: its content is in the backup.
    let (code, now, names, backups) = window(b"OLD=1\n", true);
    assert_eq!(
        (code, now.as_slice()),
        ("changed_meanwhile", &b"EDITED=3\n"[..])
    );
    assert_eq!(backups, [b"OLD=1\n".to_vec()]);
    assert!(names.is_empty(), "{names:?}");
    // The old file is not the expected one: the same, from the other branch.
    let (code, now, names, backups) = window(b"OTHER\n", true);
    assert_eq!(
        (code, now.as_slice()),
        ("changed_meanwhile", &b"EDITED=3\n"[..])
    );
    assert_eq!(backups, [b"OLD=1\n".to_vec()]);
    assert!(names.is_empty(), "{names:?}");
}

#[test]
fn a_failed_backup_in_the_window_keeps_the_old_file_aside() {
    // R12 b: no data folder, so no backup. The old file stays aside, the temp
    // file goes, and the edit stays in place.
    let (code, now, names, backups) = window(b"OTHER\n", false);
    assert_eq!(
        (code, now.as_slice()),
        ("changed_meanwhile", &b"EDITED=3\n"[..])
    );
    assert!(backups.is_empty());
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].contains(".fleetdeck-old-"), "{names:?}");
}

#[tokio::test]
async fn a_write_does_not_follow_a_link() {
    let Some(rig) = Rig::new().await else { return };
    fs::create_dir(rig.home.join("real")).unwrap();
    symlink(rig.home.join("real"), rig.home.join("config")).unwrap();
    let e = rig.put("config/.env.local", b"A=1\n", None).await;
    assert_eq!(code_of(e), "symlink");
    assert!(!rig.home.join("real/.env.local").exists());
    // A link at the file name.
    rig.write("target.txt", b"T");
    symlink(rig.home.join("target.txt"), rig.home.join(".env")).unwrap();
    let e = rig.put(".env", b"A=1\n", None).await;
    assert_eq!(code_of(e), "changed_meanwhile");
    assert_eq!(fs::read(rig.home.join("target.txt")).unwrap(), b"T");
    // A file where a folder must be.
    rig.write("flat", b"F");
    let e = rig.put("flat/.env", b"A=1\n", None).await;
    assert_eq!(code_of(e), "symlink");
}

#[test]
fn a_backup_name_sorts_by_its_number_and_its_suffix() {
    assert_eq!(backup_key("1700000000000"), Some((1_700_000_000_000, 0)));
    assert_eq!(backup_key("1700000000000-2"), Some((1_700_000_000_000, 2)));
    for bad in ["", "-1", "1-", "a", "1-a", "1.5", "1-2-3", ".tmp"] {
        assert_eq!(backup_key(bad), None, "{bad}");
    }
    let data = Path::new("/d");
    assert!(backup_folder(data, "abc_DEF-123", ".env").is_some());
    for bad in ["", "a/b", "..", "a b", &"a".repeat(65)] {
        assert_eq!(backup_folder(data, bad, ".env"), None, "{bad}");
    }
}

/// A `commit` with `expect` on `.env` of `home`, with the data folder
/// `<home>/data`. `between` and `mid` are the hooks of the commit.
fn replace_in(
    home: &Path,
    between: impl FnOnce(),
    mid: impl FnOnce(),
) -> Result<bool, SecretError> {
    let root = open_root(home).unwrap();
    let private = PrivatePaths::default();
    let data_dir = home.join("data");
    let job = Job {
        parent: root.as_fd(),
        name: ".env",
        rel: ".env",
        salt: SALT,
        project_id: "p1",
        private: &private,
        data_dir: Some(data_dir.as_path()),
    };
    commit(
        &job,
        b"NEW=2\n",
        Some(&Rig::digest(b"OLD=1\n")),
        between,
        mid,
    )
}

/// The names with `.fleetdeck-` in `dir`.
fn staging_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".fleetdeck-"))
        .collect();
    names.sort();
    names
}

fn backup_contents(home: &Path) -> Vec<Vec<u8>> {
    let folder = backup_folder(&home.join("data"), "p1", ".env").unwrap();
    let mut found: Vec<Vec<u8>> = fs::read_dir(folder)
        .map(|d| d.map(|e| fs::read(e.unwrap().path()).unwrap()).collect())
        .unwrap_or_default();
    found.sort();
    found
}

#[test]
fn the_staging_files_are_hidden_from_members() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let folder = home.join("apps/web");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join(".env"), b"OLD=1\n").unwrap();
    // Both staging files exist when the hook runs: the temporary file and the
    // moved-aside file.
    let seen = std::cell::RefCell::new(Vec::new());
    let root = open_root(&folder).unwrap();
    let private = PrivatePaths::default();
    let data_dir = home.join("data");
    let job = Job {
        parent: root.as_fd(),
        name: ".env",
        rel: "apps/web/.env",
        salt: SALT,
        project_id: "p1",
        private: &private,
        data_dir: Some(data_dir.as_path()),
    };
    let digest = Rig::digest(b"OLD=1\n");
    let made = commit(
        &job,
        b"NEW=2\n",
        Some(&digest),
        || seen.borrow_mut().extend(staging_names(&folder)),
        || {},
    );
    assert_eq!(made, Ok(true));
    let seen = seen.into_inner();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].starts_with("secrets.fleetdeck-old-"), "{seen:?}");
    assert!(seen[1].starts_with("secrets.fleetdeck-tmp-"), "{seen:?}");
    let deny = MemberDeny::default();
    for name in &seen {
        assert!(deny.denies(&format!("apps/web/{name}")), "{name}");
        assert!(deny.denies(name), "{name}");
    }
    assert!(staging_names(&folder).is_empty());
}

#[test]
fn a_write_through_an_open_descriptor_after_the_snapshot_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    fs::write(home.join(".env"), b"OLD=1\n").unwrap();
    // An editor opened `.env` before the move aside.
    let mut open = fs::OpenOptions::new()
        .write(true)
        .open(home.join(".env"))
        .unwrap();
    let made = replace_in(
        &home,
        || {},
        || {
            // After the first read and the first backup, before the install.
            open.set_len(0).unwrap();
            open.write_all(b"LATE=9\n").unwrap();
        },
    );
    assert_eq!(made, Ok(true));
    assert_eq!(fs::read(home.join(".env")).unwrap(), b"NEW=2\n");
    assert_eq!(
        backup_contents(&home),
        [b"LATE=9\n".to_vec(), b"OLD=1\n".to_vec()]
    );
    assert!(staging_names(&home).is_empty());
}

#[test]
fn a_failed_second_backup_keeps_the_old_file_aside_and_answers_success() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    fs::write(home.join(".env"), b"OLD=1\n").unwrap();
    let mut open = fs::OpenOptions::new()
        .write(true)
        .open(home.join(".env"))
        .unwrap();
    let folder = backup_folder(&home.join("data"), "p1", ".env").unwrap();
    let made = replace_in(
        &home,
        || {},
        || {
            open.set_len(0).unwrap();
            open.write_all(b"LATE=9\n").unwrap();
            // A file where the backup folder was: no new backup can be made.
            fs::remove_dir_all(&folder).unwrap();
            fs::write(&folder, b"x").unwrap();
        },
    );
    assert_eq!(made, Ok(true));
    assert_eq!(fs::read(home.join(".env")).unwrap(), b"NEW=2\n");
    let names = staging_names(&home);
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].starts_with("secrets.fleetdeck-old-"), "{names:?}");
    assert_eq!(fs::read(home.join(&names[0])).unwrap(), b"LATE=9\n");
}

#[test]
fn an_unusable_second_read_keeps_the_old_file_aside_and_answers_success() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    fs::write(home.join(".env"), b"OLD=1\n").unwrap();
    let mut open = fs::OpenOptions::new()
        .write(true)
        .open(home.join(".env"))
        .unwrap();
    let made = replace_in(
        &home,
        || {},
        || {
            // Larger than the limit: the second read refuses it.
            open.write_all(&vec![b'a'; 512 * 1024 + 1]).unwrap();
        },
    );
    assert_eq!(made, Ok(true));
    assert_eq!(fs::read(home.join(".env")).unwrap(), b"NEW=2\n");
    assert_eq!(staging_names(&home).len(), 1);
    assert_eq!(backup_contents(&home), [b"OLD=1\n".to_vec()]);
}
