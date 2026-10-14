//! The field tables against the parsers, and the view.

use std::collections::BTreeSet;

use fleetdeck_proto::settings::{FieldKind, FieldMeta};
use serde_json::json;

use super::{CANARY, CANARY_REF, Fixture};
use crate::config::{
    DeckConfig, RAW_DECK_KEYS, RAW_INSTANCE_KEYS, RAW_MODEL_HELP_KEYS, RAW_STATION_KEYS,
    StationConfig,
};
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
        FieldKind::Int => f.min.unwrap_or(1).to_string(),
        FieldKind::Text if f.key == "tailnet_hostname" => "\"deck.example\"".to_owned(),
        FieldKind::Text if f.key == "tailscale_bin" => "\"/usr/bin/tailscale\"".to_owned(),
        FieldKind::Text if f.key == "clone_host" => "\"git.example.com\"".to_owned(),
        FieldKind::Text if f.key == "offline_approval_policy" => "\"wait\"".to_owned(),
        FieldKind::Text | FieldKind::Path => "\"x\"".to_owned(),
        FieldKind::Enum => format!("{:?}", f.options[0]),
        FieldKind::Paths => "[\"/tmp\"]".to_owned(),
        FieldKind::Addresses => "[\"127.0.0.1:7420\"]".to_owned(),
        FieldKind::Texts | FieldKind::Instances | FieldKind::Trackers => "[]".to_owned(),
        FieldKind::RepoGroups => "{}".to_owned(),
    }
}

#[test]
fn station_field_keys_equal_the_parser_keys() {
    let mut expected: BTreeSet<String> = RAW_STATION_KEYS
        .iter()
        .filter(|k| **k != "model_help")
        .map(|k| (*k).to_owned())
        .collect();
    expected.extend(
        RAW_MODEL_HELP_KEYS
            .iter()
            .map(|k| format!("model_help.{k}")),
    );
    let fields = station_fields();
    assert_eq!(keys(&fields), expected);
    assert_eq!(fields.len(), expected.len(), "a key appears twice");
}

#[test]
fn deck_field_keys_equal_the_parser_keys() {
    let expected: BTreeSet<String> = RAW_DECK_KEYS.iter().map(|k| (*k).to_owned()).collect();
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
        if f.kind == FieldKind::Int {
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
    assert_eq!(get("max_sessions").max, Some(256));
}

#[test]
fn view_of_a_missing_file_shows_the_defaults() {
    let fx = Fixture::new(None);
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(view.revision, revision_of(b""));
    assert_eq!(view.values["max_sessions"], json!(12));
    assert_eq!(view.values["host"], json!("direct"));
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
