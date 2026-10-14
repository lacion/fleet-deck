//! "Use Tailscale automatically" at the start of the deck (tailnet design
//! 14.4, 14.5): the per-channel provenance rule, the cases that save
//! nothing, and the deck that starts with an automatic address it cannot
//! use. A fake `tailscale` script gives the status document.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_core::tailscale::{self, TailscaleCli};
use fleetdeck_core::tailscale_wsl::{HostFacts, WINDOWS_PROGRAM};
use fleetdeck_deck::serve::{RunningDeck, StartOptions};
use fleetdeck_deck::tailnet_auto::{self, TailnetStart, apply_at_start, apply_at_start_with};
use fleetdeck_link::reconnect::BackoffPolicy;
use settings_support::deck_settings;

const STATUS: &str = r#"{
  "BackendState": "Running",
  "Self": {"HostName": "deck-box", "DNSName": "deck-box.tail1234.ts.net.", "OS": "linux",
           "UserID": 1, "TailscaleIPs": ["100.64.0.10", "fd7a:115c:a1e0::a"]},
  "User": {"1": {"LoginName": "owner@example.com"}}
}"#;

fn a(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

fn script(path: &Path, body: &str) -> String {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

/// A fake program that prints `doc`. When `marker` is given, it first
/// creates that file, so that a test can tell that the program ran.
fn fake(dir: &Path, name: &str, doc: &str, marker: Option<&Path>) -> String {
    let doc_path = dir.join(format!("{name}.json"));
    std::fs::write(&doc_path, doc).unwrap();
    let touch = marker.map_or_else(String::new, |m| format!("touch '{}'\n", m.display()));
    script(
        &dir.join(name),
        &format!("{touch}cat '{}'", doc_path.display()),
    )
}

/// The deadline of a start check: 3 s from now.
fn deadline() -> Instant {
    Instant::now() + tailnet_auto::DEADLINE
}

fn all_usable(_: IpAddr) -> bool {
    true
}

fn none_usable(_: IpAddr) -> bool {
    false
}

struct Run {
    config: DeckConfig,
    start: TailnetStart,
    /// The file text after the run.
    text: String,
    before: String,
}

/// Runs the automatic start on a `deck.toml` of `toml`.
async fn run(
    dir: &Path,
    toml: &str,
    overrides: &[(String, String)],
    facts: &HostFacts,
    usable: &(dyn Fn(IpAddr) -> bool + Sync),
) -> Run {
    let settings = deck_settings(dir, toml);
    let config = DeckConfig::load(&settings.path).unwrap();
    let before = std::fs::read_to_string(&settings.path).unwrap();
    let (config, start) =
        apply_at_start(&settings, config, overrides, facts, usable, deadline()).await;
    let text = std::fs::read_to_string(&settings.path).unwrap();
    Run {
        config,
        start,
        text,
        before,
    }
}

fn head(bin: &str) -> String {
    format!("name = \"d\"\ntailnet_auto = true\ntailscale_bin = \"{bin}\"\n")
}

fn applied(start: &TailnetStart) -> bool {
    start.enabled && start.start_reason.is_none() && start.skipped.is_empty()
}

#[tokio::test]
async fn only_the_provenance_entries_of_each_channel_are_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!(
        "{}listen = [\"127.0.0.1:7420\", \"192.168.5.5:7420\", \"100.64.0.7:7420\", \"100.64.0.9:7420\"]\n\
         link_listen = [\"127.0.0.1:7421\", \"100.64.0.9:7421\"]\n\
         tailnet_auto_listen = [\"100.64.0.9:7420\"]\n\
         tailnet_auto_link_listen = [\"100.64.0.9:7421\"]\n",
        head(&bin)
    );
    let r = run(tmp.path(), &toml, &[], &HostFacts::default(), &all_usable).await;
    assert!(applied(&r.start), "{:?}", r.start);
    // The LAN entry and the manual tailnet-range entry stay; the old
    // automatic entry is replaced by the current addresses.
    assert_eq!(
        r.config.listen,
        vec![
            a("127.0.0.1:7420"),
            a("192.168.5.5:7420"),
            a("100.64.0.7:7420"),
            a("100.64.0.10:7420"),
            a("[fd7a:115c:a1e0::a]:7420"),
        ]
    );
    assert_eq!(
        r.config.link_listen,
        vec![
            a("127.0.0.1:7421"),
            a("100.64.0.10:7421"),
            a("[fd7a:115c:a1e0::a]:7421"),
        ]
    );
    assert_eq!(
        r.config.tailnet_auto_listen,
        vec![a("100.64.0.10:7420"), a("[fd7a:115c:a1e0::a]:7420")]
    );
    assert_eq!(
        r.config.tailnet_auto_link_listen,
        vec![a("100.64.0.10:7421"), a("[fd7a:115c:a1e0::a]:7421")]
    );
    assert_eq!(
        r.config.tailnet_hostname.as_deref(),
        Some("deck-box.tail1234.ts.net")
    );
    // The file holds the same, in one save.
    let saved = DeckConfig::parse(&r.text).unwrap();
    assert_eq!(saved, r.config);
    assert!(r.text.contains("tailnet_auto = true"), "{}", r.text);
    assert_ne!(r.text, r.before);
}

#[tokio::test]
async fn the_same_address_manual_in_one_channel_and_automatic_in_the_other() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    // `.9` is a manual entry of `link_listen` and an automatic one of
    // `listen`. The tailnet address changes to `.10`.
    let toml = format!(
        "{}listen = [\"127.0.0.1:7420\", \"100.64.0.9:7420\"]\n\
         link_listen = [\"127.0.0.1:7421\", \"100.64.0.9:7421\"]\n\
         tailnet_auto_listen = [\"100.64.0.9:7420\"]\n",
        head(&bin)
    );
    let r = run(tmp.path(), &toml, &[], &HostFacts::default(), &all_usable).await;
    assert!(applied(&r.start), "{:?}", r.start);
    assert_eq!(
        r.config.listen,
        vec![
            a("127.0.0.1:7420"),
            a("100.64.0.10:7420"),
            a("[fd7a:115c:a1e0::a]:7420"),
        ]
    );
    // The manual link entry stays across the IP change.
    assert_eq!(
        r.config.link_listen,
        vec![
            a("127.0.0.1:7421"),
            a("100.64.0.9:7421"),
            a("100.64.0.10:7421"),
            a("[fd7a:115c:a1e0::a]:7421"),
        ]
    );
    assert_eq!(
        r.config.tailnet_auto_link_listen,
        vec![a("100.64.0.10:7421"), a("[fd7a:115c:a1e0::a]:7421")]
    );
}

