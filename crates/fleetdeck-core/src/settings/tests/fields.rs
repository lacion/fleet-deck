//! The field tables against the parsers, and the view.

use std::collections::BTreeSet;

use fleetdeck_proto::settings::{FieldKind, FieldMeta};
use serde_json::json;

use super::{CANARY, CANARY_REF, Fixture};
use crate::config::{
    DeckConfig, RAW_CAPTURE_KEYS, RAW_DECK_KEYS, RAW_DECK_LIMITS_KEYS, RAW_DECK_RETENTION_KEYS,
    RAW_DEFAULT_MODE_KEYS, RAW_INSTANCE_KEYS, RAW_MODEL_HELP_KEYS, RAW_PROVIDER_UPDATES_KEYS,
    RAW_SECURITY_KEYS, RAW_STATION_ANALYTICS_KEYS, RAW_STATION_KEYS, RAW_STATION_LIMITS_KEYS,
    RAW_STATION_RETENTION_KEYS, StationConfig,
};
use crate::config_grant_limits::RAW_GRANT_LIMITS_KEYS;
use crate::config_worker_grant::RAW_WORKER_GRANT_KEYS;
use crate::paths::Env;
use crate::settings::{deck_fields, revision_of, station_fields, view_deck, view_station};

fn home() -> Env {
    Env {
        home: Some("/home/t".into()),
        ..Env::default()
    }
}

fn keys(fields: &[FieldMeta]) -> BTreeSet<String> {
    fields.iter().map(|f| f.key.clone()).collect()
}

/// A TOML value that the parser accepts for a field.
fn sample(f: &FieldMeta) -> String {
    match f.kind {
        FieldKind::Bool => "true".to_owned(),
        // The longest times take their ceiling: the minimum would break the
        // pair with the default idle time.
        FieldKind::Int if f.key.ends_with("_max_days") => f.max.unwrap_or(1).to_string(),
        FieldKind::Int | FieldKind::Limit => f.min.unwrap_or(1).to_string(),
        FieldKind::Text if f.key == "tailnet_hostname" => "\"deck.example\"".to_owned(),
        FieldKind::Text if f.key == "tailscale_bin" => "\"/usr/bin/tailscale\"".to_owned(),
        FieldKind::Text if f.key == "clone_host" => "\"git.example.com\"".to_owned(),
        FieldKind::Text if f.key == "provider_updates.window_start" => "\"03:00\"".to_owned(),
        FieldKind::Text if f.key == "provider_updates.timezone" => "\"local\"".to_owned(),
        FieldKind::Text if f.key.starts_with("provider_updates.") && f.key.ends_with("_pin") => {
            "\"2.1.305\"".to_owned()
        }
        FieldKind::Text if f.key == "offline_approval_policy" => "\"wait\"".to_owned(),
        FieldKind::Path if f.key == "clone_root" => "\"/home/t/projects\"".to_owned(),
        FieldKind::Text | FieldKind::Path => "\"x\"".to_owned(),
        FieldKind::Enum => format!("{:?}", f.options[0]),
        FieldKind::Paths => "[\"/tmp\"]".to_owned(),
        FieldKind::Addresses => "[\"127.0.0.1:7420\"]".to_owned(),
        FieldKind::Texts if f.key == "worker_grant.stations" => "[\"all\"]".to_owned(),
        FieldKind::Texts | FieldKind::Instances | FieldKind::Trackers => "[]".to_owned(),
        FieldKind::RepoGroups => "{}".to_owned(),
        FieldKind::ShareMounts => "{ \"nas/out\" = \"/mnt/out\" }".to_owned(),
    }
}

#[test]
fn station_field_keys_equal_the_parser_keys() {
    let tables = [
        "model_help",
        "limits",
        "retention",
        "capture",
        "default_mode",
        "analytics",
        "provider_updates",
    ];
    let mut expected: BTreeSet<String> = RAW_STATION_KEYS
        .iter()
        .filter(|k| !tables.contains(k))
        .map(|k| (*k).to_owned())
        .collect();
    let dotted = [
        ("model_help", RAW_MODEL_HELP_KEYS),
        ("limits", RAW_STATION_LIMITS_KEYS),
        ("retention", RAW_STATION_RETENTION_KEYS),
        ("capture", RAW_CAPTURE_KEYS),
        ("default_mode", RAW_DEFAULT_MODE_KEYS),
        ("analytics", RAW_STATION_ANALYTICS_KEYS),
        ("provider_updates", RAW_PROVIDER_UPDATES_KEYS),
    ];
    for (table, list) in dotted {
        expected.extend(list.iter().map(|k| format!("{table}.{k}")));
    }
    let fields = station_fields();
    assert_eq!(keys(&fields), expected);
    assert_eq!(fields.len(), expected.len(), "a key appears twice");
}

