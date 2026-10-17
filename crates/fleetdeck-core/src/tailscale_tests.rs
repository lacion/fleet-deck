#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use super::*;

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tailscale")
        .join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn sock(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

fn parse_message(bytes: &[u8]) -> String {
    match parse_status(bytes).unwrap_err() {
        TailscaleError::Parse(m) => m,
        other => panic!("not a parse error: {other:?}"),
    }
}

#[test]
fn a_full_status_has_this_machine_and_every_peer() {
    let s = parse_status(&fixture("status_full.json")).unwrap();
    assert_eq!(s.this.host_name, "deck-box");
    assert_eq!(s.this.dns_name, "deck-box.tail1234.ts.net");
    assert_eq!(s.this.ips, [ip("100.64.0.10"), ip("fd7a:115c:a1e0::a")]);
    assert_eq!(s.this.login.as_deref(), Some("owner@example.com"));
    assert!(s.this.online && s.this.tags.is_empty());
    assert_eq!(s.magic_dns_suffix.as_deref(), Some("tail1234.ts.net"));
    assert_eq!(s.current_tailnet.as_deref(), Some("owner@example.com"));
    assert_eq!(s.peers.len(), 3);
    let laptop = &s.peers[0];
    assert_eq!(laptop.host_name, "laptop");
    assert_eq!(laptop.dns_name, "laptop.tail1234.ts.net");
    assert_eq!(laptop.ips, [ip("100.64.0.11"), ip("fd7a:115c:a1e0::b")]);
    assert_eq!(laptop.os.as_deref(), Some("macOS"));
    assert!(laptop.online && laptop.tags.is_empty());
    // A tagged peer that is offline, with the login name of its pseudo user.
    let build = &s.peers[1];
    assert_eq!(build.tags, ["tag:ci", "tag:servers"]);
    assert!(!build.online);
    assert_eq!(build.login.as_deref(), Some("tagged-devices"));
    // A peer from another user, without a DNS name or an OS.
    let friend = &s.peers[2];
    assert_eq!(friend.login.as_deref(), Some("friend@example.org"));
    assert_eq!(friend.dns_name, "");
    assert_eq!(friend.os, None);
    assert!(!friend.online, "a missing Online is offline");
}

#[test]
fn an_ipv6_only_status_without_peers_parses() {
    let s = parse_status(&fixture("status_ipv6_only.json")).unwrap();
    assert_eq!(s.this.ips, [ip("fd7a:115c:a1e0::1234")]);
    assert!(s.peers.is_empty());
    assert_eq!(s.magic_dns_suffix.as_deref(), Some("tail1234.ts.net"));
    assert_eq!(s.current_tailnet, None, "a document without a Name");
    assert_eq!(s.this.login, None);
}

#[test]
fn a_backend_that_is_not_running_is_not_running() {
    assert_eq!(
        parse_status(&fixture("status_stopped.json")).unwrap_err(),
        TailscaleError::NotRunning
    );
    let starting = br#"{"BackendState":"Starting"}"#;
    assert_eq!(
        parse_status(starting).unwrap_err(),
        TailscaleError::NotRunning
    );
}

#[test]
fn a_duplicate_key_is_refused_at_any_depth() {
    for name in [
        "status_duplicate_top.json",
        "status_duplicate_nested.json",
        "status_duplicate_peer.json",
    ] {
        let message = parse_message(&fixture(name));
        assert!(message.contains("duplicate key"), "{name}: {message}");
    }
    let whois = br#"{"Node":{"Name":"a","Name":"b"}}"#;
    assert!(matches!(
        parse_whois(whois).unwrap_err(),
        TailscaleError::Parse(m) if m.contains("duplicate key")
    ));
}

#[test]
fn malformed_and_incomplete_documents_are_parse_errors() {
    let cases: &[&[u8]] = &[
        b"",
        b"not json",
        b"[]",
        b"{}",
        b"{\"BackendState\":\"Running\"}",
        // `Self` without addresses, with empty addresses, without a DNS name.
        br#"{"BackendState":"Running","Self":{"DNSName":"a.t.ts.net."}}"#,
        br#"{"BackendState":"Running","Self":{"DNSName":"a.t.ts.net.","TailscaleIPs":[]}}"#,
        br#"{"BackendState":"Running","Self":{"TailscaleIPs":["100.64.0.1"]}}"#,
        // A peer without a host name or addresses.
        br#"{"BackendState":"Running","Self":{"DNSName":"a.","TailscaleIPs":["100.64.0.1"]},"Peer":{"k":{"TailscaleIPs":["100.64.0.2"]}}}"#,
        br#"{"BackendState":"Running","Self":{"DNSName":"a.","TailscaleIPs":["100.64.0.1"]},"Peer":{"k":{"HostName":"x"}}}"#,
        // A wrong type, an address that is not an address.
        br#"{"BackendState":1}"#,
        br#"{"BackendState":"Running","Self":{"DNSName":"a.","TailscaleIPs":["nope"]}}"#,
        br#"{"BackendState":"Running","Self":{"DNSName":"a.","TailscaleIPs":["100.64.0.1"],"Tags":"tag:x"}}"#,
    ];
    for case in cases {
        let text = String::from_utf8_lossy(case);
        assert!(
            matches!(parse_status(case), Err(TailscaleError::Parse(_))),
            "{text}"
        );
    }
}

#[test]
fn an_address_outside_the_tailscale_range_is_refused() {
    let message = parse_message(&fixture("status_public_ip.json"));
    assert!(message.contains("outside the tailnet"), "{message}");
    let lan =
        br#"{"BackendState":"Running","Self":{"DNSName":"a.","TailscaleIPs":["192.168.1.5"]}}"#;
    assert!(matches!(parse_status(lan), Err(TailscaleError::Parse(_))));
    let peer = br#"{"BackendState":"Running","Self":{"DNSName":"a.","TailscaleIPs":["100.64.0.1"]},"Peer":{"k":{"HostName":"x","TailscaleIPs":["10.0.0.9"]}}}"#;
    assert!(matches!(parse_status(peer), Err(TailscaleError::Parse(_))));
}

#[test]
fn unknown_fields_are_ignored_and_a_trailing_dot_is_dropped() {
    let doc = br#"{"BackendState":"Running","Unknown":{"a":[1,2,null]},
        "Self":{"DNSName":"a.t.ts.net.","TailscaleIPs":["100.64.0.1"],"Future":true}}"#;
    let s = parse_status(doc).unwrap();
    assert_eq!(s.this.dns_name, "a.t.ts.net");
    let root = br#"{"BackendState":"Running","Self":{"DNSName":"a.t.ts.net..","TailscaleIPs":["100.64.0.1"]}}"#;
    assert_eq!(parse_status(root).unwrap().this.dns_name, "a.t.ts.net");
}

