//! The branch methods (DL-191) on real Git: the listing, the strict parser,
//! the committed diff, the hardened runner and the bounds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::branches::{BranchDiffQuery, BranchesQuery, MAX_ANSWER_BYTES};
use fleetdeck_proto::event::{DiffSummary, ProviderKind};
use fleetdeck_proto::files::{DiffScope, SessionDiff};
use fleetdeck_testkit::Det;

use super::*;
use crate::manager::policy::Policy;
use crate::store::{NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::PrivatePaths;

const TS: i64 = 1_700_000_000;

fn git_env(cmd: &mut Command, ts: i64) {
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", format!("{ts} +0000"))
        .env("GIT_COMMITTER_DATE", format!("{ts} +0000"));
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args(args);
    git_env(&mut cmd, TS);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn git_stdin(dir: &Path, args: &[&str], input: &str) {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::piped());
    git_env(&mut cmd, TS);
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "git {args:?}");
}

/// Writes `file`, commits it at the time `ts` and returns the new commit.
fn commit(dir: &Path, file: &str, content: &str, msg: &str, ts: i64) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", "."]);
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args(["commit", "-q", "-m", msg]);
    git_env(&mut cmd, ts);
    assert!(cmd.output().unwrap().status.success());
    git(dir, &["rev-parse", "HEAD"])
}

fn opts() -> GitOptions {
    GitOptions::isolated()
}

fn sid() -> SessionId {
    "s1".into()
}

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    c1: String,
    c2: String,
}

/// `main` has two commits and is checked out. `merged`, `same1` and `same2`
/// are at the first commit; `ahead` has two commits on top of the second;
/// `diverged` has one commit on top of the first.
fn repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let c1 = commit(&dir, "a.txt", "one\n", "first", TS);
    let c2 = commit(&dir, "a.txt", "one\ntwo\n", "second", TS + 10);
    for name in ["merged", "same1", "same2"] {
        git(&dir, &["branch", name, &c1]);
    }
    git(&dir, &["checkout", "-q", "-b", "ahead"]);
    commit(&dir, "b.txt", "bee\n", "add b", TS + 20);
    commit(&dir, "a.txt", "one\ntwo\nthree\n", "change a", TS + 30);
    git(&dir, &["checkout", "-q", "-b", "diverged", &c1]);
    commit(&dir, "d.txt", "dee\n", "diverge", TS + 40);
    git(&dir, &["checkout", "-q", "main"]);
    Repo {
        _tmp: tmp,
        dir,
        c1,
        c2,
    }
}

fn names(list: &RepoBranches) -> Vec<&str> {
    list.branches.iter().map(|b| b.name.as_str()).collect()
}

fn by_name<'a>(list: &'a RepoBranches, name: &str) -> &'a RepoBranch {
    list.branches.iter().find(|b| b.name == name).unwrap()
}

// -------------------------------------------------------------- the list

#[tokio::test]
async fn it_lists_merged_ahead_and_diverged_branches_newest_first() {
    let r = repo();
    let list = branches(&opts(), &r.dir).await.unwrap();
    assert_eq!(list.head.as_deref(), Some(r.c2.as_str()));
    assert_eq!(list.branch.as_deref(), Some("main"));
    assert!(!list.truncated);
    let order = names(&list);
    assert_eq!(&order[..2], ["diverged", "ahead"]);
    let mut rest: Vec<&str> = order[2..].to_vec();
    rest.sort_unstable();
    assert_eq!(rest, ["merged", "same1", "same2"]);
    let ahead = by_name(&list, "ahead");
    assert_eq!((ahead.ahead, ahead.behind), (2, 0));
    assert_eq!(ahead.at, TS + 30);
    assert_eq!(ahead.subject, "change a");
    let diverged = by_name(&list, "diverged");
    assert_eq!((diverged.ahead, diverged.behind), (1, 1));
    let merged = by_name(&list, "merged");
    assert_eq!((merged.ahead, merged.behind), (0, 1));
    assert_eq!(merged.head, r.c1);
}

#[tokio::test]
async fn branches_with_the_same_tip_are_all_listed_and_the_current_one_is_excluded_by_name() {
    let r = repo();
    // A second branch at the tip of the current one stays in the list.
    git(&r.dir, &["branch", "twin-of-main", &r.c2]);
    let list = branches(&opts(), &r.dir).await.unwrap();
    assert!(!names(&list).contains(&"main"));
    let twin = by_name(&list, "twin-of-main");
    assert_eq!((twin.ahead, twin.behind), (0, 0));
    assert_eq!(twin.head, r.c2);
    let tips: Vec<&str> = ["same1", "same2", "merged"]
        .iter()
        .map(|n| by_name(&list, n).head.as_str())
        .collect();
    assert_eq!(tips, [r.c1.as_str(); 3]);
}

