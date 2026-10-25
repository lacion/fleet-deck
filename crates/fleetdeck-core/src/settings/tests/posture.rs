//! The effective security posture of B27 (§3.6): the relaxed controls of a
//! file, of the running process and of the command-line flags.

use std::net::SocketAddr;

use fleetdeck_proto::settings::{Relaxed, RelaxedState};
use serde_json::json;

use super::{Fixture, change_with, ok_prepare};
use crate::config::{CLAUDE_PROVIDER_DEFAULT, DeckConfig, OfflineApprovalPolicy, StationConfig};
use crate::paths::Env;
use crate::settings::{
    DECK_NOT_POSTURE, DeckHolder, STATION_NOT_POSTURE, STATION_POSTURE_KEYS, StationHolder,
    deck_fields, posture, station_fields, view_deck, view_station,
};

fn env() -> Env {
    Env {
        home: Some("/home/t".into()),
        ..Env::default()
    }
}

fn deck(text: &str) -> DeckConfig {
    DeckConfig::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn station(text: &str) -> StationConfig {
    StationConfig::parse(text, &env()).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

fn find<'a>(items: &'a [Relaxed], key: &str) -> &'a Relaxed {
    items
        .iter()
        .find(|i| i.key == key)
        .unwrap_or_else(|| panic!("no item {key}: {items:?}"))
}

fn keys(items: &[Relaxed]) -> Vec<&str> {
    items.iter().map(|i| i.key.as_str()).collect()
}

fn label(fields: Vec<fleetdeck_proto::settings::FieldMeta>, key: &str) -> String {
    fields.into_iter().find(|f| f.key == key).unwrap().label
}

#[test]
fn a_default_deck_has_no_relaxed_control() {
    assert!(posture::<DeckHolder>(&deck(""), None, &[]).is_empty());
    assert!(posture::<StationHolder>(&station(""), None, &[]).is_empty());
}

#[test]
fn the_deck_posture_lists_every_relaxed_control() {
    let c = deck(
        "fleet_control = true\n[security]\nmin_password_chars = 8\n[grant_limits]\nspawn_cap = \"none\"\n[worker_grant]\nallow_bypass = true\n",
    );
    let items = posture::<DeckHolder>(&c, None, &[]);
    assert_eq!(items.len(), 4, "{items:?}");
    assert!(items.iter().all(|i| i.state == RelaxedState::Active));
    assert_eq!(find(&items, "fleet_control").text, "Fleet control");
    assert_eq!(
        find(&items, "security.min_password_chars").text,
        "Shortest owner password (characters): 8 (default 12)"
    );
    assert_eq!(
        find(&items, "grant_limits.spawn_cap").text,
        "Highest session limit of a grant: no limit (default 50)"
    );
    assert_eq!(
        find(&items, "worker_grant.allow_bypass").text,
        "Allow bypass mode for workers"
    );
}

#[test]
fn a_tailnet_address_is_not_relaxed_but_every_interface_is() {
    // The config parser refuses an unspecified address, so the test sets it
    // on the parsed value (a command-line value takes the same way).
    let mut c = deck("listen = [\"100.64.0.1:7420\"]\n");
    assert!(posture::<DeckHolder>(&c, None, &[]).is_empty());
    c.listen = vec![addr("0.0.0.0:7420")];
    let items = posture::<DeckHolder>(&c, None, &[]);
    assert_eq!(keys(&items), ["listen"]);
    assert_eq!(
        items[0].text,
        format!(
            "{}: every network interface",
            label(deck_fields(), "listen")
        )
    );
}