#[tokio::test]
async fn a_wanted_address_that_is_already_manual_is_not_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!(
        "{}link_listen = [\"127.0.0.1:7421\", \"100.64.0.10:7421\"]\n",
        head(&bin)
    );
    let r = run(tmp.path(), &toml, &[], &HostFacts::default(), &all_usable).await;
    assert!(applied(&r.start), "{:?}", r.start);
    assert_eq!(
        r.config.link_listen,
        vec![
            a("127.0.0.1:7421"),
            a("100.64.0.10:7421"),
            a("[fd7a:115c:a1e0::a]:7421"),
        ]
    );
    // Only the address that the mode added is automatic.
    assert_eq!(
        r.config.tailnet_auto_link_listen,
        vec![a("[fd7a:115c:a1e0::a]:7421")]
    );
}

#[tokio::test]
async fn a_second_start_with_the_same_addresses_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let first = run(
        tmp.path(),
        &head(&bin),
        &[],
        &HostFacts::default(),
        &all_usable,
    )
    .await;
    assert_ne!(first.text, first.before);
    // Run again on the file that the first run wrote.
    let settings = deck_settings(tmp.path(), &first.text);
    let config = DeckConfig::load(&settings.path).unwrap();
    let (again, start) = apply_at_start(
        &settings,
        config.clone(),
        &[],
        &HostFacts::default(),
        &all_usable,
        deadline(),
    )
    .await;
    assert!(applied(&start));
    assert_eq!(again, config);
    assert_eq!(std::fs::read_to_string(&settings.path).unwrap(), first.text);
}