#[test]
fn deck_field_keys_equal_the_parser_keys() {
    let tables = [
        "security",
        "retention",
        "limits",
        "worker_grant",
        "grant_limits",
    ];
    let mut expected: BTreeSet<String> = RAW_DECK_KEYS
        .iter()
        .filter(|k| !tables.contains(k))
        .map(|k| (*k).to_owned())
        .collect();
    let dotted = [
        ("security", RAW_SECURITY_KEYS),
        ("retention", RAW_DECK_RETENTION_KEYS),
        ("limits", RAW_DECK_LIMITS_KEYS),
        ("worker_grant", RAW_WORKER_GRANT_KEYS),
        ("grant_limits", RAW_GRANT_LIMITS_KEYS),
    ];
    for (table, list) in dotted {
        expected.extend(list.iter().map(|k| format!("{table}.{k}")));
    }
    let fields = deck_fields();
    assert_eq!(keys(&fields), expected);
    assert_eq!(fields.len(), expected.len());
}

#[test]
fn every_station_key_parses_alone() {
    let env = home();
    for f in station_fields() {
        let text = format!("{} = {}\n", f.key, sample(&f));
        StationConfig::parse(&text, &env)
            .unwrap_or_else(|e| panic!("`{}` does not parse alone: {e}", f.key));
    }
    // A key that the parser does not know is refused: the check has teeth.
    assert!(StationConfig::parse("nope = 1\n", &env).is_err());
}

#[test]
fn every_deck_key_parses_alone() {
    for f in deck_fields() {
        let text = format!("{} = {}\n", f.key, sample(&f));
        DeckConfig::parse(&text)
            .unwrap_or_else(|e| panic!("`{}` does not parse alone: {e}", f.key));
    }
}

#[test]
fn the_clone_keys_are_live_and_show_their_defaults() {
    let fields = deck_fields();
    let get = |k: &str| fields.iter().find(|f| f.key == k).unwrap().clone();
    for key in ["clone_host", "clone_org", "clone_transport"] {
        let f = get(key);
        assert_eq!(f.apply, fleetdeck_proto::settings::Apply::Live, "{key}");
        assert!(!f.danger, "{key}");
    }
    assert_eq!(get("clone_host").kind, FieldKind::Text);
    assert_eq!(get("clone_org").kind, FieldKind::Text);
    assert_eq!(get("clone_transport").kind, FieldKind::Enum);
    assert_eq!(get("clone_transport").options, vec!["https", "ssh"]);
    let fx = Fixture::new(None);
    let deck = fx.dir.path().join("cfg").join("deck.toml");
    let view = view_deck(&deck, &[]).unwrap();
    assert_eq!(view.values["clone_host"], json!("github.com"));
    assert_eq!(view.values["clone_org"], json!(""));
    assert_eq!(view.values["clone_transport"], json!("https"));
    let value_keys: BTreeSet<String> = view.values.keys().cloned().collect();
    assert_eq!(value_keys, keys(&view.fields));
}

#[test]
fn every_instance_key_parses_alone() {
    let env = home();
    for key in RAW_INSTANCE_KEYS {
        let extra = match *key {
            "id" | "driver" => String::new(),
            "binary" => "binary = \"x\"\n".to_owned(),
            "config_home" => "config_home = \"/tmp/x\"\n".to_owned(),
            "env" => "env = { A = \"b\" }\n".to_owned(),
            other => panic!("no sample for instance key `{other}`"),
        };
        let text = format!("[[provider_instance]]\nid = \"a\"\ndriver = \"claude\"\n{extra}");
        StationConfig::parse(&text, &env).unwrap_or_else(|e| panic!("`{key}`: {e}"));
    }
}

