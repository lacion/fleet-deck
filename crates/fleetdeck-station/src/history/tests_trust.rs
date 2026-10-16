//! Tests of the trusted path, the mount table and the identity rules (B7).

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::HistoryUnavailable;
use fleetdeck_proto::native::{HomeState, ImportState};

use super::Limits;
use super::browse::{Budget, run_list};
use super::tests_support::*;
use crate::workspace::PrivatePaths;
use std::sync::Arc;

use super::trust::{Identity, Mounts, PathCheck, rollout_id, supported_fs, trusted_path};

fn my_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A temp directory that the trusted-path rules can pass, or a printed skip.
fn temp() -> Option<(tempfile::TempDir, Mounts)> {
    let dir = tempfile::tempdir().unwrap();
    let mounts = Mounts::load();
    let root = dir.path().canonicalize().unwrap();
    let handle = std::fs::File::open(&root).unwrap();
    let dev = handle.metadata().unwrap().dev();
    if !supported_fs(&handle) || !mounts.verifiable(dev, &root) {
        eprintln!("skipped: the temp directory is not on a supported filesystem and mount");
        return None;
    }
    Some((dir, mounts))
}

fn untrusted(why: &'static str) -> Result<(), PathCheck> {
    Err(PathCheck::Untrusted(why))
}

#[test]
fn a_private_path_of_the_station_user_is_trusted() {
    let Some((dir, mounts)) = temp() else { return };
    let sub = dir.path().canonicalize().unwrap().join("a/b");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(trusted_path(&sub, my_uid(), &mounts), Ok(()));
}

#[test]
fn a_link_component_fails_without_being_followed() {
    let Some((dir, mounts)) = temp() else { return };
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("real/inner")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    assert_eq!(
        trusted_path(&root.join("link/inner"), my_uid(), &mounts),
        untrusted("link")
    );
    // A link as the last component fails too.
    std::os::unix::fs::symlink(root.join("real/inner"), root.join("last")).unwrap();
    assert_eq!(
        trusted_path(&root.join("last"), my_uid(), &mounts),
        untrusted("link")
    );
}

#[test]
fn a_group_or_other_writable_component_fails() {
    let Some((dir, mounts)) = temp() else { return };
    let root = dir.path().canonicalize().unwrap();
    for mode in [0o770, 0o707, 0o722] {
        let parent = root.join(format!("w{mode:o}"));
        std::fs::create_dir_all(parent.join("child")).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            trusted_path(&parent.join("child"), my_uid(), &mounts),
            untrusted("writable"),
            "{mode:o}"
        );
    }
}

#[test]
fn a_component_of_another_owner_fails() {
    let Some((_, mounts)) = temp() else { return };
    // A path below the home of the user: root-owned parents, then ours.
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return;
    };
    if trusted_path(&home, my_uid(), &mounts) != Ok(()) {
        eprintln!(
            "skipped: {} is not a trusted path for this user",
            home.display()
        );
        return;
    }
    // The station user is another one: the home belongs to us.
    assert_eq!(
        trusted_path(&home, my_uid() + 1, &mounts),
        untrusted("owner")
    );
}

#[test]
fn a_root_owned_sticky_parent_with_a_station_owned_child_is_trusted() {
    let Some((dir, mounts)) = temp() else { return };
    let root = dir.path().canonicalize().unwrap();
    let parent = root.parent().unwrap();
    let meta = std::fs::metadata(parent).unwrap();
    if meta.uid() != 0 || meta.mode() & 0o1000 == 0 || meta.mode() & 0o022 == 0 {
        eprintln!(
            "skipped: {} is not a root-owned sticky directory",
            parent.display()
        );
        return;
    }
    assert_eq!(trusted_path(&root, my_uid(), &mounts), Ok(()));
    // The same parent with a child of another user fails: the sticky rule
    // needs a station-owned next component.
    assert_eq!(
        trusted_path(&root, my_uid() + 1, &mounts),
        untrusted("writable")
    );
}

#[test]
fn a_missing_component_fails_closed() {
    let Some((dir, mounts)) = temp() else { return };
    let missing = dir.path().canonicalize().unwrap().join("nope/deeper");
    assert_eq!(
        trusted_path(&missing, my_uid(), &mounts),
        untrusted("missing")
    );
    assert_eq!(
        trusted_path(Path::new("relative"), my_uid(), &mounts),
        untrusted("io")
    );
}

