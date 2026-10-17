use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind};

use super::*;
use crate::integrations::exec::Programs;

/// A fake `gh` (and `glab`) that records its arguments and prints the file
/// `answer.<n>` for its n-th call (else the file `answer`), then exits with
/// the number in the file `exit`.
fn fake(dir: &Path, answer: &str, exit: i32) -> Programs {
    let script = dir.join("forge-cli");
    fs::write(dir.join("answer"), answer).unwrap();
    fs::write(dir.join("exit"), exit.to_string()).unwrap();
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> {d}/args\nn=$(cat {d}/n 2>/dev/null || echo 0)\nn=$((n+1))\necho $n > {d}/n\n\
             if [ -e {d}/answer.$n ]; then cat {d}/answer.$n; else cat {d}/answer; fi\nexit $(cat {d}/exit)\n",
            d = dir.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    Programs {
        gh: script.clone(),
        glab: script,
        curl: "curl".into(),
    }
}

fn binding(kind: ForgeKind) -> ForgeBinding {
    ForgeBinding {
        kind,
        host: match kind {
            ForgeKind::Github => "github.com".to_owned(),
            ForgeKind::Gitlab => "gitlab.com".to_owned(),
        },
        repo: "acme/app".to_owned(),
    }
}

const WAIT: Duration = Duration::from_secs(10);

/// The forge of `b` over the fake CLI.
fn forge_of<'a>(programs: &'a Programs, hidden: &'a [String], b: &'a ForgeBinding) -> Forge<'a> {
    Forge {
        programs,
        hidden,
        binding: b,
    }
}

/// A GitHub answer: the headers, then the pull requests `(number, repo)`
/// (`repo` empty: a head without a repository).
fn gh_page(link: Option<&str>, prs: &[(u64, &str)]) -> String {
    let items: Vec<String> = prs
        .iter()
        .map(|(n, repo)| {
            let head = if repo.is_empty() {
                r#"{"ref":"fd/a/b","repo":null}"#.to_owned()
            } else {
                format!(r#"{{"ref":"fd/a/b","repo":{{"full_name":"{repo}"}}}}"#)
            };
            format!(
                r#"{{"number":{n},"html_url":"https://github.com/acme/app/pull/{n}","head":{head}}}"#
            )
        })
        .collect();
    let link = link.map_or_else(String::new, |l| format!("Link: {l}\r\n"));
    format!("HTTP/2.0 200 OK\r\n{link}\r\n[{}]", items.join(","))
}

/// A GitLab answer: the headers, then the merge requests `(iid, source,
/// target)`.
fn gl_page(next: &str, mrs: &[(u64, u64, u64)]) -> String {
    let items: Vec<String> = mrs
        .iter()
        .map(|(n, source, target)| {
            format!(
                r#"{{"iid":{n},"web_url":"https://gitlab.com/acme/app/-/merge_requests/{n}","source_project_id":{source},"target_project_id":{target}}}"#
            )
        })
        .collect();
    format!(
        "HTTP/2.0 200 OK\r\nX-Next-Page: {next}\r\n\r\n[{}]",
        items.join(",")
    )
}

fn numbers(got: PrLookup) -> Vec<u64> {
    match got {
        PrLookup::Found(found) => found.iter().map(|p| p.number).collect(),
        PrLookup::TooManyPages => panic!("too many pages"),
    }
}

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("args"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn the_lookup_is_exact_of_this_repository_and_pages_of_one_hundred() {
    let dir = tempfile::tempdir().unwrap();
    let programs = fake(dir.path(), &gh_page(None, &[(7, "Acme/App")]), 0);
    let b = binding(ForgeKind::Github);
    let hidden = Vec::new();
    let forge = forge_of(&programs, &hidden, &b);
    let got = pr_find(&forge, None, "fd/a/b", "main", WAIT).await.unwrap();
    assert_eq!(
        got,
        PrLookup::Found(vec![PrRef {
            number: 7,
            url: "https://github.com/acme/app/pull/7".to_owned()
        }])
    );
    assert_eq!(
        calls(dir.path()),
        [
            "api --include -X GET repos/acme/app/pulls?state=open&head=acme%3Afd%2Fa%2Fb&base=main&per_page=100&page=1"
        ]
    );
}

#[tokio::test]
async fn a_github_request_from_a_fork_or_without_a_head_repository_is_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let page = gh_page(
        None,
        &[
            (1, "mallory/app"),
            (2, ""),
            (3, "acme/app-fork"),
            (4, "acme/app"),
        ],
    );
    let programs = fake(dir.path(), &page, 0);
    let b = binding(ForgeKind::Github);
    let hidden = Vec::new();
    let forge = forge_of(&programs, &hidden, &b);
    let got = pr_find(&forge, None, "fd/a/b", "main", WAIT).await.unwrap();
    assert_eq!(numbers(got), [4]);
    // Only a fork: nothing found.
    fs::write(
        dir.path().join("answer"),
        gh_page(None, &[(1, "mallory/app")]),
    )
    .unwrap();
    let got = pr_find(&forge, None, "fd/a/b", "main", WAIT).await.unwrap();
    assert_eq!(numbers(got), Vec::<u64>::new());
}

#[tokio::test]
async fn a_github_match_on_page_two_is_found_after_the_link_header() {
    let dir = tempfile::tempdir().unwrap();
    let programs = fake(dir.path(), "", 0);
    let link = r#"<https://api.github.com/x?page=2>; rel="next", <https://api.github.com/x?page=2>; rel="last""#;
    fs::write(
        dir.path().join("answer.1"),
        gh_page(Some(link), &[(1, "mallory/app")]),
    )
    .unwrap();
    fs::write(
        dir.path().join("answer.2"),
        gh_page(None, &[(2, "acme/app")]),
    )
    .unwrap();
    let b = binding(ForgeKind::Github);
    let hidden = Vec::new();
    let forge = forge_of(&programs, &hidden, &b);
    let got = pr_find(&forge, None, "fd/a/b", "main", WAIT).await.unwrap();
    assert_eq!(numbers(got), [2]);
    let seen = calls(dir.path());
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].ends_with("per_page=100&page=1"), "{seen:?}");
    assert!(seen[1].ends_with("per_page=100&page=2"), "{seen:?}");
}