#[test]
fn the_tables_are_well_formed() {
    for f in station_fields().into_iter().chain(deck_fields()) {
        assert!(!f.label.is_empty() && !f.help.is_empty(), "{}", f.key);
        assert!(f.help.ends_with('.'), "{}: help is a sentence", f.key);
        if matches!(f.kind, FieldKind::Int | FieldKind::Limit) {
            assert!(f.min.is_some() && f.max.is_some(), "{}", f.key);
        }
        if f.kind == FieldKind::Enum {
            assert!(!f.options.is_empty(), "{}", f.key);
        }
        assert!(f.cli_flag.is_none());
    }
    let station = station_fields();
    let get = |k: &str| station.iter().find(|f| f.key == k).unwrap().clone();
    assert!(get("trackers").read_only);
    assert!(get("provider_instance").secret && get("provider_instance").danger);
    assert!(get("listen").danger && get("host").danger);
    let bypass = get("allow_bypass_permissions");
    assert!(bypass.danger && bypass.apply == fleetdeck_proto::settings::Apply::Restart);
    assert_eq!(bypass.kind, FieldKind::Bool);
    let fleet = get("allow_fleet_control");
    assert!(fleet.danger && fleet.apply == fleetdeck_proto::settings::Apply::Live);
    assert_eq!(fleet.kind, FieldKind::Bool);
    assert_eq!(fleet.label, "Fleet control on this station");
    let desktop = get("allow_desktop");
    assert!(desktop.danger && desktop.apply == fleetdeck_proto::settings::Apply::Live);
    assert_eq!(desktop.kind, FieldKind::Bool);
    assert_eq!(desktop.label, "Desktop control");
    let capture = get("allow_desktop_capture");
    assert!(capture.danger && capture.apply == fleetdeck_proto::settings::Apply::Live);
    assert_eq!(capture.kind, FieldKind::Bool);
    assert_eq!(capture.label, "Desktop screenshots");
    let input = get("allow_desktop_input");
    assert!(input.danger && input.apply == fleetdeck_proto::settings::Apply::Live);
    assert_eq!(input.kind, FieldKind::Bool);
    assert_eq!(input.label, "Allow agent desktop input");
    assert_eq!(
        input.help,
        "Allows explicitly authorized coordinator grants to use keyboard input and pointer \
         movement on this station. Turning this off stops new input and requests interruption \
         of active input; already sent input cannot be undone."
    );
    // R13 P7b §3.2: the two new live danger fields and their exact texts.
    let click = get("allow_desktop_click");
    assert!(click.danger && click.apply == fleetdeck_proto::settings::Apply::Live);
    assert_eq!(click.kind, FieldKind::Bool);
    assert_eq!(click.label, "Allow agent clicks");
    assert_eq!(
        click.help,
        "Allows explicitly authorized coordinator grants to send one left pointer click at a \
         time in their permitted windows on this station. Needs Allow agent desktop input. \
         Turning this off stops new clicks and requests interruption of an active click; \
         already sent input cannot be undone."
    );
    let any = get("allow_desktop_input_any");
    assert!(any.danger && any.apply == fleetdeck_proto::settings::Apply::Live);
    assert_eq!(any.kind, FieldKind::Bool);
    assert_eq!(any.label, "Allow agent input in other application windows");
    assert_eq!(
        any.help,
        "Allows explicitly authorized coordinator grants to use keyboard input, pointer \
         movement and, when allowed, clicks in other application windows on this station, \
         including terminals and signed-in browser windows. Needs Allow agent desktop input. \
         Turning this off stops new input and requests interruption of active input; already \
         sent input cannot be undone."
    );
    let monitors = get("capture_monitors");
    assert!(monitors.danger && monitors.apply == fleetdeck_proto::settings::Apply::Live);
    assert_eq!(monitors.kind, FieldKind::Texts);
    assert_eq!(monitors.label, "Screenshot monitors");
    let summary = get("fleet_summary");
    assert!(summary.danger && summary.apply == fleetdeck_proto::settings::Apply::Live);
    assert_eq!(summary.kind, FieldKind::Bool);
    assert_eq!(summary.label, "Fleet summary for the desktop shell");
    assert!(
        summary
            .help
            .starts_with("Send a summary of the whole fleet")
    );
    assert_eq!(get("max_sessions").max, Some(256));
}