#[test]
fn an_unsupported_filesystem_cannot_be_verified() {
    let proc = Path::new("/proc/self/mountinfo");
    if !proc.exists() {
        return;
    }
    let handle = std::fs::File::open("/proc/self").unwrap();
    assert!(!supported_fs(&handle));
    assert_eq!(
        trusted_path(proc, my_uid(), &Mounts::load()),
        Err(PathCheck::Unverifiable("fs_unsupported"))
    );
}

#[test]
fn an_unknown_mount_cannot_be_verified() {
    let Some((dir, _)) = temp() else { return };
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(
        trusted_path(&root, my_uid(), &Mounts::parse("")),
        Err(PathCheck::Unverifiable("mount_unverifiable"))
    );
}

// ------------------------------------------------------------ mountinfo

const PLAIN: &str =
    "36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw,errors=continue";
const IDMAPPED: &str =
    "101 80 8:17 / /srv/shared rw,relatime,idmapped shared:5 - ext4 /dev/sdb1 rw";
const SUPER_IDMAPPED: &str = "102 80 8:18 / /srv/two rw,relatime - ext4 /dev/sdb2 rw,idmapped";
const LOOP: &str = "103 80 7:3 / /mnt/image rw,relatime - ext4 /dev/loop3 rw";
const NESTED: &str = "104 101 8:17 /sub /srv/shared/plain rw,relatime - ext4 /dev/sdb1 rw";
const SPACE: &str = r"105 80 8:33 / /mnt/my\040disk rw - ext4 /dev/sdc1 rw";

fn dev(major: u32, minor: u32) -> u64 {
    rustix::fs::makedev(major, minor)
}

#[test]
fn mountinfo_lines_are_read_for_options_and_sources() {
    let text = [
        PLAIN,
        IDMAPPED,
        SUPER_IDMAPPED,
        LOOP,
        NESTED,
        SPACE,
        "garbage line",
    ]
    .join("\n");
    let mounts = Mounts::parse(&text);
    assert!(mounts.verifiable(dev(98, 0), Path::new("/mnt2/file")));
    assert!(!mounts.verifiable(dev(8, 17), Path::new("/srv/shared/data")));
    assert!(!mounts.verifiable(dev(8, 18), Path::new("/srv/two/x")));
    assert!(!mounts.verifiable(dev(7, 3), Path::new("/mnt/image/x")));
    // An escaped space in the mount point.
    assert!(mounts.verifiable(dev(8, 33), Path::new("/mnt/my disk/x")));
}

#[test]
fn the_longest_mount_point_of_the_device_decides() {
    let text = [IDMAPPED, NESTED].join("\n");
    let mounts = Mounts::parse(&text);
    // The nested plain mount of the same device wins below its point.
    assert!(mounts.verifiable(dev(8, 17), Path::new("/srv/shared/plain/file")));
    assert!(!mounts.verifiable(dev(8, 17), Path::new("/srv/shared/other")));
    // No mount for the device or the path: not verifiable.
    assert!(!mounts.verifiable(dev(9, 9), Path::new("/srv/shared/other")));
    assert!(!mounts.verifiable(dev(8, 17), Path::new("/elsewhere")));
}

#[test]
fn the_real_mount_table_loads() {
    if Path::new("/proc/self/mountinfo").exists() {
        let handle = std::fs::File::open("/").unwrap();
        let dev = handle.metadata().unwrap().dev();
        // The root is some mount; the answer is a bool either way.
        let _ = Mounts::load().verifiable(dev, Path::new("/"));
    }
}

// ------------------------------------------------------------- identity

fn claude_line(session: &str, cwd: &str) -> String {
    serde_json::json!({"type": "user", "sessionId": session, "cwd": cwd}).to_string()
}

fn run(identity: &mut Identity, lines: &[String]) -> bool {
    lines.iter().all(|l| identity.record(l.as_bytes()).is_ok())
}