#[test]
fn a_whois_of_a_user_node_and_of_a_shared_tagged_node() {
    let w = parse_whois(&fixture("whois_user.json")).unwrap();
    assert_eq!(w.name, "laptop.tail1234.ts.net");
    assert_eq!(w.addresses, [ip("100.64.0.11"), ip("fd7a:115c:a1e0::b")]);
    assert_eq!(w.login.as_deref(), Some("owner@example.com"));
    assert!(w.tags.is_empty());
    assert_eq!(
        w.shared, None,
        "a missing field is unknown, never not shared"
    );
    let w = parse_whois(&fixture("whois_tagged_shared.json")).unwrap();
    assert_eq!(w.tags, ["tag:ci"]);
    assert_eq!(w.shared, Some(true));
    assert_eq!(w.login.as_deref(), Some("tagged-devices"));
    assert!(matches!(parse_whois(b"{}"), Err(TailscaleError::Parse(_))));
    assert!(matches!(
        parse_whois(br#"{"Node":{"Name":"a","Addresses":["x/32"]}}"#),
        Err(TailscaleError::Parse(_))
    ));
}

#[test]
fn the_reason_texts_are_short_and_fixed() {
    assert_eq!(TailscaleError::NotInstalled.reason(), "not installed");
    assert_eq!(TailscaleError::NotRunning.reason(), "not running");
    assert_eq!(TailscaleError::Timeout.reason(), "timeout");
    assert_eq!(TailscaleError::TooLarge.reason(), "output too large");
    assert_eq!(TailscaleError::Exit(1).reason(), "exit status 1");
    assert!(
        TailscaleError::Parse("x".into())
            .reason()
            .starts_with("parse error: ")
    );
    let long = parse_error("é".repeat(1000));
    assert!(long.reason().chars().count() < 200);
    assert_eq!(parse_error("a\nb\u{7}").reason(), "parse error: ab");
}

// ---------------------------------------------------------------- program

#[test]
fn the_setting_wins_then_path_then_the_bundled_program() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let on_path = bin.join("tailscale");
    fs::write(&on_path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&on_path, fs::Permissions::from_mode(0o755)).unwrap();
    let bundled = dir.path().join("Tailscale");
    fs::write(&bundled, "#!/bin/sh\n").unwrap();
    let path_var = std::env::join_paths([Path::new("/nonexistent-dir"), &bin]).unwrap();

    let set = Path::new("/opt/ts/tailscale");
    assert_eq!(
        select_program(Some(set), Some(&path_var), Some(&bundled)),
        set
    );
    assert_eq!(
        select_program(None, Some(&path_var), Some(&bundled)),
        on_path
    );
    // Not on PATH: the bundled program of the macOS app.
    let empty = std::env::join_paths([Path::new("/nonexistent-dir")]).unwrap();
    assert_eq!(select_program(None, Some(&empty), Some(&bundled)), bundled);
    // A bundled program that does not exist, or no macOS: the bare name.
    let missing = dir.path().join("missing");
    assert_eq!(
        select_program(None, Some(&empty), Some(&missing)),
        Path::new("tailscale")
    );
    assert_eq!(select_program(None, None, None), Path::new("tailscale"));
    // A file on PATH without the execute bit is not used.
    fs::set_permissions(&on_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        select_program(None, Some(&path_var), None),
        Path::new("tailscale")
    );
    // A relative PATH entry is not used.
    let relative = std::env::join_paths([Path::new("bin")]).unwrap();
    assert_eq!(
        select_program(None, Some(&relative), None),
        Path::new("tailscale")
    );
}

