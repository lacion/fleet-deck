// Tests of `remote_identity` and the rewrite rules (design §3.8, §5.10).
use super::*;

fn id(url: &str) -> RemoteIdentity {
    remote_identity(url, LocalRemotes::Deny).unwrap_or_else(|e| panic!("{url}: {e}"))
}

fn err(url: &str, local: LocalRemotes) -> IdentityError {
    remote_identity(url, local).unwrap_err()
}

#[test]
fn login_names_and_path_forms_give_distinct_dests_and_one_repo() {
    let a = id("alice@host:repo");
    let b = id("bob@host:repo");
    let c = id("alice@host:/repo");
    assert_eq!(a.dest, "ssh://alice@host:22/~/repo");
    assert_eq!(b.dest, "ssh://bob@host:22/~/repo");
    assert_eq!(c.dest, "ssh://alice@host:22/repo");
    assert_eq!(a.repo, "host/repo");
    assert_eq!(b.repo, "host/repo");
    assert_eq!(c.repo, "host/repo");
    assert_ne!(a.dest, b.dest);
    assert_ne!(a.dest, c.dest);
}

#[test]
fn the_relative_ssh_forms_give_one_dest() {
    let one = id("host:repo");
    assert_eq!(one.dest, "ssh://host:22/~/repo");
    assert_eq!(id("host:~/repo"), one);
    assert_eq!(id("ssh://host/~/repo"), one);
    assert_eq!(id("alice@h:repo").dest, id("ssh://alice@h/~/repo").dest);
    assert_eq!(id("alice@h:repo").dest, "ssh://alice@h:22/~/repo");
}

#[test]
fn a_tilde_user_path_keeps_its_form() {
    let tilde = id("host:~carol/repo");
    assert_eq!(tilde.dest, "ssh://host:22/~carol/repo");
    assert_eq!(tilde.repo, "host/carol/repo");
    assert_eq!(id("ssh://host/~carol/repo"), tilde);
}

#[test]
fn the_github_forms_are_two_destinations_of_one_repo() {
    let scp = id("git@github.com:acme/app.git");
    let url = id("ssh://git@github.com/acme/app.git");
    assert_eq!(scp.dest, "https://github.com:443/acme/app");
    assert_eq!(url.dest, "https://github.com:443/acme/app");
    assert_eq!(scp.repo, "github.com/acme/app");
    assert_eq!(url.repo, "github.com/acme/app");
}

#[test]
fn a_forge_https_dest_and_repo_drop_the_final_dot_git() {
    let with = id("https://github.com/acme/app.git");
    let without = id("https://github.com/acme/app");
    assert_eq!(with.dest, "https://github.com:443/acme/app");
    assert_eq!(without.dest, "https://github.com:443/acme/app");
    assert_eq!(with.dest, without.dest);
    assert_eq!(with.repo, "github.com/acme/app");
    assert_eq!(without.repo, with.repo);
    // One trailing slash goes, for https only.
    assert_eq!(id("https://h/a/").dest, "https://h:443/a");
    assert_eq!(id("ssh://h/a/").dest, "ssh://h:22/a/");
    assert_eq!(id("ssh://h/a/").repo, "h/a");
}

#[test]
fn the_host_is_lower_case_and_the_port_is_kept() {
    assert_eq!(id("https://GitHub.COM/a").dest, "https://github.com:443/a");
    assert_eq!(id("ssh://h:2222/a").dest, "ssh://h:2222/a");
    assert_eq!(id("ssh://h:2222/a").repo, "h/a");
    assert_eq!(
        id("ssh://git@[2001:DB8::1]:2222/a").dest,
        "ssh://git@[2001:db8::1]:2222/a"
    );
    assert_eq!(id("git@[::1]:repo").dest, "ssh://git@[::1]:22/~/repo");
}

#[test]
fn credentials_are_refused() {
    for url in [
        "https://user:tok@h/a",
        "https://user@h/a",
        "ssh://user:pw@h/a",
    ] {
        assert_eq!(
            err(url, LocalRemotes::Deny),
            IdentityError::RemoteHasCredentials,
            "{url}"
        );
    }
}

#[test]
fn a_bad_host_or_port_is_refused() {
    for url in [
        "https://h\u{e9}llo/a",
        "https://:443/a",
        "https://h:0/a",
        "https://h:abc/a",
        "https://h:99999/a",
        "-oProxyCommand=x:path",
        "ssh://[::1/a",
    ] {
        assert_eq!(
            err(url, LocalRemotes::Deny),
            IdentityError::RemoteHostInvalid,
            "{url}"
        );
    }
}

#[test]
fn a_bad_path_is_refused() {
    for url in [
        "https://h",
        "https://h/",
        "https://h/a/../b",
        "ssh://h/..",
        "h:../x",
    ] {
        assert_eq!(
            err(url, LocalRemotes::Deny),
            IdentityError::RemotePathInvalid,
            "{url}"
        );
    }
}