#[test]
fn claude_identity_needs_the_stem_one_cwd_and_the_project_directory() {
    let cwd = "/work/my.app";
    let dir = "-work-my-app";
    let mut ok = Identity::new(ProviderKind::Claude, "abc", Some(dir));
    assert!(run(
        &mut ok,
        &[claude_line("abc", cwd), claude_line("abc", cwd)]
    ));
    assert_eq!(ok.finish().unwrap().cwd, cwd);
    let mut other_session = Identity::new(ProviderKind::Claude, "abc", Some(dir));
    assert!(!run(
        &mut other_session,
        &[claude_line("abc", cwd), claude_line("zzz", cwd)]
    ));
    let mut two_cwds = Identity::new(ProviderKind::Claude, "abc", Some(dir));
    assert!(!run(
        &mut two_cwds,
        &[claude_line("abc", cwd), claude_line("abc", "/work/other")]
    ));
    let mut wrong_dir = Identity::new(ProviderKind::Claude, "abc", Some("-wrong"));
    assert!(run(&mut wrong_dir, &[claude_line("abc", cwd)]));
    assert!(wrong_dir.finish().is_err());
    let mut no_session = Identity::new(ProviderKind::Claude, "abc", Some(dir));
    assert!(run(
        &mut no_session,
        &[serde_json::json!({"type": "ai-title", "cwd": cwd}).to_string()]
    ));
    assert!(no_session.finish().is_err());
    for bad in ["relative", "/with\0nul"] {
        let mut shape = Identity::new(ProviderKind::Claude, "abc", Some(dir));
        assert!(!run(&mut shape, &[claude_line("abc", bad)]), "{bad:?}");
    }
    let mut long = Identity::new(ProviderKind::Claude, "abc", Some(dir));
    assert!(!run(
        &mut long,
        &[claude_line("abc", &format!("/{}", "a".repeat(4100)))]
    ));
}

#[test]
fn the_title_is_cut_to_120_characters() {
    let mut id = Identity::new(ProviderKind::Claude, "abc", Some("-w"));
    let title = serde_json::json!({"type": "ai-title", "aiTitle": "t".repeat(300)}).to_string();
    assert!(run(&mut id, &[claude_line("abc", "/w"), title]));
    assert_eq!(id.finish().unwrap().title.unwrap().chars().count(), 120);
}

fn meta(id: &str, cwd: &str, parent: Option<&str>) -> String {
    serde_json::json!({"type": "session_meta", "payload": {"id": id, "cwd": cwd, "parent_thread_id": parent}})
        .to_string()
}

#[test]
fn codex_identity_starts_with_session_meta_and_later_records_agree() {
    let turn = |cwd: &str| {
        serde_json::json!({"type": "turn_context", "payload": {"cwd": cwd}}).to_string()
    };
    let mut ok = Identity::new(ProviderKind::Codex, "id1", None);
    assert!(run(
        &mut ok,
        &[meta("id1", "/w", None), turn("/w"), "not json".into()]
    ));
    assert_eq!(ok.finish().unwrap().parent, None);
    let mut parent = Identity::new(ProviderKind::Codex, "id1", None);
    assert!(run(&mut parent, &[meta("id1", "/w", Some("p-1"))]));
    assert_eq!(parent.finish().unwrap().parent.as_deref(), Some("p-1"));
    let mut bad_parent = Identity::new(ProviderKind::Codex, "id1", None);
    assert!(!run(&mut bad_parent, &[meta("id1", "/w", Some("../x"))]));
    let mut other_cwd = Identity::new(ProviderKind::Codex, "id1", None);
    assert!(!run(
        &mut other_cwd,
        &[meta("id1", "/w", None), turn("/other")]
    ));
    let mut other_id = Identity::new(ProviderKind::Codex, "id1", None);
    assert!(!run(
        &mut other_id,
        &[meta("id1", "/w", None), meta("id2", "/w", None)]
    ));
    let mut first_not_meta = Identity::new(ProviderKind::Codex, "id1", None);
    assert!(!run(
        &mut first_not_meta,
        &[turn("/w"), meta("id1", "/w", None)]
    ));
    let mut wrong_first_id = Identity::new(ProviderKind::Codex, "id1", None);
    assert!(!run(&mut wrong_first_id, &[meta("id2", "/w", None)]));
    let mut no_cwd = Identity::new(ProviderKind::Codex, "id1", None);
    assert!(!run(
        &mut no_cwd,
        &[serde_json::json!({"type": "session_meta", "payload": {"id": "id1"}}).to_string()]
    ));
}

