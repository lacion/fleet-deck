//! The tailnet routes on WSL and with "Use Tailscale automatically"
//! (tailnet design 14 and 15): the program kind of the card, the NAT text,
//! the child kill at the deadline, the certificate fallback at start,
//! the card data of the automatic mode (on when the key is absent), the
//! provenance lists that a manual save and "Use tailnet" write, and the
//! elevation rules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::board::{BoardClient, Extras, board_with_extras};
use common::*;
use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_core::tailscale_wsl::HostFacts;
use fleetdeck_deck::http::tailnet_routes::use_with;
use fleetdeck_deck::http::tls::TlsError;
use fleetdeck_deck::serve::{RunningDeck, StartOptions};
use fleetdeck_deck::tailnet_auto::{TailnetStart, apply_at_start};
use fleetdeck_link::reconnect::BackoffPolicy;
use serde_json::{Value, json};
use settings_support::*;

const STATUS: &str = r#"{
  "BackendState": "Running",
  "Self": {"HostName": "deck-box", "DNSName": "deck-box.tail1234.ts.net.", "OS": "linux",
           "UserID": 1, "TailscaleIPs": ["100.64.0.10", "fd7a:115c:a1e0::a"]},
  "User": {"1": {"LoginName": "owner@example.com"}}
}"#;

const DECK_TOML: &str = "name = \"alpha\"\nlisten = [\"127.0.0.1:7420\", \"192.168.5.5:7420\"]\nlink_listen = [\"127.0.0.1:7421\"]\n";

/// The text of design 14.2, written out so that a change of the wording is a
/// failing test.
const NAT_TEXT: &str = "Tailscale runs on Windows and WSL uses NAT networking, so the tailnet address is not on this machine. Set `networkingMode=mirrored` in `%UserProfile%\\.wslconfig`, run `wsl --shutdown`, and start Fleet Deck again; or install Tailscale inside WSL.";

fn script(path: &Path, body: &str) -> String {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

/// A fake program that prints `doc`; with `marker` it first creates that file.
fn fake(dir: &Path, name: &str, doc: &str, marker: Option<&Path>) -> String {
    let doc_path = dir.join(format!("{name}.json"));
    std::fs::write(&doc_path, doc).unwrap();
    let touch = marker.map_or_else(String::new, |m| format!("touch '{}'\n", m.display()));
    script(
        &dir.join(name),
        &format!("{touch}cat '{}'", doc_path.display()),
    )
}

async fn board(dir: &Path, toml: &str, extras: Extras) -> (Fixture, BoardClient) {
    let f = fixture().await;
    let c = board_with_extras(
        Arc::clone(&f.ctx),
        Extras {
            settings: Some(deck_settings(dir, toml)),
            ..extras
        },
    )
    .await;
    (f, c)
}

fn deck_file(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("deck.toml")).unwrap()
}

fn wsl_only() -> HostFacts {
    HostFacts {
        wsl: true,
        ..HostFacts::default()
    }
}

/// The card reports how the program was chosen, and no Windows hint exists:
/// a set `tailscale_bin` is `configured`; a program that does not exist is
/// "not installed" and no program is chosen.
#[tokio::test]
async fn the_card_reports_the_program_kind_and_has_no_windows_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!("{DECK_TOML}tailscale_bin = \"{bin}\"\n");
    let (_f, c) = board(
        tmp.path(),
        &toml,
        Extras {
            host: Some(wsl_only),
            ..Extras::default()
        },
    )
    .await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["program"], "configured");
    assert!(r.body.get("windows_program").is_none());
    let r = get(&c, &h, "/api/tailnet/devices").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["available"], true);
    assert!(r.body.get("windows_program").is_none());

    let tmp2 = tempfile::tempdir().unwrap();
    let missing = tmp2.path().join("no-such-tailscale").display().to_string();
    let toml = format!("{DECK_TOML}tailscale_bin = \"{missing}\"\n");
    let (_f2, c2) = board(tmp2.path(), &toml, Extras::default()).await;
    let h2 = owner(&c2).await;
    let r = get(&c2, &h2, "/api/tailnet").await;
    assert_eq!(r.body["available"], false);
    assert_eq!(r.body["reason"], "not installed");
    assert!(r.body.get("program").is_none());
    assert!(r.body.get("windows_program").is_none());
}