#[tokio::test]
async fn a_detached_head_excludes_nothing() {
    let r = repo();
    git(&r.dir, &["checkout", "-q", "--detach", "main"]);
    let list = branches(&opts(), &r.dir).await.unwrap();
    assert_eq!(list.branch, None);
    assert_eq!(list.head.as_deref(), Some(r.c2.as_str()));
    assert!(names(&list).contains(&"main"));
}

#[tokio::test]
async fn an_unborn_head_gives_an_empty_list() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let list = branches(&opts(), &dir).await.unwrap();
    assert_eq!(list.head, None);
    assert_eq!(list.branch.as_deref(), Some("main"));
    assert!(list.branches.is_empty());
    assert!(!list.truncated);
}

#[tokio::test]
async fn a_folder_that_is_not_a_repository_answers_not_a_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let err = branches(&opts(), tmp.path()).await.unwrap_err();
    assert_eq!(err.code, "not_a_repository");
    let head = "a".repeat(40);
    let err = branch_diff(&opts(), tmp.path(), &sid(), &head, &PrivatePaths::default())
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_a_repository");
}

#[tokio::test]
async fn more_than_200_branches_are_cut_and_marked() {
    let r = repo();
    let mut refs = String::new();
    for n in 0..230 {
        refs.push_str(&format!("create refs/heads/many/b{n:03} {}\n", r.c1));
    }
    git_stdin(&r.dir, &["update-ref", "--stdin"], &refs);
    let list = branches(&opts(), &r.dir).await.unwrap();
    assert!(list.truncated);
    assert_eq!(list.branches.len(), 200);
}

#[tokio::test]
async fn git_below_2_45_is_refused_and_2_45_is_accepted() {
    let r = repo();
    let err = branches_with(&opts(), &r.dir, Some((2, 44)))
        .await
        .unwrap_err();
    assert_eq!(err.code, "git_too_old");
    let err = branch_diff_with(
        &opts(),
        &r.dir,
        &sid(),
        &r.c1,
        Some((1, 99)),
        &PrivatePaths::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "git_too_old");
    assert!(branches_with(&opts(), &r.dir, Some((2, 45))).await.is_ok());
    assert!(branches_with(&opts(), &r.dir, Some((3, 0))).await.is_ok());
}

#[test]
fn the_git_version_is_read_from_the_first_line() {
    assert_eq!(parse_git_version("git version 2.45.0\n"), Some((2, 45)));
    assert_eq!(
        parse_git_version("git version 2.39.5 (Apple Git-154)"),
        Some((2, 39))
    );
    assert_eq!(
        parse_git_version("git version 2.56.0.windows.1"),
        Some((2, 56))
    );
    assert_eq!(parse_git_version("something else"), None);
    assert_eq!(parse_git_version("git version x.y"), None);
}

// ------------------------------------------------------------ the parser

fn record(name: &str, oid: &str, unix: &str, subject: &str, counts: &str) -> Vec<u8> {
    format!("{name}\0{oid}\0{unix}\0{subject}\0{counts}\n").into_bytes()
}

const OID: &str = "0123456789abcdef0123456789abcdef01234567";

fn good(name: &str) -> Vec<u8> {
    record(&format!("refs/heads/{name}"), OID, "5", "subject", "2 1")
}

#[test]
fn the_parser_reads_five_fields_and_cuts_the_subject() {
    let long = "x".repeat(300);
    let out = record("refs/heads/a/b", OID, "42", &long, "3 4");
    let (list, truncated) = parse_branches(&out, false, None);
    assert!(!truncated);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "a/b");
    assert_eq!((list[0].ahead, list[0].behind, list[0].at), (3, 4, 42));
    assert_eq!(list[0].subject.chars().count(), 200);
}

