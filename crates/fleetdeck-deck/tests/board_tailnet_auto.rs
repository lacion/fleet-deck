//! The tailnet routes on WSL and with "Use Tailscale automatically"
//! (tailnet design 14.1 to 14.5): the hint about the Windows program, the NAT
//! text, the card data of the automatic mode, the provenance lists that a
//! manual save and "Use tailnet" write, and the elevation rules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use common::board::{BoardClient, Extras, board_with_extras};
use common::*;
use fleetdeck_core::tailscale;
use fleetdeck_core::tailscale_wsl::HostFacts;
use fleetdeck_deck::http::tailnet_routes::use_with;
use fleetdeck_deck::tailnet_auto::TailnetStart;
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

/// The path of the fake Windows program of the one test that needs it.
static WINDOWS_FAKE: OnceLock<PathBuf> = OnceLock::new();

fn wsl_with_windows_program() -> HostFacts {
    HostFacts {
        wsl: true,
        windows_program: WINDOWS_FAKE.get().cloned(),
        nonlocal_bind: false,
    }
}

#[tokio::test]
async fn the_windows_program_is_a_hint_and_is_not_run_until_the_owner_chooses_it() {
    if tailscale::is_installed() {
        eprintln!("note: tailscale is on PATH of this machine; the test needs it absent");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("ran");
    let exe = fake(tmp.path(), "tailscale.exe", STATUS, Some(&marker));
    WINDOWS_FAKE.set(PathBuf::from(&exe)).unwrap();
    let (_f, c) = board(
        tmp.path(),
        DECK_TOML,
        Extras {
            host: Some(wsl_with_windows_program),
            ..Extras::default()
        },
    )
    .await;
    set_password(&c).await;
    let h = owner(&c).await;

    // No program is found: the hint, and the program is not touched.
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["available"], false);
    assert_eq!(r.body["reason"], "windows_tailscale");
    assert_eq!(r.body["windows_program"], json!(exe));
    assert_eq!(r.body["userspace"], false);
    assert_eq!(r.body["auto"]["enabled"], false);
    let r = get(&c, &h, "/api/tailnet/devices").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["available"], false);
    assert_eq!(r.body["reason"], "windows_tailscale");
    assert_eq!(r.body["windows_program"], json!(exe));
    assert_eq!(r.body["devices"], json!([]));
    assert!(
        !marker.exists(),
        "the Windows program ran without the setting"
    );

    // "Use this program" is a settings save of `tailscale_bin`: it needs the
    // elevation.
    let view = get(&c, &h, "/api/settings/deck").await;
    let revision = view.body["revision"].as_str().unwrap().to_owned();
    let save = |elevation: Option<&str>| {
        let mut body = json!({ "changes": { "tailscale_bin": exe }, "revision": revision });
        if let Some(e) = elevation {
            body["elevation"] = json!(e);
        }
        body
    };
    let r = post(&c, &h, "/api/settings/deck", save(None)).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["tailscale_bin"]));
    assert!(!marker.exists());
    let token = elevate(&c, &h).await;
    let r = post(&c, &h, "/api/settings/deck", save(Some(&token))).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(deck_file(tmp.path()).contains(&exe));
    // Now the program is the owner's choice: it runs, and the hint is gone.
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.body["available"], true, "{}", r.body);
    assert!(r.body.get("windows_program").is_none());
    assert!(marker.exists(), "the chosen program did not run");
}

#[tokio::test]
async fn a_set_tailscale_bin_that_is_missing_keeps_the_not_installed_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-tailscale").display().to_string();
    let toml = format!("{DECK_TOML}tailscale_bin = \"{missing}\"\n");
    let (_f, c) = board(
        tmp.path(),
        &toml,
        Extras {
            host: Some(|| HostFacts {
                wsl: true,
                windows_program: Some(PathBuf::from(
                    "/mnt/c/Program Files/Tailscale/tailscale.exe",
                )),
                nonlocal_bind: false,
            }),
            ..Extras::default()
        },
    )
    .await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.body["available"], false);
    assert_eq!(r.body["reason"], "not installed");
    assert!(r.body.get("windows_program").is_none());
    let r = get(&c, &h, "/api/tailnet/devices").await;
    assert_eq!(r.body["reason"], "not installed");
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
    let (_f3, c3) = board(tmp3.path(), DECK_TOML, Extras::default()).await;
    let h3 = owner(&c3).await;
    let r = get(&c3, &h3, "/api/tailnet").await;
    assert_eq!(
        r.body["auto"],
        json!({ "enabled": false, "skipped": [], "changed": false })
    );
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