#[tokio::test]
async fn on_wsl_with_nat_networking_the_reason_is_the_nat_text() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!("{DECK_TOML}tailscale_bin = \"{bin}\"\n");
    let (_f, c) = board(
        tmp.path(),
        &toml,
        Extras {
            host: Some(wsl_only),
            ..Extras::default()
        },
    )
    .await;
    let h = owner(&c).await;
    // The tailnet addresses of the fake are not on this machine.
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["available"], true);
    assert_eq!(r.body["userspace"], true);
    assert_eq!(r.body["reason"], NAT_TEXT);
    let before = deck_file(tmp.path());
    let r = post(&c, &h, "/api/tailnet/use", json!({})).await;
    expect(&r, 409, "tailnet_userspace");
    assert_eq!(r.body["message"], NAT_TEXT);
    assert_eq!(deck_file(tmp.path()), before);
}

#[tokio::test]
async fn the_card_data_of_the_automatic_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!("{DECK_TOML}tailscale_bin = \"{bin}\"\ntailnet_auto = true\n");
    let start = TailnetStart {
        enabled: true,
        start_reason: Some("Tailscale did not answer within 3 seconds.".to_owned()),
        skipped: vec![(
            "100.64.0.9:7420".parse().unwrap(),
            "the address is not on this machine".to_owned(),
        )],
    };
    let (_f, c) = board(
        tmp.path(),
        &toml,
        Extras {
            tailnet_start: Some(start),
            ..Extras::default()
        },
    )
    .await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(
        r.body["auto"],
        json!({
            "enabled": true,
            "start_reason": "Tailscale did not answer within 3 seconds.",
            "skipped": [{
                "address": "100.64.0.9:7420",
                "reason": "the address is not on this machine"
            }],
            "changed": false
        })
    );

    // The start facts show only when the mode was on at start.
    let tmp2 = tempfile::tempdir().unwrap();
    let bin2 = fake(tmp2.path(), "tailscale", STATUS, None);
    let toml2 = format!("{DECK_TOML}tailscale_bin = \"{bin2}\"\ntailnet_auto = true\n");
    let (_f2, c2) = board(
        tmp2.path(),
        &toml2,
        Extras {
            tailnet_start: Some(TailnetStart {
                enabled: false,
                start_reason: Some("hidden".to_owned()),
                skipped: vec![("100.64.0.9:7420".parse().unwrap(), "hidden".to_owned())],
            }),
            ..Extras::default()
        },
    )
    .await;
    let h2 = owner(&c2).await;
    let r = get(&c2, &h2, "/api/tailnet").await;
    assert_eq!(
        r.body["auto"],
        json!({ "enabled": true, "skipped": [], "changed": false })
    );

    // The saved setting, not the start fact, is `enabled`.
    let tmp3 = tempfile::tempdir().unwrap();
    let toml3 = format!("{DECK_TOML}tailnet_auto = false\n");
    let (_f3, c3) = board(tmp3.path(), &toml3, Extras::default()).await;
    let h3 = owner(&c3).await;
    let r = get(&c3, &h3, "/api/tailnet").await;
    assert_eq!(
        r.body["auto"],
        json!({ "enabled": false, "skipped": [], "changed": false })
    );
}

/// The key is absent: the mode is on (design 15.2), on the card and in the
/// settings view.
#[tokio::test]
async fn the_mode_is_on_when_the_key_is_absent() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!DECK_TOML.contains("tailnet_auto"));
    let (_f, c) = board(tmp.path(), DECK_TOML, Extras::default()).await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.body["auto"]["enabled"], true);
    let view = get(&c, &h, "/api/settings/deck").await;
    assert_eq!(view.body["values"]["tailnet_auto"], true);
}

const AUTO_TOML: &str = "name = \"alpha\"\ntailnet_auto = true\n\
    listen = [\"127.0.0.1:7420\", \"100.64.0.9:7420\", \"100.64.0.10:7420\"]\n\
    link_listen = [\"127.0.0.1:7421\", \"100.64.0.9:7421\"]\n\
    tailnet_auto_listen = [\"100.64.0.9:7420\", \"100.64.0.10:7420\"]\n\
    tailnet_auto_link_listen = [\"100.64.0.9:7421\"]\n";

async fn revision(c: &BoardClient, h: &Headers) -> String {
    let view = get(c, h, "/api/settings/deck").await;
    view.body["revision"].as_str().unwrap().to_owned()
}