#[tokio::test]
async fn a_listen_that_the_command_line_set_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!(
        "{}listen = [\"127.0.0.1:7420\", \"100.64.0.9:7420\"]\n\
         tailnet_auto_listen = [\"100.64.0.9:7420\"]\n",
        head(&bin)
    );
    let settings = deck_settings(tmp.path(), &toml);
    let mut config = DeckConfig::load(&settings.path).unwrap();
    // The command line gave `--listen`.
    config.listen = vec![a("127.0.0.1:9999")];
    let overrides = vec![("listen".to_owned(), "--listen".to_owned())];
    let (next, start) = apply_at_start(
        &settings,
        config,
        &overrides,
        &HostFacts::default(),
        &all_usable,
        deadline(),
    )
    .await;
    assert!(applied(&start), "{start:?}");
    assert_eq!(next.listen, vec![a("127.0.0.1:9999")]);
    assert_eq!(next.tailnet_auto_listen, vec![a("100.64.0.9:7420")]);
    // `link_listen` and the host name are still set.
    assert_eq!(
        next.link_listen,
        vec![
            a("127.0.0.1:7421"),
            a("100.64.0.10:7421"),
            a("[fd7a:115c:a1e0::a]:7421"),
        ]
    );
    let saved = DeckConfig::parse(&std::fs::read_to_string(&settings.path).unwrap()).unwrap();
    assert_eq!(
        saved.listen,
        vec![a("127.0.0.1:7420"), a("100.64.0.9:7420")]
    );
    assert_eq!(saved.tailnet_auto_listen, vec![a("100.64.0.9:7420")]);
}

#[tokio::test]
async fn the_mode_off_runs_nothing_and_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("ran");
    let bin = fake(tmp.path(), "tailscale", STATUS, Some(&marker));
    let toml = format!("name = \"d\"\ntailscale_bin = \"{bin}\"\n");
    let r = run(tmp.path(), &toml, &[], &HostFacts::default(), &all_usable).await;
    assert_eq!(r.start, TailnetStart::default());
    assert_eq!(r.text, r.before);
    assert!(!marker.exists(), "the program ran with the mode off");
}

/// A run that must save nothing and give `reason`.
async fn nothing_saved(
    dir: &Path,
    toml: &str,
    facts: &HostFacts,
    usable: &(dyn Fn(IpAddr) -> bool + Sync),
    reason: &str,
) {
    let r = run(dir, toml, &[], facts, usable).await;
    assert_eq!(r.start.start_reason.as_deref(), Some(reason));
    assert!(r.start.enabled && r.start.skipped.is_empty());
    assert_eq!(r.text, r.before, "the file changed");
    assert_eq!(r.config, DeckConfig::parse(&r.before).unwrap());
}

#[tokio::test]
async fn an_unavailable_tailscale_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(
        tmp.path(),
        "tailscale",
        r#"{"BackendState":"Stopped"}"#,
        None,
    );
    nothing_saved(
        tmp.path(),
        &head(&bin),
        &HostFacts::default(),
        &all_usable,
        "Tailscale is not available: not running.",
    )
    .await;
    // A program that does not exist.
    let tmp2 = tempfile::tempdir().unwrap();
    let missing = tmp2.path().join("no-such-tailscale").display().to_string();
    nothing_saved(
        tmp2.path(),
        &head(&missing),
        &HostFacts::default(),
        &all_usable,
        "Tailscale is not available: not installed.",
    )
    .await;
}

#[tokio::test]
async fn userspace_networking_saves_nothing_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    nothing_saved(
        tmp.path(),
        &head(&bin),
        &HostFacts::default(),
        &none_usable,
        "Tailscale runs in userspace networking: set the tailnet addresses by hand.",
    )
    .await;
    // On WSL the text of the design 14.2.
    let tmp2 = tempfile::tempdir().unwrap();
    let bin2 = fake(tmp2.path(), "tailscale", STATUS, None);
    let wsl = HostFacts {
        wsl: true,
        ..HostFacts::default()
    };
    nothing_saved(
        tmp2.path(),
        &head(&bin2),
        &wsl,
        &none_usable,
        tailnet_auto::WSL_NAT,
    )
    .await;
}

#[tokio::test]
async fn non_local_binding_stops_the_mode_and_runs_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("ran");
    let bin = fake(tmp.path(), "tailscale", STATUS, Some(&marker));
    let facts = HostFacts {
        nonlocal_bind: true,
        ..HostFacts::default()
    };
    nothing_saved(
        tmp.path(),
        &head(&bin),
        &facts,
        &all_usable,
        "Non-local binding is on; set the tailnet addresses by hand.",
    )
    .await;
    assert!(
        !marker.exists(),
        "the program ran with non-local binding on"
    );
}