#[test]
fn a_rollout_name_gives_its_id() {
    assert_eq!(
        rollout_id("rollout-2026-10-14T10-00-00-01a13f28-e384.jsonl"),
        Some("01a13f28-e384")
    );
    for bad in [
        "rollout-2026-10-14T10-00-00-.jsonl",
        "rollout-2026-10-14T10-00-00.jsonl",
        "rollout-2026-10-14 10-00-00-abc.jsonl",
        "rollout-short-abc.jsonl",
        "rollout-2026-10-14T10-00-00-a/b.jsonl",
        "other-2026-10-14T10-00-00-abc.jsonl",
        "rollout-2026-10-14T10-00-00-abc.json",
    ] {
        assert_eq!(rollout_id(bad), None, "{bad}");
    }
}

// ----------------------------------------------- the list, trust and identity

#[tokio::test]
async fn a_home_of_another_owner_is_not_trusted_and_not_walked() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.good_claude(ID_A);
    *b.rig.history.seams().uid.lock().unwrap() = Some(rustix::process::getuid().as_raw() + 1);
    let list = b.list().await;
    assert!(list.sessions.is_empty());
    let home = list
        .homes
        .iter()
        .find(|h| h.provider == ProviderKind::Claude)
        .unwrap();
    assert_eq!(
        home.state,
        HomeState::Unavailable {
            reason: "home_not_trusted".into()
        }
    );
}

#[tokio::test]
async fn a_private_file_is_not_read() {
    let Some(mut b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.good_claude(ID_A);
    let mut private = PrivatePaths::default();
    private.deny_all = true;
    b.rig.private = Arc::new(private);
    let list = b.list().await;
    let s = entry(&list, ID_A);
    assert_eq!(s.import, ImportState::NotTrusted);
    assert!(s.title.is_none() && s.cwd.is_none());
}

// ------------------------------------------------------------- identity

#[tokio::test]
async fn contradictions_give_not_trusted_without_text() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    // A project directory that is not encode_cwd(cwd).
    let wrong_dir = b.home().join("projects/-wrong");
    std::fs::create_dir_all(&wrong_dir).unwrap();
    write_private(
        &wrong_dir.join(format!("{ID_A}.jsonl")),
        &join(&[claude_rec(ID_A, &b.cwd, 1, "secret one")]),
    );
    // A record with another session id.
    b.put_claude(
        ID_B,
        &[
            claude_rec(ID_B, &b.cwd, 1, "ok"),
            claude_rec(ID_C, &b.cwd, 2, "secret two"),
        ],
    );
    // Two different cwds.
    b.put_claude(
        ID_C,
        &[
            claude_rec(ID_C, &b.cwd, 1, "ok"),
            claude_rec(ID_C, "/other/place", 2, "secret three"),
        ],
    );
    // A Codex id in both trees.
    b.put_codex(CODEX_ID, &[codex_meta_rec(CODEX_ID, &b.cwd, None)]);
    let archived = b.codex_home().join("archived_sessions");
    std::fs::create_dir_all(&archived).unwrap();
    write_private(
        &archived.join(format!("rollout-2026-10-13T09-00-00-{CODEX_ID}.jsonl")),
        &join(&[codex_meta_rec(CODEX_ID, &b.cwd, None)]),
    );
    let list = b.list().await;
    for s in &list.sessions {
        assert_eq!(s.import, ImportState::NotTrusted, "{}", s.native_id);
        assert!(s.cwd.is_none() && s.title.is_none() && s.first_prompt.is_none());
    }
    assert_eq!(list.sessions.len(), 5, "{:?}", list.sessions.len());
}

