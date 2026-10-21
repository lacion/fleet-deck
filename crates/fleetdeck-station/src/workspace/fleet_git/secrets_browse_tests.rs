//! `project.secret_browse` (Fleet Sync): the entries of one folder of a home,
//! their kinds, their reasons and their order.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;

use fleetdeck_proto::fleet_git_secrets::{SecretBrowseAnswer, SecretEntryKind};
use serde_json::json;

use super::tests::{Rig, code_of};
use super::*;

impl Rig {
    async fn browse(&self, dir: &str) -> Result<SecretBrowseAnswer, SecretError> {
        let mut params = self.base();
        params["dir"] = json!(dir);
        let value = self.call("project.secret_browse", params).await?;
        Ok(serde_json::from_value(value).unwrap())
    }

    /// The names of the entries with their kind, eligibility and reason.
    fn rows(answer: &SecretBrowseAnswer) -> Vec<(String, SecretEntryKind, bool, Option<String>)> {
        answer
            .entries
            .iter()
            .map(|e| (e.name.clone(), e.kind, e.eligible, e.reason.clone()))
            .collect()
    }
}

fn row(
    name: &str,
    kind: SecretEntryKind,
    eligible: bool,
    reason: Option<&str>,
) -> (String, SecretEntryKind, bool, Option<String>) {
    (name.to_owned(), kind, eligible, reason.map(str::to_owned))
}

#[tokio::test]
async fn the_home_folder_lists_hidden_entries_with_kinds_reasons_and_order() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"A=1\n");
    rig.write(".env.example", b"A=\n");
    rig.git(&["add", "-f", ".env.example"]);
    rig.write("README.md", b"# hi\n");
    rig.git(&["add", "README.md"]);
    rig.write(".env.big", &vec![b'x'; 512 * 1024 + 1]);
    symlink(".env", rig.home.join(".env.link")).unwrap();
    fs::create_dir(rig.home.join(".hidden")).unwrap();
    fs::create_dir(rig.home.join("config")).unwrap();
    let _socket = UnixListener::bind(rig.home.join("sock")).unwrap();
    let answer = rig.browse("").await.unwrap();
    assert_eq!(answer.dir, "");
    assert!(!answer.truncated);
    let file = SecretEntryKind::File;
    assert_eq!(
        Rig::rows(&answer),
        [
            row(".hidden", SecretEntryKind::Dir, false, None),
            row("config", SecretEntryKind::Dir, false, None),
            row(".env", file, true, None),
            row(".env.big", file, false, Some("too_large")),
            row(".env.example", file, false, Some("not_ignored")),
            row(".env.link", SecretEntryKind::Other, false, Some("symlink")),
            // Tracked, so Git does not report it as ignored.
            row(".env.tracked", file, false, Some("not_ignored")),
            row(".gitignore", file, false, Some("not_member_hidden")),
            row("README.md", file, false, Some("not_member_hidden")),
            row("sock", SecretEntryKind::Other, false, Some("not_regular")),
            row("tracked.txt", file, false, Some("not_member_hidden")),
        ]
    );
    // The size is there for the regular files only.
    let size = |name: &str| answer.entries.iter().find(|e| e.name == name).unwrap().size;
    assert_eq!(size(".env"), Some(4));
    assert_eq!(size(".env.big"), Some(512 * 1024 + 1));
    assert_eq!(size(".hidden"), None);
    assert_eq!(size(".env.link"), None);
    assert_eq!(size("sock"), None);
    // The `.git` folder is not an entry.
    assert!(answer.entries.iter().all(|e| e.name != ".git"));
}

#[tokio::test]
async fn a_sub_folder_lists_its_own_entries() {
    let Some(rig) = Rig::new().await else { return };
    rig.write("config/.env.local", b"A=1\n");
    rig.write("config/app.local", b"B=2\n");
    rig.write("config/deeper/.env", b"C=3\n");
    let answer = rig.browse("config").await.unwrap();
    assert_eq!(answer.dir, "config");
    let file = SecretEntryKind::File;
    assert_eq!(
        Rig::rows(&answer),
        [
            row("deeper", SecretEntryKind::Dir, false, None),
            row(".env.local", file, true, None),
            // Ignored by Git, but members can read this name.
            row("app.local", file, false, Some("not_member_hidden")),
        ]
    );
    let deeper = rig.browse("config/deeper").await.unwrap();
    assert_eq!(
        Rig::rows(&deeper),
        [row(".env", file, true, None)],
        "the path of a nested file is checked as a whole"
    );
}

#[tokio::test]
async fn a_bad_dir_is_invalid_path() {
    let Some(rig) = Rig::new().await else { return };
    for bad in [
        "../x", "a/../x", "./x", "a/./x", "/etc", "a//b", "a/", "a\\b", ".git", "a/.GIT/b", "a\0b",
    ] {
        let e = rig.browse(bad).await.unwrap_err();
        assert_eq!(e.code, "invalid_path", "{bad:?}");
        assert_eq!(e.text, SECRET_PATH_TEXT, "{bad:?}");
    }
    let long = format!("{}x", "d/".repeat(200));
    assert_eq!(code_of(rig.browse(&long).await), "invalid_path");
    // The member-hidden rule does not apply to a folder.
    fs::create_dir(rig.home.join("README.md.d")).unwrap();
    assert!(rig.browse("README.md.d").await.is_ok());
}