#[test]
fn the_station_posture_lists_every_relaxed_control() {
    type Relax = fn(&mut StationConfig);
    let rows: [(&str, Relax); 17] = [
        ("allow_setup_cmd", |c| c.allow_setup_cmd = true),
        ("allow_shell", |c| c.allow_shell = true),
        ("allow_bypass_permissions", |c| {
            c.allow_bypass_permissions = true;
        }),
        ("allow_fleet_control", |c| c.allow_fleet_control = true),
        ("allow_desktop", |c| c.allow_desktop = true),
        ("allow_desktop_capture", |c| c.allow_desktop_capture = true),
        ("allow_desktop_input", |c| c.allow_desktop_input = true),
        ("allow_desktop_click", |c| c.allow_desktop_click = true),
        ("allow_desktop_input_any", |c| {
            c.allow_desktop_input_any = true;
        }),
        ("fleet_summary", |c| c.fleet_summary = true),
        ("allow_check_cmd", |c| c.allow_check_cmd = true),
        ("allow_local_remotes", |c| c.allow_local_remotes = true),
        ("offline_approval_policy", |c| {
            // `wait` is the default, so no other policy is above it.
            c.offline_approval_policy = OfflineApprovalPolicy::Wait;
        }),
        ("listen", |c| {
            c.listen = vec!["0.0.0.0:7421".parse().unwrap()]
        }),
        ("retention.raw_log_mib", |c| c.retention.raw_log_mib = 64),
        ("default_mode.claude", |c| {
            c.default_mode.claude = Some("acceptEdits".to_owned());
        }),
        ("default_mode.codex", |c| {
            c.default_mode.codex = Some("on-request/workspace-write".to_owned());
        }),
    ];
    assert_eq!(
        rows.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        STATION_POSTURE_KEYS
    );
    for (key, relax) in rows {
        let mut c = station("");
        let off = posture::<StationHolder>(&c, None, &[]);
        assert!(off.is_empty(), "{key}: {off:?}");
        relax(&mut c);
        let on = posture::<StationHolder>(&c, None, &[]);
        if key == "offline_approval_policy" {
            // The rank of `wait` is the highest: no value of the policy is above it.
            assert!(on.is_empty(), "{on:?}");
            continue;
        }
        assert_eq!(keys(&on), [key], "{key}");
        let name = label(station_fields(), key);
        assert!(on[0].text.starts_with(&name), "{key}: {}", on[0].text);
    }
    let mut c = station("");
    c.retention.raw_log_mib = 64;
    let text = &posture::<StationHolder>(&c, None, &[])[0].text;
    assert!(text.ends_with(": 64 (default 16)"), "{text}");
    let mut c = station("");
    c.default_mode.claude = Some("acceptEdits".to_owned());
    let text = &posture::<StationHolder>(&c, None, &[])[0].text;
    assert!(
        text.ends_with(&format!(
            ": acceptEdits (default {CLAUDE_PROVIDER_DEFAULT})"
        )),
        "{text}"
    );
}

#[test]
fn every_danger_field_is_a_posture_key_or_excluded() {
    // The station: the danger fields are the posture keys of the table or
    // the excluded keys, and nothing else.
    let danger: Vec<String> = station_fields()
        .into_iter()
        .filter(|f| f.danger)
        .map(|f| f.key)
        .collect();
    for key in &danger {
        assert!(
            STATION_POSTURE_KEYS.contains(&key.as_str())
                || STATION_NOT_POSTURE.contains(&key.as_str()),
            "the station danger field {key} is neither a posture key nor excluded"
        );
    }
    // The deck: a file that relaxes every danger field that is a posture key.
    let mut c = deck(
        "fleet_control = true\ndesktop_control = true\ntailscale_cert = true\n\
         [security]\nmin_password_chars = 8\nlogin_idle_days = 14\nlogin_max_days = 60\n\
         elevation_minutes = 30\napi_token_max_days = 180\ndisplay_idle_days = 60\n\
         display_max_days = 180\ninvite_minutes = 60\ndesktop_screenshots_per_minute = 10\n\
         desktop_actions_per_minute = 100\n[retention]\naudit_days = 30\naudit_max_rows = 20000\n\
         [grant_limits]\nworker_grants = \"none\"\nspawn_cap = \"none\"\n\
         worker_spawn_cap = \"none\"\nidle_hours = \"none\"\nanswer_cap = \"none\"\n\
         [worker_grant]\nenabled = true\nallow_bypass = true\nstations = [\"all\"]\n\
         spawn_cap = \"none\"\nidle_minutes = \"none\"\nlifetime_hours = 168\n\
         claude_mode = \"bypassPermissions\"\ncodex_mode = \"never/danger-full-access\"\n",
    );
    c.listen = vec![addr("0.0.0.0:7420")];
    c.link_listen = vec![addr("0.0.0.0:7421")];
    let items = posture::<DeckHolder>(&c, None, &[]);
    let mut listed: Vec<String> = keys(&items).into_iter().map(str::to_owned).collect();
    listed.extend(DECK_NOT_POSTURE.iter().map(|k| (*k).to_owned()));
    listed.sort();
    let mut danger: Vec<String> = deck_fields()
        .into_iter()
        .filter(|f| f.danger)
        .map(|f| f.key)
        .collect();
    danger.sort();
    assert_eq!(listed, danger);
}