#[tokio::test]
async fn a_tailscale_that_does_not_answer_in_3_seconds_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = script(&tmp.path().join("tailscale"), "exec sleep 20");
    let started = std::time::Instant::now();
    nothing_saved(
        tmp.path(),
        &head(&bin),
        &HostFacts::default(),
        &all_usable,
        "Tailscale did not answer within 3 seconds.",
    )
    .await;
    let took = started.elapsed();
    assert!(
        took >= std::time::Duration::from_secs(2) && took < std::time::Duration::from_secs(8),
        "{took:?}"
    );
}

#[tokio::test]
async fn a_failed_save_changes_nothing_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let settings = deck_settings(tmp.path(), &head(&bin));
    let config = DeckConfig::load(&settings.path).unwrap();
    let before = std::fs::read_to_string(&settings.path).unwrap();
    // A file that other users can write is refused by the writer.
    std::fs::set_permissions(&settings.path, std::fs::Permissions::from_mode(0o666)).unwrap();
    let (next, start) = apply_at_start(
        &settings,
        config.clone(),
        &[],
        &HostFacts::default(),
        &all_usable,
        deadline(),
    )
    .await;
    assert_eq!(next, config);
    assert_eq!(
        start.start_reason.as_deref(),
        Some("The settings could not be saved: settings_unreadable.")
    );
    assert_eq!(std::fs::read_to_string(&settings.path).unwrap(), before);
}

/// A program lookup that blocks ends at the deadline, with the timeout
/// reason and no save.
#[tokio::test]
async fn a_program_lookup_that_blocks_ends_at_the_deadline_and_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let settings = deck_settings(tmp.path(), &head(&bin));
    let config = DeckConfig::load(&settings.path).unwrap();
    let before = std::fs::read_to_string(&settings.path).unwrap();
    let started = Instant::now();
    let (next, start) = apply_at_start_with(
        &settings,
        config.clone(),
        &[],
        &HostFacts::default(),
        &all_usable,
        Instant::now() + Duration::from_millis(300),
        |configured| {
            std::thread::sleep(Duration::from_secs(5));
            TailscaleCli::new(configured.unwrap())
        },
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(next, config);
    assert!(start.enabled && start.skipped.is_empty());
    assert_eq!(
        start.start_reason.as_deref(),
        Some("Tailscale did not answer within 3 seconds.")
    );
    assert_eq!(std::fs::read_to_string(&settings.path).unwrap(), before);
}

/// A status answer that comes after the deadline is not saved.
#[tokio::test]
async fn a_status_answer_after_the_deadline_is_not_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let doc = tmp.path().join("doc.json");
    std::fs::write(&doc, STATUS).unwrap();
    let bin = script(
        &tmp.path().join("tailscale"),
        &format!("sleep 2\ncat '{}'", doc.display()),
    );
    let settings = deck_settings(tmp.path(), &head(&bin));
    let config = DeckConfig::load(&settings.path).unwrap();
    let before = std::fs::read_to_string(&settings.path).unwrap();
    let started = Instant::now();
    let (next, start) = apply_at_start(
        &settings,
        config.clone(),
        &[],
        &HostFacts::default(),
        &all_usable,
        Instant::now() + Duration::from_millis(500),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_millis(1900),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(next, config);
    assert_eq!(
        start.start_reason.as_deref(),
        Some("Tailscale did not answer within 3 seconds.")
    );
    assert_eq!(std::fs::read_to_string(&settings.path).unwrap(), before);
}