#[test]
fn view_of_a_missing_file_shows_the_defaults() {
    let fx = Fixture::new(None);
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(view.revision, revision_of(b""));
    assert_eq!(view.values["max_sessions"], json!(12));
    // Shim parity step 5 (DL-199): the default host of this system.
    let host = if cfg!(target_os = "linux") {
        "shim"
    } else {
        "direct"
    };
    assert_eq!(view.values["host"], json!(host));
    assert_eq!(view.values["model_help.provider"], json!(null));
    assert_eq!(view.values["offline_approval_policy"], json!("wait"));
    let value_keys: BTreeSet<String> = view.values.keys().cloned().collect();
    assert_eq!(value_keys, keys(&view.fields));
    assert!(view.overrides.is_empty());
}

#[test]
fn view_marks_the_command_line_overrides() {
    let fx = Fixture::new(None);
    let deck = fx.dir.path().join("cfg").join("deck.toml");
    let view = view_deck(&deck, &[("listen", "--listen"), ("nope", "--nope")]).unwrap();
    assert_eq!(view.overrides, vec!["listen".to_owned()]);
    let listen = view.fields.iter().find(|f| f.key == "listen").unwrap();
    assert_eq!(listen.cli_flag.as_deref(), Some("--listen"));
    let name = view.fields.iter().find(|f| f.key == "name").unwrap();
    assert_eq!(name.cli_flag, None);
}

#[test]
fn a_secret_never_reaches_a_view() {
    let fx = Fixture::new(Some(&super::inline_station()));
    // A second instance holds a stored secret.
    std::fs::create_dir_all(&fx.store).unwrap();
    let path = fx.store.join("stored-one");
    std::fs::write(&path, CANARY_REF).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let mut text = fx.text();
    text.push_str("\n[[provider_instance]]\nid = \"other\"\ndriver = \"codex\"\nenv = { OPENAI_API_KEY = { secret = \"stored-one\" } }\n");
    std::fs::write(&fx.config, text).unwrap();
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    let all = format!("{view:?} {}", serde_json::to_string(&view).unwrap());
    assert!(!all.contains(CANARY), "{all}");
    assert!(!all.contains(CANARY_REF), "{all}");
    let instances = view.values["provider_instance"].as_array().unwrap();
    assert_eq!(
        instances[0]["env"]["ANTHROPIC_API_KEY"],
        json!({ "set": true })
    );
    assert_eq!(
        instances[1]["env"]["OPENAI_API_KEY"],
        json!({ "set": true })
    );
}

#[test]
fn a_reference_needs_the_store() {
    let fx = Fixture::new(Some(
        "[[provider_instance]]\nid = \"a\"\ndriver = \"claude\"\nenv = { K = { secret = \"x\" } }\n",
    ));
    let plain = StationConfig::load(&fx.config, &fx.env)
        .unwrap_err()
        .to_string();
    assert!(plain.contains("secret store"), "{plain}");
    let missing = fx.load().unwrap_err().to_string();
    assert!(missing.contains("`x`"), "{missing}");
}

#[test]
fn the_fleet_git_keys_are_restart_keys_and_two_are_danger() {
    let fields = station_fields();
    let get = |k: &str| fields.iter().find(|f| f.key == k).unwrap().clone();
    for key in [
        "clone_root",
        "traits",
        "allow_check_cmd",
        "allow_local_remotes",
        "taildrive_mounts",
    ] {
        let f = get(key);
        assert_eq!(f.apply, fleetdeck_proto::settings::Apply::Restart, "{key}");
        assert!(!f.read_only && !f.secret && !f.hidden, "{key}");
    }
    assert_eq!(get("clone_root").kind, FieldKind::Path);
    assert_eq!(get("clone_root").label, "Clone folder");
    assert_eq!(get("traits").kind, FieldKind::Texts);
    assert_eq!(get("taildrive_mounts").kind, FieldKind::ShareMounts);
    assert!(get("allow_check_cmd").danger && get("allow_local_remotes").danger);
    assert!(!get("clone_root").danger && !get("traits").danger && !get("taildrive_mounts").danger);
}