#[test]
fn a_malformed_record_ends_the_parse_and_marks_the_list() {
    let sha256 = "a".repeat(64);
    let bad: Vec<Vec<u8>> = vec![
        // Four fields.
        format!("refs/heads/x\0{OID}\x005\x001 1\n").into_bytes(),
        // Six fields.
        format!("refs/heads/x\0{OID}\x005\0s\x001 1\0extra\n").into_bytes(),
        // Not under refs/heads/.
        record("refs/tags/x", OID, "5", "s", "1 1"),
        record("refs/heads/", OID, "5", "s", "1 1"),
        // A name over 256 bytes.
        record(
            &format!("refs/heads/{}", "n".repeat(250)),
            OID,
            "5",
            "s",
            "1 1",
        ),
        // A short, a non-hex and a signed id.
        record("refs/heads/x", "abc", "5", "s", "1 1"),
        record("refs/heads/x", &"g".repeat(40), "5", "s", "1 1"),
        // Not decimal.
        record("refs/heads/x", OID, "-5", "s", "1 1"),
        record("refs/heads/x", OID, "5", "s", "1 -1"),
        record("refs/heads/x", OID, "5", "s", "one 1"),
        record("refs/heads/x", OID, "5", "s", "1"),
    ];
    for record in bad {
        let mut out = good("first");
        out.extend_from_slice(&record);
        out.extend_from_slice(&good("never-read"));
        let (list, truncated) = parse_branches(&out, false, None);
        assert!(truncated, "{}", String::from_utf8_lossy(&record));
        assert_eq!(list.len(), 1, "{}", String::from_utf8_lossy(&record));
        assert_eq!(list[0].name, "first");
    }
    // A 64-digit id is valid.
    let ok = record("refs/heads/x", &sha256, "5", "s", "1 1");
    assert!(!parse_branches(&ok, false, None).1);
}

#[test]
fn a_cut_output_and_an_unfinished_record_mark_the_list() {
    let (list, truncated) = parse_branches(&good("a"), true, None);
    assert_eq!(list.len(), 1);
    assert!(truncated);
    let mut out = good("a");
    out.extend_from_slice(b"refs/heads/half\0");
    let (list, truncated) = parse_branches(&out, false, None);
    assert_eq!(list.len(), 1);
    assert!(truncated);
}

#[test]
fn the_201st_record_gives_200_branches_and_a_mark() {
    let mut out = Vec::new();
    for n in 0..201 {
        out.extend_from_slice(&good(&format!("b{n}")));
    }
    let (list, truncated) = parse_branches(&out, false, None);
    assert_eq!(list.len(), 200);
    assert!(truncated);
    // 200 records are complete.
    let mut out = Vec::new();
    for n in 0..200 {
        out.extend_from_slice(&good(&format!("b{n}")));
    }
    let (list, truncated) = parse_branches(&out, false, None);
    assert_eq!(list.len(), 200);
    assert!(!truncated);
    // The current branch is not in the list.
    let (list, _) = parse_branches(&out, false, Some("b7"));
    assert_eq!(list.len(), 199);
    assert!(list.iter().all(|b| b.name != "b7"));
}

// ------------------------------------------------------------- the diff

#[tokio::test]
async fn it_diffs_a_branch_against_its_merge_base() {
    let r = repo();
    let list = branches(&opts(), &r.dir).await.unwrap();
    let ahead = by_name(&list, "ahead").head.clone();
    let diff = branch_diff(&opts(), &r.dir, &sid(), &ahead, &PrivatePaths::default())
        .await
        .unwrap();
    assert_eq!(diff.scope, DiffScope::Committed);
    // The merge base of `main` and `ahead` is the second commit.
    assert_eq!(diff.base_oid, r.c2);
    assert!(!diff.truncated);
    assert!(diff.patch.contains("+bee"), "{}", diff.patch);
    assert!(diff.patch.contains("+three"));
    assert_eq!(
        diff.summary,
        DiffSummary {
            files_changed: 2,
            insertions: 2,
            deletions: 0
        }
    );
    // A branch behind HEAD has an empty diff; the base is its own tip.
    let merged = by_name(&list, "merged").head.clone();
    let diff = branch_diff(&opts(), &r.dir, &sid(), &merged, &PrivatePaths::default())
        .await
        .unwrap();
    assert_eq!(diff.base_oid, r.c1);
    assert!(diff.patch.is_empty());
}

#[tokio::test]
async fn a_head_must_be_the_tip_of_a_listed_branch() {
    let r = repo();
    let list = branches(&opts(), &r.dir).await.unwrap();
    let ahead = by_name(&list, "ahead").head.clone();
    let invalid = |err: BranchError| assert_eq!(err.code, "invalid_params", "{err:?}");
    // The tip of the current branch is not listed.
    invalid(
        branch_diff(&opts(), &r.dir, &sid(), &r.c2, &PrivatePaths::default())
            .await
            .unwrap_err(),
    );
    // An object that is no branch tip: a parent of `ahead`.
    let parent = git(&r.dir, &["rev-parse", "ahead~1"]);
    invalid(
        branch_diff(&opts(), &r.dir, &sid(), &parent, &PrivatePaths::default())
            .await
            .unwrap_err(),
    );
    // Not an id.
    let not_hex = "g".repeat(40);
    for bad in [
        "",
        "ahead",
        "HEAD",
        "--output=/tmp/x",
        "abc",
        not_hex.as_str(),
    ] {
        invalid(
            branch_diff(&opts(), &r.dir, &sid(), bad, &PrivatePaths::default())
                .await
                .unwrap_err(),
        );
    }
    // The case of the hex digits does not matter.
    let upper = ahead.to_ascii_uppercase();
    let diff = branch_diff(&opts(), &r.dir, &sid(), &upper, &PrivatePaths::default())
        .await
        .unwrap();
    assert!(diff.patch.contains("+bee"));
}