async fn save(c: &BoardClient, h: &Headers, changes: Value) -> common::board::Http {
    let rev = revision(c, h).await;
    post(
        c,
        h,
        "/api/settings/deck",
        json!({ "changes": changes, "revision": rev }),
    )
    .await
}

#[tokio::test]
async fn a_manual_save_keeps_only_the_provenance_entries_that_stay() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = board(tmp.path(), AUTO_TOML, Extras::default()).await;
    let h = owner(&c).await;
    // The hidden keys are in the view, flagged.
    let view = get(&c, &h, "/api/settings/deck").await;
    let hidden: Vec<&str> = view.body["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["hidden"] == true)
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(hidden, ["tailnet_auto_listen", "tailnet_auto_link_listen"]);
    assert_eq!(
        view.body["values"]["tailnet_auto_listen"],
        json!(["100.64.0.9:7420", "100.64.0.10:7420"])
    );

    // The owner removes the entry `.9` from `listen`: it is forgotten, `.10`
    // stays automatic, and the other channel is not touched.
    let r = save(
        &c,
        &h,
        json!({ "listen": ["127.0.0.1:7420", "100.64.0.10:7420"] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["values"]["tailnet_auto_listen"],
        json!(["100.64.0.10:7420"])
    );
    assert_eq!(
        r.body["values"]["tailnet_auto_link_listen"],
        json!(["100.64.0.9:7421"])
    );
    let file = DeckFile::of(tmp.path());
    assert_eq!(file.auto_listen, ["100.64.0.10:7420"]);

    // A save of `link_listen` without the automatic entry empties its list.
    let r = save(&c, &h, json!({ "link_listen": ["127.0.0.1:7421"] })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["values"]["tailnet_auto_link_listen"], json!([]));
    assert_eq!(
        r.body["values"]["tailnet_auto_listen"],
        json!(["100.64.0.10:7420"])
    );

    // A save that sets the provenance list itself is taken as it is.
    let r = save(
        &c,
        &h,
        json!({
            "listen": ["127.0.0.1:7420", "100.64.0.10:7420"],
            "tailnet_auto_listen": []
        }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["values"]["tailnet_auto_listen"], json!([]));

    // A save of another key leaves both lists alone.
    let r = save(&c, &h, json!({ "name": "beta" })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["values"]["tailnet_auto_listen"], json!([]));
}

/// A reset of `tailnet_auto` (the change that `fleetdeck settings` sends for
/// `null`) goes back to the default, which is on: the provenance lists stay
/// (design 15.2), and no elevation is needed from an effective `true`.
#[tokio::test]
async fn resetting_the_mode_to_the_default_keeps_both_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = board(tmp.path(), AUTO_TOML, Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let r = save(&c, &h, json!({ "tailnet_auto": null })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["values"]["tailnet_auto"], true);
    assert_eq!(
        r.body["values"]["tailnet_auto_listen"],
        json!(["100.64.0.9:7420", "100.64.0.10:7420"])
    );
    assert_eq!(
        r.body["values"]["tailnet_auto_link_listen"],
        json!(["100.64.0.9:7421"])
    );
    assert!(!deck_file(tmp.path()).contains("tailnet_auto ="));
    assert_eq!(
        r.body["values"]["listen"],
        json!(["127.0.0.1:7420", "100.64.0.9:7420", "100.64.0.10:7420"])
    );
}

/// A saved `false` reset to the default (on) needs the elevation, and a
/// `false` save empties both lists.
#[tokio::test]
async fn resetting_a_saved_false_needs_the_elevation() {
    let tmp = tempfile::tempdir().unwrap();
    let toml = AUTO_TOML.replace("tailnet_auto = true", "tailnet_auto = false");
    let (_f, c) = board(tmp.path(), &toml, Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let r = save(&c, &h, json!({ "tailnet_auto": null })).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["tailnet_auto"]));
    assert!(deck_file(tmp.path()).contains("tailnet_auto = false"));
    let token = elevate(&c, &h).await;
    let rev = revision(&c, &h).await;
    let r = post(
        &c,
        &h,
        "/api/settings/deck",
        json!({ "changes": { "tailnet_auto": null }, "revision": rev, "elevation": token }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["values"]["tailnet_auto"], true);
    // Turning it off again empties both lists, as an explicit `false` does.
    let r = save(&c, &h, json!({ "tailnet_auto": false })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["values"]["tailnet_auto"], false);
    assert_eq!(r.body["values"]["tailnet_auto_listen"], json!([]));
    assert_eq!(r.body["values"]["tailnet_auto_link_listen"], json!([]));
}

/// The values of the provenance lists of a `deck.toml`.
struct DeckFile {
    auto_listen: Vec<String>,
}

impl DeckFile {
    fn of(dir: &Path) -> Self {
        let config = fleetdeck_core::config::DeckConfig::parse(&deck_file(dir)).unwrap();
        Self {
            auto_listen: config
                .tailnet_auto_listen
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

#[tokio::test]
async fn turning_the_mode_off_empties_both_lists_and_turning_it_on_needs_elevation() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = board(tmp.path(), AUTO_TOML, Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;

    // Off needs no elevation, and both provenance lists are empty after it,
    // also when the same save sets one of them.
    let r = save(
        &c,
        &h,
        json!({ "tailnet_auto": false, "tailnet_auto_listen": ["100.64.0.10:7420"] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["values"]["tailnet_auto"], false);
    assert_eq!(r.body["values"]["tailnet_auto_listen"], json!([]));
    assert_eq!(r.body["values"]["tailnet_auto_link_listen"], json!([]));
    // Every entry is manual now: the lists are as they were.
    assert_eq!(
        r.body["values"]["listen"],
        json!(["127.0.0.1:7420", "100.64.0.9:7420", "100.64.0.10:7420"])
    );

    // On needs the elevation.
    let r = save(&c, &h, json!({ "tailnet_auto": true })).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["tailnet_auto"]));
    assert!(!deck_file(tmp.path()).contains("tailnet_auto = true"));
    let token = elevate(&c, &h).await;
    let rev = revision(&c, &h).await;
    let r = post(
        &c,
        &h,
        "/api/settings/deck",
        json!({ "changes": { "tailnet_auto": true }, "revision": rev, "elevation": token }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["restart_needed"], json!(["tailnet_auto"]));
    assert_eq!(r.body["values"]["tailnet_auto"], true);
}

fn assignable(ip: IpAddr) -> bool {
    ip.is_ipv4() || ip.is_ipv6()
}

#[tokio::test]
async fn use_tailnet_writes_the_provenance_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!("{DECK_TOML}tailscale_bin = \"{bin}\"\n");
    let (_f, c) = board(tmp.path(), &toml, Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let cookie = h[0].1.split_once('=').unwrap().1.to_owned();
    let session = c
        .store
        .login_session(&fleetdeck_core::Secret::new(cookie))
        .await
        .unwrap()
        .unwrap();
    let token = elevate(&c, &h).await;
    let done = use_with(&c.state, &session, Some(token), assignable)
        .await
        .map_err(|e| e.message)
        .unwrap();
    assert!(serde_json::to_value(&done).unwrap()["restart_needed"] == true);
    let config = fleetdeck_core::config::DeckConfig::parse(&deck_file(tmp.path())).unwrap();
    let texts = |list: &[std::net::SocketAddr]| -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    };
    // The entries that the button added, not the manual LAN entry.
    assert_eq!(
        texts(&config.tailnet_auto_listen),
        ["100.64.0.10:7420", "[fd7a:115c:a1e0::a]:7420"]
    );
    assert_eq!(
        texts(&config.tailnet_auto_link_listen),
        ["100.64.0.10:7421", "[fd7a:115c:a1e0::a]:7421"]
    );
    assert!(
        config
            .listen
            .iter()
            .any(|a| a.to_string() == "192.168.5.5:7420")
    );
    assert!(
        !config
            .tailnet_auto_listen
            .iter()
            .any(|a| a.to_string() == "192.168.5.5:7420")
    );
}

fn a(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

fn all_usable(_: IpAddr) -> bool {
    true
}

fn head(bin: &str) -> String {
    format!("name = \"d\"\ntailnet_auto = true\ntailscale_bin = \"{bin}\"\n")
}

fn dirs_of(root: &Path) -> NodeDirs {
    NodeDirs {
        config: root.join("config"),
        data: root.join("data"),
    }
}

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

/// The automatic start on a `deck.toml` of `toml`, with every address usable.
async fn run(dir: &Path, toml: &str) -> (DeckConfig, TailnetStart) {
    let settings = deck_settings(dir, toml);
    let config = DeckConfig::load(&settings.path).unwrap();
    let deadline = Instant::now() + fleetdeck_deck::tailnet_auto::DEADLINE;
    apply_at_start(
        &settings,
        config,
        &[],
        &HostFacts::default(),
        &all_usable,
        deadline,
    )
    .await
}

/// The child is killed when the deadline is gone: the process of a program
/// that outlives the deadline does not stay.
#[tokio::test]
async fn the_child_is_killed_at_the_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("pid");
    let bin = script(
        &tmp.path().join("tailscale"),
        &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()),
    );
    let settings = deck_settings(tmp.path(), &head(&bin));
    let config = DeckConfig::load(&settings.path).unwrap();
    // The child must start before the deadline: a loaded machine needs time.
    let deadline = Instant::now() + Duration::from_secs(5);
    let started = async {
        let until = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            // `echo` writes "<pid>\n": a line end means a complete pid.
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && text.ends_with('\n')
            {
                return text.trim().to_owned();
            }
            assert!(
                tokio::time::Instant::now() < until,
                "the child did not start"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let facts = HostFacts::default();
    let ((_, start), pid) = tokio::join!(
        apply_at_start(&settings, config, &[], &facts, &all_usable, deadline),
        started
    );
    assert_eq!(
        start.start_reason.as_deref(),
        Some("Tailscale did not answer within 3 seconds.")
    );
    let proc = Path::new("/proc").join(&pid);
    let until = tokio::time::Instant::now() + Duration::from_secs(15);
    while proc.exists() {
        assert!(
            tokio::time::Instant::now() < until,
            "the child {pid} is still running"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn fail_with_automatic_entries(bound: &[SocketAddr]) -> Result<(), TlsError> {
    if bound.len() > 1 {
        return Err(TlsError::File {
            file: "board-selfsigned.crt".to_owned(),
            reason: "test".to_owned(),
        });
    }
    Ok(())
}

/// Distinct free local ports, all bound at the same time before they are
/// released: an entry on port 0 of the deck can never take the port of the
/// automatic entry.
fn free_ports<const N: usize>() -> [std::net::SocketAddr; N] {
    let held: Vec<_> = (0..N)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    std::array::from_fn(|i| held[i].local_addr().unwrap())
}

/// The board certificate cannot be prepared with an automatic entry (design
/// 15.3): the deck starts with the manual entries and the existing
/// certificate, and the card says why.
#[tokio::test]
async fn a_certificate_failure_with_an_automatic_entry_falls_back_to_the_manual_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs_of(tmp.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    let path = dirs.deck_config();
    let write = |text: &str| {
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    // A first start makes the certificate for the manual entries.
    write(
        "name = \"d\"\ntailnet_auto = false\nlisten = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:0\"]\n",
    );
    let first = RunningDeck::start(
        &dirs,
        &DeckConfig::load(&path).unwrap(),
        fast(),
        StartOptions::default(),
    )
    .await
    .unwrap();
    let old = first.endpoint.as_ref().unwrap().certificate_sha256.clone();
    first.stop().await;
    // The second start has an automatic board entry (a local address with a
    // free port) and a certificate hook that fails when it is bound.
    let [auto, manual, link] = free_ports();
    let missing = tmp.path().join("no-such-tailscale").display().to_string();
    write(&format!(
        "name = \"d\"\ntailnet_auto = true\ntailscale_bin = \"{missing}\"\n\
         listen = [\"{manual}\", \"{auto}\"]\nlink_listen = [\"{link}\"]\n\
         tailnet_auto_listen = [\"{auto}\"]\n"
    ));
    let options = StartOptions {
        certificate_check: Some(fail_with_automatic_entries),
        ..StartOptions::default()
    };
    let deck = RunningDeck::start(&dirs, &DeckConfig::load(&path).unwrap(), fast(), options)
        .await
        .expect("the deck starts with the manual entries");
    let endpoint = deck.endpoint.as_ref().unwrap();
    assert_eq!(endpoint.certificate_sha256, old, "the old certificate");
    assert_ne!(endpoint.url, format!("https://{auto}"));
    let start = deck.tailnet_start();
    assert!(start.enabled);
    assert_eq!(
        start.start_reason.as_deref(),
        Some("the board certificate could not be updated (file)")
    );
    deck.stop().await;
}

/// A real write failure while the certificate for an automatic entry is
/// updated (design 15.3): the deck starts with the stored pair that was read
/// before the update, and the pair on the disk is not changed.
#[tokio::test]
async fn a_real_certificate_write_failure_falls_back_to_the_stored_pair() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs_of(tmp.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    let path = dirs.deck_config();
    let write = |text: &str| {
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    write(
        "name = \"d\"\ntailnet_auto = false\nlisten = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:0\"]\n",
    );
    let first = RunningDeck::start(
        &dirs,
        &DeckConfig::load(&path).unwrap(),
        fast(),
        StartOptions::default(),
    )
    .await
    .unwrap();
    let old = first.endpoint.as_ref().unwrap().certificate_sha256.clone();
    first.stop().await;
    let deck_dir = dirs.deck();
    let crt = std::fs::read(deck_dir.join("board-selfsigned.crt")).unwrap();
    let key = std::fs::read(deck_dir.join("board-selfsigned.key")).unwrap();
    // The key cannot be written: its temporary path is a directory.
    let blocker = deck_dir.join("board-selfsigned.key.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "x").unwrap();
    let [auto, manual, link] = free_ports();
    let missing = tmp.path().join("no-such-tailscale").display().to_string();
    // The host name adds a name to the certificate, so the update must write.
    write(&format!(
        "name = \"d\"\ntailnet_auto = true\ntailnet_hostname = \"deck.tail1234.ts.net\"\ntailscale_bin = \"{missing}\"\n\
         listen = [\"{manual}\", \"{auto}\"]\nlink_listen = [\"{link}\"]\n\
         tailnet_auto_listen = [\"{auto}\"]\n"
    ));
    let deck = RunningDeck::start(
        &dirs,
        &DeckConfig::load(&path).unwrap(),
        fast(),
        StartOptions::default(),
    )
    .await
    .expect("the deck starts with the stored pair");
    let endpoint = deck.endpoint.as_ref().unwrap();
    assert_eq!(endpoint.certificate_sha256, old, "the stored certificate");
    assert_ne!(endpoint.url, format!("https://{auto}"));
    let start = deck.tailnet_start();
    assert!(start.enabled);
    assert_eq!(
        start.start_reason.as_deref(),
        Some("the board certificate could not be updated (AlreadyExists)")
    );
    deck.stop().await;
    assert_eq!(
        std::fs::read(deck_dir.join("board-selfsigned.crt")).unwrap(),
        crt
    );
    assert_eq!(
        std::fs::read(deck_dir.join("board-selfsigned.key")).unwrap(),
        key
    );
}

/// A certificate failure with the manual entries only still fails the start.
#[tokio::test]
async fn a_certificate_failure_with_the_manual_entries_only_still_fails_the_start() {
    fn always_fail(_: &[SocketAddr]) -> Result<(), TlsError> {
        Err(TlsError::File {
            file: "board.crt".to_owned(),
            reason: "test".to_owned(),
        })
    }
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs_of(tmp.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    let path = dirs.deck_config();
    let toml = "name = \"d\"\ntailnet_auto = false\nlisten = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:0\"]\n";
    std::fs::write(&path, toml).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let options = StartOptions {
        certificate_check: Some(always_fail),
        ..StartOptions::default()
    };
    let err = RunningDeck::start(&dirs, &DeckConfig::load(&path).unwrap(), fast(), options)
        .await
        .expect_err("the start fails");
    assert!(err.to_string().contains("board.crt"), "{err}");
}

/// The migration from an absent key: the mode is on, and a first start with
/// the default saves the tailnet addresses (design 15.2).
#[tokio::test]
async fn an_absent_key_is_on_at_the_start() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake(tmp.path(), "tailscale", STATUS, None);
    let toml = format!("name = \"d\"\ntailscale_bin = \"{bin}\"\n");
    let r = run(tmp.path(), &toml).await;
    assert!(r.0.tailnet_auto);
    assert!(r.1.enabled && r.1.start_reason.is_none(), "{:?}", r.1);
    assert_eq!(
        r.0.tailnet_auto_listen,
        vec![a("100.64.0.10:7420"), a("[fd7a:115c:a1e0::a]:7420")]
    );
    // A saved `false` runs nothing.
    let tmp2 = tempfile::tempdir().unwrap();
    let marker = tmp2.path().join("ran");
    let bin2 = fake(tmp2.path(), "tailscale", STATUS, Some(&marker));
    let toml = format!("name = \"d\"\ntailnet_auto = false\ntailscale_bin = \"{bin2}\"\n");
    let r = run(tmp2.path(), &toml).await;
    assert_eq!(r.1, TailnetStart::default());
    assert!(!marker.exists());
}