#[tokio::test]
async fn more_than_ten_pages_are_too_many() {
    let dir = tempfile::tempdir().unwrap();
    let link = r#"<https://api.github.com/x>; rel="next""#;
    let programs = fake(dir.path(), &gh_page(Some(link), &[(1, "acme/app")]), 0);
    let b = binding(ForgeKind::Github);
    let hidden = Vec::new();
    let forge = forge_of(&programs, &hidden, &b);
    let got = pr_find(&forge, None, "fd/a/b", "main", WAIT).await.unwrap();
    assert_eq!(got, PrLookup::TooManyPages);
    assert_eq!(calls(dir.path()).len(), 10);
    // Ten pages, the tenth without a next page: that is the whole answer.
    fs::write(dir.path().join("n"), "0").unwrap();
    fs::remove_file(dir.path().join("args")).unwrap();
    fs::write(
        dir.path().join("answer.10"),
        gh_page(None, &[(2, "acme/app")]),
    )
    .unwrap();
    let got = pr_find(&forge, None, "fd/a/b", "main", WAIT).await.unwrap();
    assert_eq!(numbers(got), [1, 1, 1, 1, 1, 1, 1, 1, 1, 2]);
}

#[tokio::test]
async fn a_gitlab_lookup_reads_only_the_merge_requests_of_the_bound_project() {
    let dir = tempfile::tempdir().unwrap();
    // A fork, a request to another project, and two of ours.
    let page = gl_page("", &[(3, 42, 42), (4, 99, 42), (5, 42, 99), (6, 42, 42)]);
    let programs = fake(dir.path(), &page, 0);
    let b = binding(ForgeKind::Gitlab);
    let hidden = Vec::new();
    let forge = forge_of(&programs, &hidden, &b);
    let got = pr_find(&forge, Some(42), "fd/a/b", "main", WAIT)
        .await
        .unwrap();
    assert_eq!(numbers(got), [3, 6]);
    let seen = calls(dir.path());
    assert!(
        seen[0].contains(
            "projects/acme%2Fapp/merge_requests?state=opened&source_branch=fd%2Fa%2Fb&target_branch=main&per_page=100&page=1"
        ),
        "{seen:?}"
    );
    // A merge request without the project ids is no match; a lookup without
    // the project id is an error.
    fs::write(
        dir.path().join("answer"),
        "HTTP/2.0 200 OK\r\n\r\n[{\"iid\":3,\"web_url\":\"u\"}]",
    )
    .unwrap();
    let got = pr_find(&forge, Some(42), "fd/a/b", "main", WAIT)
        .await
        .unwrap();
    assert_eq!(numbers(got), Vec::<u64>::new());
    assert!(pr_find(&forge, None, "fd/a/b", "main", WAIT).await.is_err());
}

#[tokio::test]
async fn a_gitlab_fork_request_is_no_match_and_the_match_on_page_two_is_found() {
    let dir = tempfile::tempdir().unwrap();
    let programs = fake(dir.path(), "", 0);
    fs::write(dir.path().join("answer.1"), gl_page("2", &[(1, 99, 42)])).unwrap();
    fs::write(dir.path().join("answer.2"), gl_page("", &[(2, 42, 42)])).unwrap();
    let b = binding(ForgeKind::Gitlab);
    let hidden = Vec::new();
    let forge = forge_of(&programs, &hidden, &b);
    let got = pr_find(&forge, Some(42), "fd/a/b", "main", WAIT)
        .await
        .unwrap();
    assert_eq!(numbers(got), [2]);
    let seen = calls(dir.path());
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[1].ends_with("per_page=100&page=2"), "{seen:?}");
}