#[test]
fn the_child_environment_has_only_the_allowlist() {
    let parent = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    };
    let canary = "canary-credential-value";
    let vars = parent(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/home/o"),
        ("LANG", "C.UTF-8"),
        ("LC_ALL", "C"),
        ("SSH_AUTH_SOCK", "/tmp/agent"),
        ("ANTHROPIC_API_KEY", canary),
        ("AWS_SECRET_ACCESS_KEY", canary),
        ("TAILSCALE_AUTHKEY", canary),
        ("TS_AUTHKEY", canary),
        ("GITHUB_TOKEN", canary),
    ]);
    let names = |env: &ChildEnv| -> Vec<String> {
        env.names()
            .map(|n| n.to_string_lossy().into_owned())
            .collect()
    };
    let env = child_env(vars.clone(), false);
    assert_eq!(names(&env), ["HOME", "LANG", "PATH"]);
    assert!(
        !format!("{env:?}").contains(canary) && env.iter().all(|(_, v)| v != OsStr::new(canary))
    );
    let env = child_env(vars, true);
    assert_eq!(names(&env), ["HOME", "LANG", "PATH", "TAILSCALE_BE_CLI"]);
    assert_eq!(env.get("TAILSCALE_BE_CLI"), Some(OsStr::new("1")));
    assert!(child_env(Vec::new(), false).is_empty());
}

// ----------------------------------------------------------------- runner

/// A fake `tailscale` program: a shell script with mode 0755.
fn fake(dir: &Path, name: &str, body: &str) -> TailscaleCli {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    TailscaleCli::new(path)
}

/// `status()` again while a parallel `fork` still holds the new script open
/// for writing (`ETXTBSY`).
async fn status_of(cli: &TailscaleCli) -> Result<Status, TailscaleError> {
    let mut last = cli.status().await;
    for _ in 0..20 {
        if !matches!(&last, Err(TailscaleError::Spawn(_))) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        last = cli.status().await;
    }
    last
}

#[tokio::test]
async fn a_fake_tailscale_answers_status_and_whois() {
    let dir = tempfile::tempdir().unwrap();
    let status = dir.path().join("status.json");
    fs::write(&status, fixture("status_full.json")).unwrap();
    let whois = dir.path().join("whois.json");
    fs::write(&whois, fixture("whois_user.json")).unwrap();
    let args = dir.path().join("args");
    let body = format!(
        "echo \"$@\" >> '{}'\ncase \"$1\" in status) cat '{}';; whois) cat '{}';; esac",
        args.display(),
        status.display(),
        whois.display()
    );
    let cli = fake(dir.path(), "tailscale", &body);
    let s = status_of(&cli).await.unwrap();
    assert_eq!(s.peers.len(), 3);
    let w = cli.whois(ip("100.64.0.11")).await.unwrap();
    assert_eq!(w.login.as_deref(), Some("owner@example.com"));
    let calls = fs::read_to_string(&args).unwrap();
    assert_eq!(calls, "status --json\nwhois --json 100.64.0.11\n");
}