#[test]
fn the_fleet_git_values_save_and_show_in_the_view() {
    use crate::settings::danger_keys;
    let fx = Fixture::new(Some("allowed_roots = [\"/tmp\"]\n"));
    let w = fx.writer();
    let change = fx.change(json!({
        "clone_root": "/tmp/projects",
        "traits": ["gpu", "always_on"],
        "allow_check_cmd": true,
        "taildrive_mounts": { "nas/out": "/mnt/out" },
    }));
    let ok_prepare = |_: &crate::config::StationConfig| Ok::<(), (String, String)>(());
    w.save(&change, true, ok_prepare).unwrap();
    let text = fx.text();
    assert!(text.contains("[taildrive_mounts]"), "{text}");
    assert!(text.contains("\"nas/out\" = \"/mnt/out\""), "{text}");
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(view.values["clone_root"], json!("/tmp/projects"));
    assert_eq!(view.values["traits"], json!(["gpu", "always_on"]));
    assert_eq!(view.values["allow_check_cmd"], json!(true));
    assert_eq!(view.values["allow_local_remotes"], json!(false));
    assert_eq!(
        view.values["taildrive_mounts"],
        json!({ "nas/out": "/mnt/out" })
    );
    // An unset clone_root shows the empty text.
    let none = Fixture::new(None);
    let view = view_station(&none.config, &none.env, &none.store, &[]).unwrap();
    assert_eq!(view.values["clone_root"], json!(""));
    assert_eq!(view.values["taildrive_mounts"], json!({}));
    // A bad value names its key, not the entry.
    let bad = fx.change(json!({ "taildrive_mounts": { "NAS/out": "/m" } }));
    let err = w.save(&bad, true, ok_prepare).unwrap_err();
    assert!(err.to_string().contains("taildrive_mounts"), "{err}");
    // Turning a danger key on needs elevation; the old file and a new one differ.
    let old = crate::config::StationConfig::parse("", &fx.env).unwrap();
    let new = crate::config::StationConfig::parse(
        "allow_check_cmd = true\nallow_local_remotes = true\n",
        &fx.env,
    )
    .unwrap();
    assert_eq!(
        danger_keys::<crate::settings::StationHolder>(&old, &new),
        ["allow_check_cmd", "allow_local_remotes"]
    );
    assert!(danger_keys::<crate::settings::StationHolder>(&new, &old).is_empty());
}

#[test]
fn agent_desktop_input_is_off_by_default_and_parses_as_a_bool() {
    // R13 P7 §3.3: the default is false everywhere; the key is a strict bool.
    let c = StationConfig::parse("", &home()).unwrap();
    assert!(!c.allow_desktop_input);
    let c = StationConfig::parse("allow_desktop_input = true\n", &home()).unwrap();
    assert!(c.allow_desktop_input);
    let c = StationConfig::parse("allow_desktop_input = false\n", &home()).unwrap();
    assert!(!c.allow_desktop_input);
    assert!(StationConfig::parse("allow_desktop_input = \"yes\"\n", &home()).is_err());
    // The view of a missing file shows `false`, and the value follows the file.
    let fx = Fixture::new(None);
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(view.values["allow_desktop_input"], json!(false));
    let fx = Fixture::new(Some("name = \"box\"\nallow_desktop_input = true\n"));
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(view.values["allow_desktop_input"], json!(true));
}

#[test]
fn agent_clicks_and_broader_input_are_off_by_default_and_parse_as_bools() {
    // R13 P7b §3.2: the default is false everywhere; the keys are strict bools.
    let c = StationConfig::parse("", &home()).unwrap();
    assert!(!c.allow_desktop_click && !c.allow_desktop_input_any);
    let c = StationConfig::parse(
        "allow_desktop_click = true\nallow_desktop_input_any = true\n",
        &home(),
    )
    .unwrap();
    assert!(c.allow_desktop_click && c.allow_desktop_input_any);
    // Each key is independent of the other and of `allow_desktop_input`.
    let c = StationConfig::parse("allow_desktop_click = true\n", &home()).unwrap();
    assert!(c.allow_desktop_click && !c.allow_desktop_input_any && !c.allow_desktop_input);
    let c = StationConfig::parse("allow_desktop_input_any = true\n", &home()).unwrap();
    assert!(!c.allow_desktop_click && c.allow_desktop_input_any);
    assert!(StationConfig::parse("allow_desktop_click = \"yes\"\n", &home()).is_err());
    assert!(StationConfig::parse("allow_desktop_input_any = 1\n", &home()).is_err());
    let fx = Fixture::new(None);
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(view.values["allow_desktop_click"], json!(false));
    assert_eq!(view.values["allow_desktop_input_any"], json!(false));
    let fx = Fixture::new(Some(
        "name = \"box\"\nallow_desktop_click = true\nallow_desktop_input_any = true\n",
    ));
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(view.values["allow_desktop_click"], json!(true));
    assert_eq!(view.values["allow_desktop_input_any"], json!(true));
}