#[tokio::test]
async fn a_branch_without_common_history_answers_no_common_history() {
    let r = repo();
    git(&r.dir, &["checkout", "-q", "--orphan", "island"]);
    git(&r.dir, &["rm", "-rf", "-q", "."]);
    let island = commit(&r.dir, "z.txt", "zed\n", "orphan", TS + 50);
    git(&r.dir, &["checkout", "-q", "main"]);
    let err = branch_diff(&opts(), &r.dir, &sid(), &island, &PrivatePaths::default())
        .await
        .unwrap_err();
    assert_eq!(err.code, "no_common_history");
}

#[tokio::test]
async fn a_large_patch_is_cut_to_the_limits_of_the_answer() {
    let r = repo();
    // Control characters take six bytes of JSON each.
    let line = format!("{}\n", "\u{1}".repeat(100));
    git(&r.dir, &["checkout", "-q", "ahead"]);
    commit(&r.dir, "big.txt", &line.repeat(3000), "big", TS + 60);
    git(&r.dir, &["checkout", "-q", "main"]);
    let list = branches(&opts(), &r.dir).await.unwrap();
    let head = by_name(&list, "ahead").head.clone();
    let diff = branch_diff(&opts(), &r.dir, &sid(), &head, &PrivatePaths::default())
        .await
        .unwrap();
    assert!(diff.truncated);
    let size = serde_json::to_vec(&diff).unwrap().len();
    assert!(size <= MAX_ANSWER_BYTES, "{size}");
    assert!(!diff.patch.is_empty());
    // The summary counts the whole diff, not the cut patch.
    assert_eq!(diff.summary.files_changed, 3);
}

#[test]
fn the_answer_cut_is_at_a_character_boundary() {
    let diff = SessionDiff {
        session_id: sid(),
        base_oid: "b".repeat(40),
        summary: DiffSummary {
            files_changed: 1,
            insertions: 1,
            deletions: 0,
        },
        patch: "é\u{1}\"\\".repeat(100_000),
        truncated: false,
        scope: DiffScope::Committed,
    };
    let fitted = fit(diff);
    assert!(fitted.truncated);
    assert!(serde_json::to_vec(&fitted).unwrap().len() <= MAX_ANSWER_BYTES);
    assert!(fitted.patch.len() > 1000);
}

// ------------------------------------------------------- the hardened runner

#[tokio::test]
async fn a_hostile_repository_config_runs_no_program_and_changes_nothing() {
    let r = repo();
    let list = branches(&opts(), &r.dir).await.unwrap();
    let ahead = by_name(&list, "ahead").head.clone();
    // A submodule entry in the branch, so that `diff.submodule` matters.
    git(&r.dir, &["checkout", "-q", "ahead"]);
    git(
        &r.dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},sub", r.c1),
        ],
    );
    let mut cmd = Command::new("git");
    cmd.current_dir(&r.dir)
        .args(["commit", "-q", "-m", "gitlink"]);
    git_env(&mut cmd, TS + 70);
    assert!(cmd.output().unwrap().status.success());
    git(&r.dir, &["checkout", "-q", "main"]);
    let list = branches(&opts(), &r.dir).await.unwrap();
    let head = by_name(&list, "ahead").head.clone();
    assert_ne!(head, ahead);
    let clean = branch_diff(&opts(), &r.dir, &sid(), &head, &PrivatePaths::default())
        .await
        .unwrap();
    assert!(clean.patch.contains("Subproject commit"), "{}", clean.patch);

    // The hostile configuration, set last: it would run for the helper above.
    let tmp = r.dir.parent().unwrap();
    let marker = tmp.join("marker");
    let script = tmp.join("evil.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho ran >> '{}'\ncat \"$@\" 2>/dev/null\n",
            marker.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let script_path = script.to_str().unwrap();
    std::fs::write(r.dir.join(".gitattributes"), "*.txt diff=evil\n").unwrap();
    std::fs::write(r.dir.join(".git/info/attributes"), "*.txt diff=evil\n").unwrap();
    for (key, value) in [
        ("diff.external", script_path),
        ("diff.evil.textconv", script_path),
        ("diff.evil.command", script_path),
        ("diff.submodule", "diff"),
        ("core.fsmonitor", script_path),
        ("core.pager", script_path),
    ] {
        git(&r.dir, &["config", key, value]);
    }
    // Control: plain Git does run the program in this repository.
    let mut cmd = Command::new("git");
    cmd.current_dir(&r.dir)
        .args(["diff", &r.c2, &head])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let _ = cmd.output().unwrap();
    assert!(
        marker.exists(),
        "the hostile configuration must be effective"
    );
    std::fs::remove_file(&marker).unwrap();

    let hostile = branch_diff(&opts(), &r.dir, &sid(), &head, &PrivatePaths::default())
        .await
        .unwrap();
    let listed = branches(&opts(), &r.dir).await.unwrap();
    assert!(!marker.exists(), "a program of the repository ran");
    assert_eq!(hostile, clean);
    assert_eq!(names(&listed), names(&list));
}