#[tokio::test]
async fn a_program_that_is_missing_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let cli = TailscaleCli::new(dir.path().join("nope"));
    assert_eq!(
        cli.status().await.unwrap_err(),
        TailscaleError::NotInstalled
    );
}

#[tokio::test]
async fn a_nonzero_exit_is_an_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let cli = fake(dir.path(), "tailscale", "echo oops >&2\nexit 1");
    assert_eq!(status_of(&cli).await.unwrap_err(), TailscaleError::Exit(1));
}

#[tokio::test]
async fn a_slow_program_times_out_and_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let cli = fake(
        dir.path(),
        "tailscale",
        &format!("echo $$ > '{}'\nexec sleep 30", pid.display()),
    );
    let started = std::time::Instant::now();
    assert_eq!(status_of(&cli).await.unwrap_err(), TailscaleError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(10));
    // The child is gone: reaped, not a zombie and not running.
    let pid: i32 = fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
    let alive = fs::metadata(format!("/proc/{pid}")).is_ok();
    assert!(!alive, "the child {pid} is still there");
}

#[tokio::test]
async fn too_much_stdout_or_stderr_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let big = fake(dir.path(), "big", "head -c 3000000 /dev/zero");
    assert_eq!(status_of(&big).await.unwrap_err(), TailscaleError::TooLarge);
    let noisy = fake(
        dir.path(),
        "noisy",
        "head -c 100000 /dev/zero >&2\necho '{}'",
    );
    assert_eq!(
        status_of(&noisy).await.unwrap_err(),
        TailscaleError::TooLarge
    );
    // Just under the stdout limit is read, then refused as a parse error.
    let ok_size = fake(dir.path(), "ok_size", "head -c 2000000 /dev/zero");
    assert!(matches!(
        status_of(&ok_size).await.unwrap_err(),
        TailscaleError::Parse(_)
    ));
}

#[tokio::test]
async fn the_child_sees_only_the_allowlisted_variables() {
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("env.txt");
    let cli = fake(
        dir.path(),
        "tailscale",
        &format!(
            "env > '{}'\necho '{{\"BackendState\":\"Stopped\"}}'",
            dump.display()
        ),
    );
    assert_eq!(
        status_of(&cli).await.unwrap_err(),
        TailscaleError::NotRunning
    );
    let text = fs::read_to_string(&dump).unwrap();
    let names: BTreeSet<&str> = text
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .collect();
    // The shell adds `PWD`, `SHLVL`, `_` and `OLDPWD` by itself.
    let allowed: BTreeSet<&str> = [
        "PATH",
        "HOME",
        "LANG",
        "PWD",
        "SHLVL",
        "_",
        "OLDPWD",
        "TAILSCALE_BE_CLI",
    ]
    .into();
    let extra: Vec<_> = names.difference(&allowed).collect();
    assert!(extra.is_empty(), "variables that must not pass: {extra:?}");
    for var in std::env::vars_os().map(|(k, _)| k) {
        let name = var.to_string_lossy().into_owned();
        if !allowed.contains(name.as_str()) {
            assert!(!names.contains(name.as_str()), "{name} reached the child");
        }
    }
}

// ------------------------------------------------------------ plan_use

fn status_with(ips: &[&str], dns: &str) -> Status {
    Status {
        this: Node {
            host_name: "deck".into(),
            dns_name: dns.into(),
            ips: ips.iter().map(|i| ip(i)).collect(),
            os: None,
            login: None,
            tags: Vec::new(),
            online: true,
        },
        peers: Vec::new(),
        magic_dns_suffix: None,
        current_tailnet: None,
    }
}

const BOTH: &[&str] = &["100.64.0.10", "fd7a:115c:a1e0::a"];