#[test]
fn an_unknown_form_is_unsupported() {
    for url in [
        "",
        "git://h/a",
        "http://h/a",
        "ext::sh -c x",
        "ext::x",
        "plain",
        "h: x",
    ] {
        assert_eq!(
            err(url, LocalRemotes::Deny),
            IdentityError::RemoteUnsupported {
                escaped_path: false
            },
            "{url}"
        );
    }
}

#[test]
fn an_escaped_path_is_refused_in_every_form() {
    let escaped = IdentityError::RemoteUnsupported { escaped_path: true };
    for url in [
        "alice@h:repo%2Fother",
        "ssh://alice@h/~/repo%2Fother",
        "https://h/a%2Fb.git",
        "ssh://h/a%41",
    ] {
        assert_eq!(err(url, LocalRemotes::Deny), escaped, "{url}");
    }
    for url in ["file:///srv/a%2Fb.git", "/srv/a%2Fb.git"] {
        assert_eq!(err(url, LocalRemotes::Allow), escaped, "{url}");
    }
    assert_eq!(escaped.code(), "remote_unsupported");
    assert_eq!(
        escaped.to_string(),
        "This remote URL has an escaped path. Use a plain path."
    );
}

#[test]
fn a_local_remote_needs_allow() {
    for url in ["file:///srv/a.git", "/srv/a.git"] {
        assert_eq!(
            err(url, LocalRemotes::Deny),
            IdentityError::RemoteLocalRefused,
            "{url}"
        );
    }
    let got = remote_identity("file:///srv/a.git", LocalRemotes::Allow).unwrap();
    assert_eq!(got.dest, "file:///srv/a.git");
    assert_eq!(got.repo, "localhost/srv/a.git");
    assert_eq!(
        remote_identity("/srv/a.git", LocalRemotes::Allow).unwrap(),
        got
    );
}

#[test]
fn a_local_path_is_checked_by_syntax_only() {
    for url in [
        "file://srv/a",
        "file:///srv/../a",
        "file:///srv//a",
        "/srv/./a",
    ] {
        assert_eq!(
            err(url, LocalRemotes::Allow),
            IdentityError::RemotePathInvalid,
            "{url}"
        );
    }
}

#[test]
fn each_error_has_its_wire_code_and_a_text() {
    let all = [
        (
            IdentityError::RemoteHasCredentials,
            "remote_has_credentials",
        ),
        (IdentityError::RemoteLocalRefused, "remote_local_refused"),
        (IdentityError::RemotePathInvalid, "remote_path_invalid"),
        (IdentityError::RemoteHostInvalid, "remote_host_invalid"),
        (
            IdentityError::RemoteUnsupported {
                escaped_path: false,
            },
            "remote_unsupported",
        ),
    ];
    for (error, code) in all {
        assert_eq!(error.code(), code);
        assert!(!error.to_string().is_empty());
    }
}

const RULES: &str = "\
url.git@github.com:.insteadof https://github.com/
url.ssh://git@host/.pushinsteadof https://host/
url.ssh://git@host/team/.pushInsteadOf https://host/team/
url.https://mirror.example/x/.insteadof https://up.example/x/
core.editor vim
url.https://empty/.insteadof
";

#[test]
fn the_rewrite_rules_are_parsed_from_git_config_output() {
    let rules = parse_rewrite_rules(RULES);
    assert_eq!(rules.len(), 4, "{rules:?}");
    assert_eq!(
        rules[0],
        RewriteRule {
            base: "git@github.com:".into(),
            prefix: "https://github.com/".into(),
            push: false
        }
    );
    assert!(rules[1].push && rules[2].push && !rules[3].push);
    assert_eq!(rules[3].base, "https://mirror.example/x/");
}

#[test]
fn the_longest_prefix_wins_and_a_push_rule_beats_instead_of() {
    let rules = parse_rewrite_rules(RULES);
    // A push uses the longest pushInsteadOf match.
    assert_eq!(
        effective_url("https://host/team/app", &rules, true),
        "ssh://git@host/team/app"
    );
    assert_eq!(
        effective_url("https://host/other/app", &rules, true),
        "ssh://git@host/other/app"
    );
    // A fetch ignores pushInsteadOf.
    assert_eq!(
        effective_url("https://host/team/app", &rules, false),
        "https://host/team/app"
    );
    // A push with only insteadOf matches uses insteadOf.
    assert_eq!(
        effective_url("https://github.com/acme/app", &rules, true),
        "git@github.com:acme/app"
    );
    assert_eq!(
        effective_url("https://github.com/acme/app", &rules, false),
        "git@github.com:acme/app"
    );
    // No match leaves the URL alone.
    assert_eq!(
        effective_url("https://other/x", &rules, true),
        "https://other/x"
    );
    assert_eq!(
        effective_url("https://other/x", &[], false),
        "https://other/x"
    );
}