/// The Windows program is not run unless `tailscale_bin` names it.
#[tokio::test]
async fn the_windows_program_is_not_run_without_the_setting() {
    if tailscale::is_installed() {
        eprintln!("note: tailscale is on PATH of this machine; the test needs it absent");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("ran");
    let windows = fake(tmp.path(), "tailscale.exe", STATUS, Some(&marker));
    let facts = HostFacts {
        wsl: true,
        windows_program: Some(PathBuf::from(windows)),
        nonlocal_bind: false,
    };
    // No `tailscale_bin`: the program is looked up, not found, not run.
    nothing_saved(
        tmp.path(),
        "name = \"d\"\ntailnet_auto = true\n",
        &facts,
        &all_usable,
        "Tailscale is not available: not installed.",
    )
    .await;
    assert!(!marker.exists(), "the Windows program ran");
    assert_ne!(WINDOWS_PROGRAM, "");
}

fn dirs_of(root: &Path) -> NodeDirs {
    NodeDirs {
        config: root.join("config"),
        data: root.join("data"),
    }
}

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: std::time::Duration::from_millis(50),
        cap: std::time::Duration::from_millis(200),
        stable: std::time::Duration::from_secs(60),
    }
}

/// An automatic entry that is not on this machine is skipped, and the deck
/// starts with its manual listeners.
#[tokio::test]
async fn an_automatic_entry_that_is_not_local_is_skipped_and_the_deck_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs_of(tmp.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    // Not on any machine of this test: the top of the tailnet range.
    let gone_board = "100.127.254.254:7420";
    let gone_link = "100.127.254.254:7421";
    // The mode is on, with a program that does not exist: nothing is saved.
    let missing = tmp.path().join("no-such-tailscale").display().to_string();
    let toml = format!(
        "name = \"d\"\ntailnet_auto = true\ntailscale_bin = \"{missing}\"\n\
         listen = [\"127.0.0.1:0\", \"{gone_board}\"]\n\
         link_listen = [\"127.0.0.1:0\", \"{gone_link}\"]\n\
         tailnet_auto_listen = [\"{gone_board}\"]\n\
         tailnet_auto_link_listen = [\"{gone_link}\"]\n"
    );
    let path = dirs.deck_config();
    std::fs::write(&path, &toml).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = DeckConfig::load(&path).unwrap();
    let deck = RunningDeck::start(&dirs, &config, fast(), StartOptions::default())
        .await
        .expect("the deck starts although an automatic address is not local");
    // The manual listeners are up; the automatic ones are not.
    assert_eq!(deck.node.listen_addrs().len(), 1);
    assert!(deck.node.listen_addrs()[0].ip().is_loopback());
    assert_eq!(deck.node.skipped().len(), 1);
    assert_eq!(deck.node.skipped()[0].0, a(gone_link));
    assert_eq!(
        deck.node.skipped()[0].1,
        "the address is not on this machine"
    );
    assert!(deck.endpoint.is_some(), "the board listens");
    deck.stop().await;
}

/// A manual entry that cannot be bound still fails the start.
#[tokio::test]
async fn a_manual_entry_that_cannot_be_bound_still_fails_the_start() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs_of(tmp.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let toml =
        format!("name = \"d\"\nlisten = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:{port}\"]\n");
    let path = dirs.deck_config();
    std::fs::write(&path, &toml).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = DeckConfig::load(&path).unwrap();
    let err = RunningDeck::start(&dirs, &config, fast(), StartOptions::default())
        .await
        .expect_err("the start fails");
    assert!(err.to_string().contains("link"), "{err}");
}

/// An automatic entry that is local but cannot be bound is skipped too.
#[tokio::test]
async fn an_automatic_entry_that_cannot_be_bound_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs_of(tmp.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy_addr = busy.local_addr().unwrap();
    let missing = tmp.path().join("no-such-tailscale").display().to_string();
    let toml = format!(
        "name = \"d\"\ntailnet_auto = true\ntailscale_bin = \"{missing}\"\n\
         listen = [\"127.0.0.1:0\"]\n\
         link_listen = [\"127.0.0.1:0\", \"{busy_addr}\"]\n\
         tailnet_auto_link_listen = [\"{busy_addr}\"]\n"
    );
    let path = dirs.deck_config();
    std::fs::write(&path, &toml).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = DeckConfig::load(&path).unwrap();
    let deck = RunningDeck::start(&dirs, &config, fast(), StartOptions::default())
        .await
        .expect("the deck starts");
    assert_eq!(deck.node.listen_addrs().len(), 1);
    assert_eq!(deck.node.skipped().len(), 1);
    assert_eq!(deck.node.skipped()[0].0, busy_addr);
    assert_eq!(
        deck.node.skipped()[0].1,
        "the address cannot be bound: AddrInUse"
    );
    deck.stop().await;
}