#[tokio::test]
async fn a_contradiction_in_the_tail_window_is_found() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let mut lines = vec![claude_rec(ID_A, &b.cwd, 1, "head")];
    // Filler between the windows (the head is 256 KiB, the tail 64 KiB).
    let filler = claude_rec(ID_A, &b.cwd, 2, &"x".repeat(1000));
    lines.extend(std::iter::repeat_n(filler, 500));
    lines.push(claude_rec("zzzzzzzz-other", &b.cwd, 3, "tail"));
    lines.push(claude_rec(ID_A, &b.cwd, 4, "last"));
    b.put_claude(ID_A, &lines);
    let list = b.list().await;
    assert_eq!(entry(&list, ID_A).import, ImportState::NotTrusted);
}

#[tokio::test]
async fn codex_later_records_must_agree() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.put_codex(
        ID_A,
        &[codex_meta_rec(ID_A, &b.cwd, None), turn_rec("/other/cwd")],
    );
    b.put_codex(
        ID_B,
        &[
            codex_meta_rec(ID_B, &b.cwd, None),
            codex_meta_rec(ID_C, &b.cwd, None),
        ],
    );
    // The first record is not session_meta.
    b.put_codex(
        ID_C,
        &[turn_rec(&b.cwd), codex_meta_rec(ID_C, &b.cwd, None)],
    );
    let list = b.list().await;
    for id in [ID_A, ID_B, ID_C] {
        assert_eq!(entry(&list, id).import, ImportState::NotTrusted, "{id}");
    }
}

// --------------------------------------------------------------- fixtures

/// Copies a fixture into the rig: every `cwd` is the rig path, and the
/// Codex parent thread is cleared.
fn load_fixture(name: &str, cwd: &str) -> Vec<String> {
    let text = std::fs::read_to_string(fixture(name)).unwrap();
    let path = regex_free_replace(&text, cwd);
    path.lines().map(str::to_owned).collect()
}

fn regex_free_replace(text: &str, cwd: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("\"<path ") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let end = tail.find('"').unwrap();
        out.push('"');
        out.push_str(cwd);
        rest = &tail[end..];
    }
    out.push_str(rest);
    out.replace(
        "\"parent_thread_id\":\"<text 3>\"",
        "\"parent_thread_id\":null",
    )
}

#[tokio::test]
async fn the_claude_fixtures_give_the_expected_fields() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let id = "00e00a5d-d49a-4946-b5bc-fcdf51bd4493";
    for name in [
        "claude-2.1.300-basic.jsonl",
        "claude-2.1.300-tools.jsonl",
        "claude-2.1.300-resumed.jsonl",
        "claude-2.1.300-sidechain-meta.jsonl",
    ] {
        let path = b.claude_path(&b.cwd, id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, &join(&load_fixture(name, &b.cwd)));
        let list = b.list().await;
        let s = entry(&list, id);
        assert_eq!(s.import, ImportState::Ok, "{name}");
        assert_eq!(s.cwd.as_deref(), Some(b.cwd.as_str()), "{name}");
        assert!(s.verified, "{name}");
    }
    let path = b.claude_path(&b.cwd, id);
    write_private(
        &path,
        &join(&load_fixture("claude-2.1.300-basic.jsonl", &b.cwd)),
    );
    let list = b.list().await;
    let s = entry(&list, id);
    assert_eq!(s.first_prompt.as_deref(), Some("<text 5>"));
    // The tools fixture holds no prompt that a person typed.
    write_private(
        &path,
        &join(&load_fixture("claude-2.1.300-tools.jsonl", &b.cwd)),
    );
    assert_eq!(entry(&b.list().await, id).first_prompt, None);
    write_private(
        &path,
        &join(&load_fixture("claude-2.1.300-basic.jsonl", &b.cwd)),
    );
    let list = b.list().await;
    let s = entry(&list, id);
    assert_eq!(s.title, None);
}

#[tokio::test]
async fn the_codex_fixtures_give_the_expected_fields() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let tools = b.codex_path(CODEX_ID);
    std::fs::create_dir_all(tools.parent().unwrap()).unwrap();
    write_private(
        &tools,
        &join(&load_fixture("codex-0.162.1-tools.jsonl", &b.cwd)),
    );
    let basic_id = "01a13777-3cb3-7a42-b7ff-9572798fa426";
    let basic = b.codex_path(basic_id);
    write_private(
        &basic,
        &join(&load_fixture("codex-0.162.1-basic.jsonl", &b.cwd)),
    );
    let list = b.list().await;
    let s = entry(&list, CODEX_ID);
    assert_eq!(s.import, ImportState::Ok);
    assert_eq!(s.cwd.as_deref(), Some(b.cwd.as_str()));
    assert!(s.parent_native_id.is_none());
    let s = entry(&list, basic_id);
    assert_eq!(s.import, ImportState::Ok);
    assert!(s.first_prompt.is_some());
}