#[test]
fn use_adds_the_tailnet_addresses_with_the_default_ports() {
    let s = status_with(BOTH, "deck.tail1234.ts.net");
    let plan = plan_use(&s, &[], &[], |_| true);
    assert_eq!(
        plan.listen,
        [sock("100.64.0.10:7420"), sock("[fd7a:115c:a1e0::a]:7420")]
    );
    assert_eq!(
        plan.link_listen,
        [sock("100.64.0.10:7421"), sock("[fd7a:115c:a1e0::a]:7421")]
    );
    assert_eq!(
        plan.tailnet_hostname.as_deref(),
        Some("deck.tail1234.ts.net")
    );
    assert!(!plan.userspace);
    assert_eq!(plan.usable_ips.len(), 2);
    // An IPv6 entry is bracketed in its text.
    assert_eq!(plan.listen[1].to_string(), "[fd7a:115c:a1e0::a]:7420");
}

#[test]
fn use_keeps_manual_entries_and_replaces_only_tailnet_entries() {
    let s = status_with(&["100.64.0.10"], "deck.t.ts.net");
    let listen = [
        sock("127.0.0.1:7420"),
        sock("100.64.0.99:7420"),
        sock("192.168.1.5:7420"),
        sock("[fd7a:115c:a1e0::99]:7420"),
    ];
    let link = [sock("192.168.1.5:9000"), sock("100.100.100.1:9000")];
    let plan = plan_use(&s, &listen, &link, |_| true);
    assert_eq!(
        plan.listen,
        [
            sock("127.0.0.1:7420"),
            sock("192.168.1.5:7420"),
            sock("100.64.0.10:7420")
        ]
    );
    // The port is the port of the first entry of the list, a LAN entry too.
    assert_eq!(
        plan.link_listen,
        [sock("192.168.1.5:9000"), sock("100.64.0.10:9000")]
    );
}

#[test]
fn use_takes_the_port_of_the_first_entry_and_deduplicates() {
    let s = status_with(BOTH, "deck.t.ts.net");
    let listen = [sock("100.64.0.10:8443")];
    let plan = plan_use(&s, &listen, &listen, |_| true);
    assert_eq!(
        plan.listen,
        [sock("100.64.0.10:8443"), sock("[fd7a:115c:a1e0::a]:8443")]
    );
    // A second run gives the same lists: the plan is stable.
    let again = plan_use(&s, &plan.listen, &plan.link_listen, |_| true);
    assert_eq!(again.listen, plan.listen);
    assert_eq!(again.link_listen, plan.link_listen);
    // The address of the status twice does not repeat.
    let twice = status_with(&["100.64.0.10", "100.64.0.10"], "deck.t.ts.net");
    assert_eq!(plan_use(&twice, &[], &[], |_| true).listen.len(), 1);
}

#[test]
fn use_adds_only_the_addresses_that_are_assigned_locally() {
    let s = status_with(BOTH, "deck.t.ts.net");
    let only_v4 = |i: IpAddr| i.is_ipv4();
    let plan = plan_use(&s, &[sock("[fd7a:115c:a1e0::77]:7420")], &[], only_v4);
    assert_eq!(plan.listen, [sock("100.64.0.10:7420")]);
    assert_eq!(plan.usable_ips, [ip("100.64.0.10")]);
    assert!(!plan.userspace);
}

#[test]
fn no_usable_address_is_userspace_networking() {
    let s = status_with(BOTH, "deck.t.ts.net");
    let plan = plan_use(&s, &[sock("127.0.0.1:7420")], &[], |_| false);
    assert!(plan.userspace && plan.usable_ips.is_empty());
    assert_eq!(plan.listen, [sock("127.0.0.1:7420")]);
}

#[test]
fn an_empty_or_invalid_name_leaves_the_host_name_alone() {
    for name in ["", "bad name.ts.net", "-x.ts.net", "a..b"] {
        let s = status_with(BOTH, name);
        assert_eq!(
            plan_use(&s, &[], &[], |_| true).tailnet_hostname,
            None,
            "{name:?}"
        );
    }
    let s = status_with(BOTH, "Deck.Tail.TS.net");
    assert_eq!(
        plan_use(&s, &[], &[], |_| true).tailnet_hostname.as_deref(),
        Some("deck.tail.ts.net")
    );
}

#[test]
fn loopback_is_assigned_locally_and_a_documentation_address_is_not() {
    assert!(is_assigned_locally(ip("127.0.0.1")));
    assert!(!is_assigned_locally(ip("203.0.113.77")));
}