/// With the mode on and a Tailscale in userspace networking, the start
/// saves nothing, and the deck starts with the saved lists.
#[tokio::test]
async fn the_deck_starts_when_the_automatic_mode_cannot_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs_of(tmp.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    // The tailnet addresses of this fake are not on this machine.
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!(
        "name = \"d\"\ntailnet_auto = true\ntailscale_bin = \"{bin}\"\n\
         listen = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:0\"]\n"
    );
    let path = dirs.deck_config();
    std::fs::write(&path, &toml).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = DeckConfig::load(&path).unwrap();
    let deck = RunningDeck::start(&dirs, &config, fast(), StartOptions::default())
        .await
        .expect("the deck starts");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), toml);
    // No audit row: nothing was saved.
    let rows = deck.store.audit_log(50).await.unwrap();
    assert!(rows.iter().all(|r| r.action != "tailnet.auto"));
    deck.stop().await;
}

/// The audit row `tailnet.auto` has the keys that the start changed, never a
/// value, and only when a save happened.
#[tokio::test]
async fn the_audit_row_has_the_changed_keys_only() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let det = fleetdeck_testkit::Det::new();
    let store = fleetdeck_deck::DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let settings = deck_settings(tmp.path(), &head(&bin));
    let config = DeckConfig::load(&settings.path).unwrap();
    // The harness settings name `listen` as a command-line value, so the
    // save leaves `listen` out.
    let overrides = vec![("listen".to_owned(), "--listen".to_owned())];
    let (next, _) = apply_at_start(
        &settings,
        config.clone(),
        &overrides,
        &HostFacts::default(),
        &all_usable,
        deadline(),
    )
    .await;
    tailnet_auto::audit(&store, &config, &next).await;
    let rows = store.audit_log(10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].actor.as_str(), rows[0].action.as_str()),
        ("deck", "tailnet.auto")
    );
    assert_eq!(rows[0].result, fleetdeck_deck::store::AuditResult::Ok);
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("link_listen,tailnet_hostname,tailnet_auto_link_listen")
    );
    let shown = format!("{rows:?}");
    assert!(!shown.contains("100.64.0.10"), "{shown}");
    // No change, no row.
    tailnet_auto::audit(&store, &next, &next).await;
    assert_eq!(store.audit_log(10).await.unwrap().len(), 1);
}

/// The board binds its optional (automatic) addresses with skipping.
#[tokio::test]
async fn the_board_skips_an_optional_address_that_is_not_local_or_is_busy() {
    let f = common::fixture().await;
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy_addr = busy.local_addr().unwrap();
    let gone = a("100.127.254.254:7420");
    let c = common::board::board_with_extras(
        std::sync::Arc::clone(&f.ctx),
        common::board::Extras {
            optional: vec![gone, busy_addr],
            ..common::board::Extras::default()
        },
    )
    .await;
    assert_eq!(c.skipped.len(), 2);
    assert_eq!(
        c.skipped[0],
        (gone, "the address is not on this machine".to_owned())
    );
    assert_eq!(
        c.skipped[1],
        (
            busy_addr,
            "the address cannot be bound: AddrInUse".to_owned()
        )
    );
    // The board answers on its manual address.
    let r = c.req("GET", "/api/tailnet", &[], None).await;
    assert_eq!(r.status, 401, "{}", r.body);
}

/// With the mode off, a provenance entry that cannot be bound fails the
/// start like a manual entry: the provenance lists are ignored.
#[tokio::test]
async fn with_the_mode_off_a_provenance_entry_that_cannot_bind_fails_the_start() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs_of(tmp.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    let gone_link = "100.127.254.254:7421";
    let toml = format!(
        "name = \"d\"\nlisten = [\"127.0.0.1:0\"]\n\
         link_listen = [\"127.0.0.1:0\", \"{gone_link}\"]\n\
         tailnet_auto_link_listen = [\"{gone_link}\"]\n"
    );
    let path = dirs.deck_config();
    std::fs::write(&path, &toml).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = DeckConfig::load(&path).unwrap();
    assert!(!config.tailnet_auto);
    let err = RunningDeck::start(&dirs, &config, fast(), StartOptions::default())
        .await
        .expect_err("the start fails");
    assert!(err.to_string().contains("link"), "{err}");
}