#[test]
fn a_save_reports_the_keys_that_relaxed() {
    let fx = Fixture::new(Some("name = \"box\"\n"));
    let w = fx.writer();
    let saved = w
        .save(&fx.change(json!({ "allow_shell": true })), true, ok_prepare)
        .unwrap()
        .0;
    assert_eq!(saved.relaxed, ["allow_shell"]);
    let back = change_with(&fx.revision(), json!({ "allow_shell": false }));
    let saved = w.save(&back, true, ok_prepare).unwrap().0;
    assert!(saved.relaxed.is_empty());
    // The wire form leaves an empty list out.
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("\"relaxed\"")
    );
}

#[test]
fn a_view_has_the_posture() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.toml");
    std::fs::write(&path, "fleet_control = true\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let relaxed = view_deck(&path, &[]).unwrap().relaxed.unwrap();
    assert_eq!(keys(&relaxed), ["fleet_control"]);
    assert!(relaxed.iter().all(|i| i.state == RelaxedState::Active));
    let fx = Fixture::new(Some("allow_shell = true\n"));
    let relaxed = view_station(&fx.config, &fx.env, &fx.store, &[])
        .unwrap()
        .relaxed
        .unwrap();
    assert_eq!(keys(&relaxed), ["allow_shell"]);
    assert!(relaxed.iter().all(|i| i.state == RelaxedState::Active));
    let fx = Fixture::new(None);
    let secure = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(secure.relaxed, Some(Vec::new()));
}

#[test]
fn a_restart_field_saved_secure_stays_until_the_restart() {
    let running = station("allow_bypass_permissions = true\n");
    let file = station("allow_bypass_permissions = false\n");
    let items = posture::<StationHolder>(&file, Some(&running), &[]);
    assert_eq!(keys(&items), ["allow_bypass_permissions"]);
    assert_eq!(items[0].state, RelaxedState::UntilRestart);
}

#[test]
fn a_restart_field_saved_relaxed_waits_for_the_restart() {
    let running = station("allow_bypass_permissions = false\n");
    let file = station("allow_bypass_permissions = true\n");
    let items = posture::<StationHolder>(&file, Some(&running), &[]);
    assert_eq!(keys(&items), ["allow_bypass_permissions"]);
    assert_eq!(items[0].state, RelaxedState::AfterRestart);
}

#[test]
fn a_command_line_value_is_in_the_posture() {
    let file = deck("listen = [\"100.64.0.1:7420\"]\n");
    let mut running = deck("listen = [\"100.64.0.1:7420\"]\n");
    running.listen = vec![addr("0.0.0.0:7420")];
    let items = posture::<DeckHolder>(&file, Some(&running), &["listen"]);
    assert_eq!(keys(&items), ["listen"]);
    assert_eq!(items[0].state, RelaxedState::CommandLine);
    // A secure flag value over a relaxed file value: no item.
    let mut relaxed_file = deck("");
    relaxed_file.listen = vec![addr("0.0.0.0:7420")];
    let secure_running = deck("listen = [\"100.64.0.1:7420\"]\n");
    let items = posture::<DeckHolder>(&relaxed_file, Some(&secure_running), &["listen"]);
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_live_field_follows_the_file() {
    let file = deck("[worker_grant]\nallow_bypass = true\n");
    let running = deck("");
    let items = posture::<DeckHolder>(&file, Some(&running), &[]);
    assert_eq!(keys(&items), ["worker_grant.allow_bypass"]);
    assert_eq!(items[0].state, RelaxedState::Active);
}

#[test]
fn a_bypass_default_waits_for_the_bypass_restart() {
    let file = station(
        "allow_bypass_permissions = true\n[default_mode]\ncodex = \"never/danger-full-access\"\n",
    );
    let running = station("allow_bypass_permissions = false\n");
    let items = posture::<StationHolder>(&file, Some(&running), &[]);
    assert_eq!(
        keys(&items),
        ["allow_bypass_permissions", "default_mode.codex"]
    );
    assert!(items.iter().all(|i| i.state == RelaxedState::AfterRestart));
    // With the bypass switch on in the running process, the default is active.
    let on = station("allow_bypass_permissions = true\n");
    let items = posture::<StationHolder>(&file, Some(&on), &[]);
    assert_eq!(
        find(&items, "default_mode.codex").state,
        RelaxedState::Active
    );
}