#[tokio::test]
async fn a_link_a_file_and_a_missing_folder_are_refused() {
    let Some(rig) = Rig::new().await else { return };
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join(".env"), b"S=1\n").unwrap();
    symlink(outside.path(), rig.home.join("linkdir")).unwrap();
    assert_eq!(code_of(rig.browse("linkdir").await), "symlink");
    // A link below a folder is refused too.
    fs::create_dir(rig.home.join("real")).unwrap();
    symlink(outside.path(), rig.home.join("real/inner")).unwrap();
    assert_eq!(code_of(rig.browse("real/inner").await), "symlink");
    // A file where a folder is named.
    assert_eq!(code_of(rig.browse("tracked.txt").await), "symlink");
    let e = rig.browse("nope").await.unwrap_err();
    assert_eq!((e.code, e.text), ("missing", "The folder is missing."));
    assert_eq!(code_of(rig.browse("real/nope").await), "missing");
}

#[tokio::test]
async fn more_than_500_entries_are_cut_and_marked() {
    let Some(rig) = Rig::new().await else { return };
    for n in 0..501 {
        rig.write(&format!("many/f{n:03}"), b"x");
    }
    let answer = rig.browse("many").await.unwrap();
    assert!(answer.truncated);
    assert_eq!(answer.entries.len(), 500);
    // The first 500 names by byte order stay.
    assert_eq!(answer.entries[0].name, "f000");
    assert_eq!(answer.entries[499].name, "f499");
    fs::remove_file(rig.home.join("many/f500")).unwrap();
    let answer = rig.browse("many").await.unwrap();
    assert!(!answer.truncated);
    assert_eq!(answer.entries.len(), 500);
}

#[tokio::test]
async fn the_folders_come_first_inside_the_kept_names() {
    let Some(rig) = Rig::new().await else { return };
    for n in 0..500 {
        rig.write(&format!("many/f{n:03}"), b"x");
    }
    // `zz` sorts after the 500 files, so it is cut.
    fs::create_dir(rig.home.join("many/zz")).unwrap();
    let answer = rig.browse("many").await.unwrap();
    assert!(answer.truncated);
    assert!(
        answer
            .entries
            .iter()
            .all(|e| e.kind == SecretEntryKind::File)
    );
    // `a` sorts first and is a folder: it comes first and a file is cut.
    fs::remove_dir(rig.home.join("many/zz")).unwrap();
    fs::create_dir(rig.home.join("many/a")).unwrap();
    let answer = rig.browse("many").await.unwrap();
    assert!(answer.truncated);
    assert_eq!(answer.entries[0].name, "a");
    assert_eq!(answer.entries[0].kind, SecretEntryKind::Dir);
    assert_eq!(answer.entries.len(), 500);
}

#[tokio::test]
async fn a_name_that_is_not_utf8_is_skipped() {
    let Some(rig) = Rig::new().await else { return };
    let bad = rig.home.join(OsStr::from_bytes(b"bad-\xff-name"));
    fs::write(bad, b"x").unwrap();
    rig.write("good", b"x");
    let answer = rig.browse("").await.unwrap();
    assert!(answer.entries.iter().any(|e| e.name == "good"));
    assert!(answer.entries.iter().all(|e| !e.name.starts_with("bad-")));
}

#[tokio::test]
async fn a_private_or_unreadable_file_is_not_eligible() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".gitignore", b".env*\n");
    rig.write(".env.hard", b"A=1\n");
    fs::hard_link(rig.home.join(".env.hard"), rig.home.join(".env.hard2")).unwrap();
    rig.write(".env.locked", b"A=1\n");
    fs::set_permissions(
        rig.home.join(".env.locked"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let answer = rig.browse("").await.unwrap();
    let reason = |name: &str| {
        answer
            .entries
            .iter()
            .find(|e| e.name == name)
            .unwrap()
            .reason
            .clone()
    };
    assert_eq!(reason(".env.hard").as_deref(), Some("private_file"));
    assert_eq!(reason(".env.hard2").as_deref(), Some("private_file"));
    // A user that can read a file with mode 0 (root) gets no failed open.
    if fs::File::open(rig.home.join(".env.locked")).is_err() {
        let locked = answer
            .entries
            .iter()
            .find(|e| e.name == ".env.locked")
            .unwrap();
        assert_eq!(locked.kind, SecretEntryKind::File);
        assert!(!locked.eligible);
        assert_eq!(locked.reason.as_deref(), Some("unreadable"));
        assert_eq!(locked.size, Some(4));
    }
}

#[tokio::test]
async fn the_home_checks_of_the_other_methods_apply() {
    let Some(rig) = Rig::new().await else { return };
    let mut params = rig.base();
    params["dir"] = json!("");
    params["generation"] = json!(2);
    let e = rig
        .call("project.secret_browse", params.clone())
        .await
        .unwrap_err();
    assert_eq!(e.code, "stale_generation");
    params["generation"] = json!(1);
    let other = remote_identity("https://github.com/acme/other.git", LocalRemotes::Allow).unwrap();
    params["dest"] = json!(other.dest);
    let e = rig
        .call("project.secret_browse", params.clone())
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_this_repo");
    params["dest"] = json!(rig.dest);
    params["surprise"] = json!(1);
    let e = rig.call("project.secret_browse", params).await.unwrap_err();
    assert_eq!(e.code, "invalid_params");
}