#[tokio::test]
async fn a_cwd_through_a_link_is_path_not_trusted_or_a_cwd_mismatch_when_unverifiable() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let link = b.rig.root.join("work/link");
    std::os::unix::fs::symlink(&b.cwd, &link).unwrap();
    let via = link.to_string_lossy().into_owned();
    let path = b.claude_path(&via, ID_A);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_private(&path, &join(&[claude_rec(ID_A, &via, 1, "x")]));
    // The trusted path refuses the link component.
    let list = b.list().await;
    assert_eq!(entry(&list, ID_A).import, ImportState::PathNotTrusted);
    // Where nothing can be verified, the stored path is only not canonical.
    let list = run_list(
        &b.rig.store,
        &b.policy,
        &b.rig.private,
        q(),
        Budget::default(),
        Some(Mounts::parse("")),
    )
    .await
    .unwrap();
    assert_eq!(entry(&list, ID_A).import, ImportState::CwdMismatch);
    assert!(!entry(&list, ID_A).verified);
}

#[tokio::test]
async fn a_cwd_below_a_group_writable_directory_is_path_not_trusted() {
    use std::os::unix::fs::PermissionsExt;
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.good_claude(ID_A);
    let work = b.rig.root.join("work");
    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o770)).unwrap();
    let list = b.list().await;
    assert_eq!(entry(&list, ID_A).import, ImportState::PathNotTrusted);
    assert!(!entry(&list, ID_A).verified);
}

// -------------------------------------------------------- unverifiable

#[tokio::test]
async fn an_idmapped_or_unknown_mount_lists_entries_as_not_verified() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.good_claude(ID_A);
    let dev = std::fs::metadata(&b.rig.root).unwrap().dev();
    let (major, minor) = (rustix::fs::major(dev), rustix::fs::minor(dev));
    let idmapped = format!("1 0 {major}:{minor} / / rw,relatime,idmapped - ext4 /dev/sdz rw\n");
    for text in [idmapped.as_str(), ""] {
        let list = run_list(
            &b.rig.store,
            &b.policy,
            &b.rig.private,
            q(),
            Budget::default(),
            Some(Mounts::parse(text)),
        )
        .await
        .unwrap();
        let s = entry(&list, ID_A);
        assert!(!s.verified, "{text}");
        assert_eq!(s.import, ImportState::Ok, "{text}");
        assert!(list.homes.iter().all(|h| h.state == HomeState::Ok));
    }
}

#[tokio::test]
async fn a_contradiction_in_the_tail_gives_no_items() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.put_claude(
        ID_A,
        &[
            claude_rec(ID_A, &b.cwd, 1, "secret"),
            claude_rec(ID_B, &b.cwd, 2, "forged"),
        ],
    );
    let p = b
        .preview(query_of("claude", ID_A, pin_of(&path, &b.home())))
        .await
        .unwrap();
    unavailable(&p, HistoryUnavailable::NotTrusted);
    assert_eq!(p.cwd, "");
    assert_eq!(p.size, std::fs::metadata(&path).unwrap().len());
}

#[tokio::test]
async fn a_codex_id_in_two_files_is_not_trusted_in_the_preview() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.put_codex(CODEX_ID, &[codex_meta_rec(CODEX_ID, &b.cwd, None)]);
    let archived = b.codex_home().join("archived_sessions");
    std::fs::create_dir_all(&archived).unwrap();
    write_private(
        &archived.join(format!("rollout-2026-10-13T09-00-00-{CODEX_ID}.jsonl")),
        &join(&[codex_meta_rec(CODEX_ID, &b.cwd, None)]),
    );
    let p = b
        .preview(query_of("codex", CODEX_ID, pin_of(&path, &b.codex_home())))
        .await
        .unwrap();
    unavailable(&p, HistoryUnavailable::NotTrusted);
}