#[test]
fn among_insteadof_rules_the_longest_prefix_wins() {
    let rules =
        parse_rewrite_rules("url.A/.insteadof https://h/\nurl.B/.insteadof https://h/team/\n");
    assert_eq!(effective_url("https://h/team/x", &rules, false), "B/x");
    assert_eq!(effective_url("https://h/x", &rules, false), "A/x");
}

#[test]
fn a_rewritten_url_can_change_the_dest() {
    let rules =
        parse_rewrite_rules("url.https://evil.example/.pushinsteadof https://wip.example/\n");
    let url = "https://wip.example/acme/app.git";
    let effective = effective_url(url, &rules, true);
    assert_ne!(id(&effective).dest, id(url).dest);
}

const FORGE_URLS: [&str; 5] = [
    "git@github.com:Lacion/Fleet-Deck.git",
    "ssh://git@github.com/lacion/fleet-deck",
    "ssh://git@github.com:22/~/lacion/fleet-deck.git/",
    "https://github.com/lacion/fleet-deck",
    "https://GitHub.com/lacion/fleet-deck.git",
];

#[test]
fn forge_urls_of_one_repository_share_one_dest() {
    for url in FORGE_URLS {
        assert_eq!(
            id(url).dest,
            "https://github.com:443/lacion/fleet-deck",
            "{url}"
        );
    }
}

#[test]
fn a_forge_dest_is_a_fixed_point() {
    for url in FORGE_URLS {
        let dest = id(url).dest;
        assert_eq!(id(&dest).dest, dest, "{url}");
    }
}

#[test]
fn other_logins_ports_and_hosts_keep_the_strict_dest() {
    let cases = [
        (
            "alice@github.com:lacion/fleet-deck",
            "ssh://alice@github.com:22/~/lacion/fleet-deck",
        ),
        (
            "ssh://git@github.com:2222/lacion/fleet-deck",
            "ssh://git@github.com:2222/lacion/fleet-deck",
        ),
        (
            "https://github.com:8443/lacion/fleet-deck",
            "https://github.com:8443/lacion/fleet-deck",
        ),
        ("https://github.com/lacion", "https://github.com:443/lacion"),
        (
            "git@gitlab.example.com:a/b",
            "ssh://git@gitlab.example.com:22/~/a/b",
        ),
    ];
    for (url, dest) in cases {
        let got = id(url).dest;
        assert!(
            !got.starts_with("https://github.com:443/lacion/fleet-deck"),
            "{url}"
        );
        assert_eq!(got, dest, "{url}");
    }
}

#[test]
fn the_forge_path_grammar_keeps_a_fixed_point() {
    // A second `.git` keeps the strict dest.
    assert_eq!(
        id("https://github.com/g/r.git.git").dest,
        "https://github.com:443/g/r.git.git"
    );
    // The final `.git` is removed without regard to case.
    assert_eq!(
        id("https://github.com/g/r.GIT").dest,
        "https://github.com:443/g/r"
    );
    // A character outside `[A-Za-z0-9._-]` and a `~` segment keep the strict dest.
    assert_eq!(
        id("https://github.com/g/r+x").dest,
        "https://github.com:443/g/r+x"
    );
    assert_eq!(
        id("ssh://git@github.com/~carol/repo").dest,
        "ssh://git@github.com:22/~carol/repo"
    );
    for url in [
        "https://github.com/g/r.git.git",
        "https://github.com/g/r.GIT",
        "https://github.com/g/r+x",
        "https://gitlab.com/a/b/c.git",
        "ssh://git@github.com/g.git/r",
    ] {
        let dest = id(url).dest;
        assert_eq!(id(&dest).dest, dest, "{url}");
    }
}

#[test]
fn forge_aliases_share_one_repo_and_one_dest() {
    let a = id("git@github.com:Acme/App.git");
    let b = id("https://github.com/acme/app");
    assert_eq!(a.repo, "github.com/acme/app");
    assert_eq!(a, b);
    // Another host keeps the repo of today's rule.
    assert_eq!(
        id("git@gitlab.example.com:Acme/App.git").repo,
        "gitlab.example.com/Acme/App"
    );
}

#[test]
fn same_dest_reads_an_old_strict_dest_as_the_canonical_one() {
    assert!(same_dest(
        "ssh://git@github.com:22/~/acme/app.git",
        "https://github.com:443/acme/app"
    ));
    assert!(same_dest(
        "https://github.com:443/acme/app.git",
        "https://github.com:443/acme/app"
    ));
    assert!(same_dest("file:///srv/a.git", "file:///srv/a.git"));
    assert!(same_dest("ssh://host:22/~/repo", "ssh://host:22/~/repo"));
    assert!(!same_dest(
        "ssh://git@github.com:22/~/acme/app.git",
        "https://github.com:443/acme/other"
    ));
    assert!(!same_dest(
        "https://github.com:443/a/b",
        "ssh://host:22/~/a/b"
    ));
    assert!(!same_dest("not a url", "https://github.com:443/a/b"));
}