#[tokio::test]
async fn the_runner_adds_the_hardened_options_and_variables() {
    let args = hardened_args(&["diff", "a", "b"]);
    assert_eq!(
        args,
        [
            "-c",
            "core.pager=cat",
            "-c",
            "diff.external=",
            "-c",
            "core.fsmonitor=false",
            "diff",
            "a",
            "b"
        ]
    );
    let mut vars: Vec<&str> = HARDENED_ENV.iter().map(|(k, _)| *k).collect();
    vars.sort_unstable();
    assert_eq!(
        vars,
        ["GIT_NO_LAZY_FETCH", "GIT_PAGER", "GIT_TERMINAL_PROMPT"]
    );
    // The environment reaches the child.
    let r = repo();
    let git = git_for(&opts(), &r.dir);
    let (code, out, _) = git
        .bounded_with_env(&["var", "GIT_PAGER"], &[("GIT_PAGER", "my-pager")], 100)
        .await
        .unwrap();
    assert_eq!(
        (code, String::from_utf8_lossy(&out).trim()),
        (0, "my-pager")
    );
}

// ---------------------------------------------------- the link methods

fn policy(root: &Path) -> Policy {
    Policy::new(
        vec![root.to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )
}

async fn store_with(session: &str, cwd: &Path) -> Store {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(NewSession {
            session_id: session.into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: cwd.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    store
}

#[tokio::test]
async fn the_methods_resolve_the_session_directory_with_the_file_service_rules() {
    let r = repo();
    let root = r.dir.parent().unwrap().to_path_buf();
    let store = store_with("s1", &r.dir).await;
    let private = PrivatePaths::default();
    let policy = policy(&root);
    let list = run_branches(
        &store,
        &policy,
        &private,
        &opts(),
        BranchesQuery { session_id: sid() },
    )
    .await
    .unwrap();
    assert_eq!(list.branch.as_deref(), Some("main"));
    let head = by_name(&list, "ahead").head.clone();
    let diff = run_branch_diff(
        &store,
        &policy,
        &private,
        &opts(),
        BranchDiffQuery {
            session_id: sid(),
            head,
        },
    )
    .await
    .unwrap();
    assert_eq!(diff.session_id, sid());
    // An unknown session.
    let err = run_branches(
        &store,
        &policy,
        &private,
        &opts(),
        BranchesQuery {
            session_id: "nope".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "unknown_session");
    // A directory outside the allowed roots.
    let other = tempfile::tempdir().unwrap();
    let elsewhere = other.path().canonicalize().unwrap();
    let store = store_with("s2", &elsewhere).await;
    let err = run_branches(
        &store,
        &policy,
        &private,
        &opts(),
        BranchesQuery {
            session_id: "s2".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "outside_roots");
}

#[tokio::test]
async fn a_request_waits_for_the_session_lock_under_the_deadline() {
    let held = session_lock(&sid()).lock_owned().await;
    let wait = Duration::from_millis(150);
    let started = std::time::Instant::now();
    let err = within_for(wait, Some(&sid()), async { Ok(()) })
        .await
        .unwrap_err();
    assert_eq!(err.code, "busy");
    assert!(started.elapsed() >= wait);
    drop(held);
    // The lock is free again.
    within_for(wait, Some(&sid()), async { Ok(()) })
        .await
        .unwrap();
    // Another session is not blocked by it.
    let held = session_lock(&sid()).lock_owned().await;
    let other: SessionId = "s-other".into();
    within_for(wait, Some(&other), async { Ok(()) })
        .await
        .unwrap();
    drop(held);
}
