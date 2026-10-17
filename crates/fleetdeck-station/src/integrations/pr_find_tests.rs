use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind};

use super::*;
use crate::integrations::exec::Programs;

/// A fake `gh` (and `glab`) that records its arguments and prints the file
/// `answer`, then exits with the number in the file `exit`.
fn fake(dir: &Path, answer: &str, exit: i32) -> Programs {
    let script = dir.join("forge-cli");
    fs::write(dir.join("answer"), answer).unwrap();
    fs::write(dir.join("exit"), exit.to_string()).unwrap();
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> {d}/args\ncat {d}/answer\nexit $(cat {d}/exit)\n",
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

#[tokio::test]
async fn the_lookup_is_exact_and_gives_at_most_the_two_answers() {
    let dir = tempfile::tempdir().unwrap();
    let programs = fake(
        dir.path(),
        "HTTP/2.0 200 OK\r\nX: y\r\n\r\n[{\"number\":7,\"html_url\":\"https://github.com/acme/app/pull/7\"}]",
        0,
    );
    let b = binding(ForgeKind::Github);
    let hidden = Vec::new();
    let forge = Forge {
        programs: &programs,
        hidden: &hidden,
        binding: &b,
    };
    let got = pr_find(&forge, "fd/a/b", "main", WAIT).await.unwrap();
    assert_eq!(
        got,
        vec![PrRef {
            number: 7,
            url: "https://github.com/acme/app/pull/7".to_owned()
        }]
    );
    let args = fs::read_to_string(dir.path().join("args")).unwrap();
    assert_eq!(
        args.trim(),
        "api --include -X GET repos/acme/app/pulls?state=open&head=acme%3Afd%2Fa%2Fb&base=main&per_page=2"
    );
}

#[tokio::test]
async fn a_gitlab_lookup_reads_the_merge_requests() {
    let dir = tempfile::tempdir().unwrap();
    let programs = fake(
        dir.path(),
        "HTTP/2.0 200 OK\r\n\r\n[{\"iid\":3,\"web_url\":\"https://gitlab.com/acme/app/-/merge_requests/3\"},{\"iid\":4,\"web_url\":\"u\"}]",
        0,
    );
    let b = binding(ForgeKind::Gitlab);
    let hidden = Vec::new();
    let forge = Forge {
        programs: &programs,
        hidden: &hidden,
        binding: &b,
    };
    let got = pr_find(&forge, "fd/a/b", "main", WAIT).await.unwrap();
    assert_eq!(got.iter().map(|p| p.number).collect::<Vec<_>>(), [3, 4]);
    let args = fs::read_to_string(dir.path().join("args")).unwrap();
    assert!(
        args.contains(
            "projects/acme%2Fapp/merge_requests?state=opened&source_branch=fd%2Fa%2Fb&target_branch=main&per_page=2"
        ),
        "{args}"
    );
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
        let forge = Forge {
            programs: &programs,
            hidden: &hidden,
            binding: &b,
        };
        assert!(
            pr_find(&forge, "h", "main", WAIT).await.is_err(),
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
        let forge = Forge {
            programs: &programs,
            hidden: &hidden,
            binding: &b,
        };
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
    let forge = Forge {
        programs: &programs,
        hidden: &hidden,
        binding: &b,
    };
    let got = pr_create_direct(&forge, "h", "main", "t", "b", Duration::from_millis(200)).await;
    assert!(matches!(got, PrCreate::Unknown(_)), "{got:?}");
}