#[tokio::test]
async fn the_gitlab_project_id_is_read_from_the_project_path() {
    let dir = tempfile::tempdir().unwrap();
    let programs = fake(
        dir.path(),
        "HTTP/2.0 200 OK\r\n\r\n{\"id\":42,\"path\":\"app\"}",
        0,
    );
    let hidden = Vec::new();
    let gitlab = binding(ForgeKind::Gitlab);
    let forge = forge_of(&programs, &hidden, &gitlab);
    assert_eq!(pr_project_id(&forge, WAIT).await.unwrap(), Some(42));
    assert_eq!(
        calls(dir.path()),
        ["api --include -X GET projects/acme%2Fapp"]
    );
    // GitHub has no such id and makes no call.
    let github = binding(ForgeKind::Github);
    let forge = forge_of(&programs, &hidden, &github);
    assert_eq!(pr_project_id(&forge, WAIT).await.unwrap(), None);
    assert_eq!(calls(dir.path()).len(), 1);
    // An answer that is not a project is an error.
    for (answer, exit) in [
        ("HTTP/2.0 404 Not Found\r\n\r\n{\"message\":\"404\"}", 1),
        ("HTTP/2.0 200 OK\r\n\r\n{\"x\":1}", 0),
    ] {
        let programs = fake(dir.path(), answer, exit);
        let forge = forge_of(&programs, &hidden, &gitlab);
        assert!(pr_project_id(&forge, WAIT).await.is_err(), "{answer}");
    }
}

#[tokio::test]
async fn a_lookup_that_fails_gives_no_answer() {
    let dir = tempfile::tempdir().unwrap();
    let b = binding(ForgeKind::Github);
    let hidden = Vec::new();
    for (answer, exit) in [
        ("HTTP/2.0 502 Bad Gateway\r\n\r\nbad", 1),
        ("HTTP/2.0 200 OK\r\n\r\n{\"a\":1}", 0),
        ("not http", 1),
    ] {
        let programs = fake(dir.path(), answer, exit);
        let forge = forge_of(&programs, &hidden, &b);
        assert!(
            pr_find(&forge, None, "h", "main", WAIT).await.is_err(),
            "{answer}"
        );
    }
}

#[tokio::test]
async fn a_create_is_classified_from_the_http_status() {
    let dir = tempfile::tempdir().unwrap();
    let b = binding(ForgeKind::Github);
    let hidden = Vec::new();
    let cases = [
        (
            "HTTP/2.0 201 Created\r\n\r\n{\"number\":9,\"html_url\":\"https://x/pull/9\"}",
            0,
            "applied",
        ),
        (
            "HTTP/2.0 422 Unprocessable\r\n\r\n{\"message\":\"Validation Failed\"}",
            1,
            "rejected",
        ),
        ("HTTP/2.0 422 Unprocessable\r\n\r\n", 1, "unknown"),
        ("HTTP/2.0 502 Bad Gateway\r\n\r\nbad gateway", 1, "unknown"),
        ("HTTP/2.0 201 Created\r\n\r\n{}", 0, "unknown"),
        ("no status line", 1, "unknown"),
    ];
    for (answer, exit, want) in cases {
        let programs = fake(dir.path(), answer, exit);
        let forge = forge_of(&programs, &hidden, &b);
        let got = pr_create_direct(&forge, "fd/a/b", "main", "t", "body", WAIT).await;
        let name = match got {
            PrCreate::Applied(pr) => {
                assert_eq!(pr.number, 9);
                "applied"
            }
            PrCreate::Rejected(_) => "rejected",
            PrCreate::Unknown(_) => "unknown",
        };
        assert_eq!(name, want, "{answer}");
    }
}

#[tokio::test]
async fn a_slow_forge_is_unknown_after_the_time_limit() {
    let dir = tempfile::tempdir().unwrap();
    let programs = fake(dir.path(), "", 0);
    fs::write(dir.path().join("forge-cli"), "#!/bin/sh\nsleep 5\n").unwrap();
    let b = binding(ForgeKind::Github);
    let hidden = Vec::new();
    let forge = forge_of(&programs, &hidden, &b);
    let got = pr_create_direct(&forge, "h", "main", "t", "b", Duration::from_millis(200)).await;
    assert!(matches!(got, PrCreate::Unknown(_)), "{got:?}");
}
