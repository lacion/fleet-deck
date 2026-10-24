//! The security options of B27 (§3.2 to §3.5): the grant limits with "no
//! limit", the bypass switch of worker grants and the default mode of a
//! station, in the config parsers, the field tables and the writer.

use fleetdeck_proto::settings::FieldKind;
use serde_json::json;

use super::{Fixture, change_with, ok_prepare};
use crate::config::{ConfigError, DeckConfig, StationConfig};
use crate::config_grant_limits::{GrantLimits, default_within};
use crate::config_worker_grant::{WorkerGrantConfig, widens};
use crate::paths::Env;
use crate::settings::{
    DeckHolder, SaveError, SettingsWriter, StationHolder, danger_keys, deck_fields, station_fields,
    view_deck, view_station,
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

/// The field and the message of a refused `deck.toml`.
fn deck_error(text: &str) -> (String, String) {
    match DeckConfig::parse(text) {
        Err(ConfigError::Invalid { field, message }) => (field, message),
        other => panic!("{text}: expected a field error, got {other:?}"),
    }
}

fn station_error(text: &str) -> (String, String) {
    match StationConfig::parse(text, &env()) {
        Err(ConfigError::Invalid { field, message }) => (field, message),
        other => panic!("{text}: expected a field error, got {other:?}"),
    }
}

#[test]
fn grant_limits_default_to_the_old_constants() {
    let g = deck("").grant_limits;
    assert_eq!(g, GrantLimits::default());
    assert_eq!(g.worker_grants, Some(200));
    assert_eq!(g.spawn_cap, Some(50));
    assert_eq!(g.worker_spawn_cap, Some(10));
    assert_eq!(g.idle_hours, Some(24));
    assert_eq!(g.answer_cap, Some(5000));
}

#[test]
fn a_grant_limit_is_a_number_in_range_or_none() {
    let g = deck("[grant_limits]\nspawn_cap = \"none\"\n").grant_limits;
    assert_eq!(g.spawn_cap, None);
    assert_eq!(g.answer_cap, Some(5000));
    for bad in ["0", "100001", "\"unlimited\""] {
        let (field, _) = deck_error(&format!("[grant_limits]\nspawn_cap = {bad}\n"));
        assert_eq!(field, "grant_limits.spawn_cap", "{bad}");
    }
    let (field, message) = deck_error("[grant_limits]\nidle_hours = 169\n");
    assert_eq!(field, "grant_limits.idle_hours");
    assert_eq!(message, "use a whole number from 1 to 168, or \"none\"");
    assert!(DeckConfig::parse("[grant_limits]\nnope = 1\n").is_err());
}

#[test]
fn worker_grant_values_stay_within_the_grant_limits() {
    let worker =
        |limits: &str, grant: &str| format!("[grant_limits]\n{limits}\n[worker_grant]\n{grant}\n");
    // The six rows of the table, each with its exact message.
    let rows = [
        (
            worker("", "spawn_cap = 11"),
            "worker_grant.spawn_cap",
            "use a value from 0 to 10, the grant limit of the deck",
        ),
        (
            worker("", "spawn_cap = \"none\""),
            "worker_grant.spawn_cap",
            "\"none\" needs grant_limits.worker_spawn_cap = \"none\"",
        ),
        (
            worker("", "idle_minutes = 1441"),
            "worker_grant.idle_minutes",
            "use a value from 5 to 1440, the grant limit of the deck",
        ),
        (
            worker("", "idle_minutes = \"none\""),
            "worker_grant.idle_minutes",
            "\"none\" needs grant_limits.idle_hours = \"none\"",
        ),
        (
            worker("", "claude_mode = \"bypassPermissions\""),
            "worker_grant.claude_mode",
            "bypassPermissions needs worker_grant.allow_bypass = true",
        ),
        (
            worker("", "codex_mode = \"never/danger-full-access\""),
            "worker_grant.codex_mode",
            "never/danger-full-access needs worker_grant.allow_bypass = true",
        ),
    ];
    for (text, field, message) in rows {
        assert_eq!(
            deck_error(&text),
            (field.to_owned(), message.to_owned()),
            "{text}"
        );
    }
    // A limit lower than the file value is refused with the new ceiling.
    let (field, message) = deck_error(&worker("worker_spawn_cap = 1", "spawn_cap = 2"));
    assert_eq!(field, "worker_grant.spawn_cap");
    assert_eq!(
        message,
        "use a value from 0 to 1, the grant limit of the deck"
    );
    // The accepted cases.
    let w = deck(&worker("worker_spawn_cap = 20", "spawn_cap = 11")).worker_grant;
    assert_eq!(w.spawn_cap, Some(11));
    let w = deck(&worker(
        "worker_spawn_cap = \"none\"\nidle_hours = \"none\"",
        "spawn_cap = \"none\"\nidle_minutes = \"none\"",
    ))
    .worker_grant;
    assert_eq!((w.spawn_cap, w.idle_minutes), (None, None));
    let w = deck(&worker("idle_hours = 48", "idle_minutes = 2880")).worker_grant;
    assert_eq!(w.idle_minutes, Some(2880));
    // An omitted cap takes the default, lowered to the limit.
    let w = deck(&worker("worker_spawn_cap = 1", "")).worker_grant;
    assert_eq!(w.spawn_cap, Some(1));
}

#[test]
fn a_bypass_ceiling_needs_allow_bypass() {
    let refused = |grant: &str| deck_error(&format!("[worker_grant]\n{grant}\n")).0;
    assert_eq!(
        refused("claude_mode = \"bypassPermissions\""),
        "worker_grant.claude_mode"
    );
    assert_eq!(
        refused("codex_mode = \"never/danger-full-access\""),
        "worker_grant.codex_mode"
    );
    let w = deck(
        "[worker_grant]\nallow_bypass = true\nclaude_mode = \"bypassPermissions\"\ncodex_mode = \"never/danger-full-access\"\n",
    )
    .worker_grant;
    assert!(w.allow_bypass);
    assert_eq!(w.claude_mode, "bypassPermissions");
    assert_eq!(w.codex_mode, "never/danger-full-access");
    // `allow_bypass = false` keeps the refusal; an unknown mode is refused too.
    assert!(
        DeckConfig::parse(
            "[worker_grant]\nallow_bypass = false\nclaude_mode = \"bypassPermissions\"\n"
        )
        .is_err()
    );
    assert!(
        DeckConfig::parse("[worker_grant]\nallow_bypass = true\nclaude_mode = \"x\"\n").is_err()
    );
}

#[test]
fn widening_covers_the_new_worker_keys() {
    let base = WorkerGrantConfig::default();
    let with = |f: &dyn Fn(&mut WorkerGrantConfig)| {
        let mut c = base.clone();
        f(&mut c);
        c
    };
    let bypass = with(&|c| c.allow_bypass = true);
    assert_eq!(widens(&base, &bypass), ["worker_grant.allow_bypass"]);
    assert!(widens(&bypass, &base).is_empty());
    let spawn = with(&|c| c.spawn_cap = None);
    assert_eq!(widens(&base, &spawn), ["worker_grant.spawn_cap"]);
    assert!(widens(&spawn, &base).is_empty());
    let idle = with(&|c| c.idle_minutes = None);
    assert_eq!(widens(&base, &idle), ["worker_grant.idle_minutes"]);
    assert!(widens(&idle, &base).is_empty());
    let mode = with(&|c| c.claude_mode = "bypassPermissions".into());
    assert_eq!(widens(&base, &mode), ["worker_grant.claude_mode"]);
    assert!(widens(&mode, &base).is_empty());
}

#[test]
fn a_raised_grant_limit_needs_the_elevation() {
    let keys = [
        ("worker_grants", "200", "300"),
        ("spawn_cap", "50", "60"),
        ("worker_spawn_cap", "10", "20"),
        ("idle_hours", "24", "48"),
        ("answer_cap", "5000", "6000"),
    ];
    for (key, default, raised) in keys {
        let full = vec![format!("grant_limits.{key}")];
        let old = deck("");
        let at = |value: &str| deck(&format!("[grant_limits]\n{key} = {value}\n"));
        assert_eq!(danger_keys::<DeckHolder>(&old, &at(raised)), full);
        assert_eq!(danger_keys::<DeckHolder>(&old, &at("\"none\"")), full);
        assert!(
            danger_keys::<DeckHolder>(&old, &at("1")).is_empty(),
            "{key}"
        );
        assert!(danger_keys::<DeckHolder>(&at(raised), &at(default)).is_empty());
        assert!(danger_keys::<DeckHolder>(&at("\"none\""), &old).is_empty());
    }
}

#[test]
fn the_default_mode_never_passes_the_station_ceiling() {
    let refused = |text: &str| station_error(text);
    let (field, message) = refused("[default_mode]\nclaude = \"bypassPermissions\"\n");
    assert_eq!(field, "default_mode.claude");
    assert_eq!(
        message,
        "bypassPermissions needs allow_bypass_permissions = true"
    );
    let (field, message) = refused("[default_mode]\ncodex = \"untrusted/danger-full-access\"\n");
    assert_eq!(field, "default_mode.codex");
    assert_eq!(
        message,
        "danger-full-access needs allow_bypass_permissions = true"
    );
    let ok = station(
        "allow_bypass_permissions = true\n[default_mode]\nclaude = \"bypassPermissions\"\ncodex = \"untrusted/danger-full-access\"\n",
    );
    assert_eq!(ok.default_mode.claude.as_deref(), Some("bypassPermissions"));
    assert_eq!(
        ok.default_mode.codex.as_deref(),
        Some("untrusted/danger-full-access")
    );
    // An unknown mode is refused, with or without the switch.
    assert_eq!(
        refused("[default_mode]\nclaude = \"yolo\"\n").0,
        "default_mode.claude"
    );
    assert_eq!(
        refused("[default_mode]\ncodex = \"never\"\n").0,
        "default_mode.codex"
    );
    assert_eq!(
        refused("allow_bypass_permissions = true\n[default_mode]\ncodex = \"x/read-only\"\n").0,
        "default_mode.codex"
    );
    // A save that turns the switch off while a default is a bypass mode is refused.
    assert!(
        StationConfig::parse(
            "allow_bypass_permissions = false\n[default_mode]\nclaude = \"bypassPermissions\"\n",
            &env()
        )
        .is_err()
    );
    // The provider defaults when the table is absent.
    let plain = station("");
    assert_eq!(plain.default_mode.for_family("claude"), Some("default"));
    assert_eq!(
        plain.default_mode.for_family("codex"),
        Some("untrusted/workspace-write")
    );
    assert_eq!(plain.default_mode.for_family("other"), None);
}

#[test]
fn a_more_permissive_default_mode_needs_the_elevation() {
    let old = station("");
    let more = station("[default_mode]\nclaude = \"acceptEdits\"\n");
    assert_eq!(
        danger_keys::<StationHolder>(&old, &more),
        ["default_mode.claude"]
    );
    assert!(danger_keys::<StationHolder>(&more, &old).is_empty());
    let plan = station("[default_mode]\nclaude = \"plan\"\n");
    assert!(danger_keys::<StationHolder>(&old, &plan).is_empty());
    let codex = station("[default_mode]\ncodex = \"on-request/workspace-write\"\n");
    assert_eq!(
        danger_keys::<StationHolder>(&old, &codex),
        ["default_mode.codex"]
    );
    assert!(danger_keys::<StationHolder>(&codex, &old).is_empty());
}

#[test]
fn a_limit_field_saves_a_number_or_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.toml");
    std::fs::write(&path, "name = \"d\"\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&path, None, &Env::default());
    let revision = || view_deck(&path, &[]).unwrap().revision;
    // `"none"` is the TOML string.
    let change = change_with(&revision(), json!({ "grant_limits.spawn_cap": "none" }));
    w.save(&change, true, ok_prepare).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("[grant_limits]") && text.contains("spawn_cap = \"none\""),
        "{text}"
    );
    // A number is a TOML integer.
    let change = change_with(&revision(), json!({ "grant_limits.spawn_cap": 30 }));
    w.save(&change, true, ok_prepare).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("spawn_cap = 30"), "{text}");
    // Every other value is refused, and the file stays.
    for bad in [
        json!("x"),
        json!(100_001),
        json!(0),
        json!(true),
        json!(1.5),
    ] {
        let change = change_with(&revision(), json!({ "grant_limits.spawn_cap": bad }));
        let err = w.save(&change, true, ok_prepare).unwrap_err();
        assert!(
            matches!(&err, SaveError::Invalid { key } if key == "grant_limits.spawn_cap"),
            "{bad}: {err:?}"
        );
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    // The worker keys take the same two forms.
    let change = change_with(&revision(), json!({ "worker_grant.idle_minutes": "none" }));
    let err = w.save(&change, true, ok_prepare).unwrap_err();
    assert!(
        matches!(
            err,
            SaveError::CannotApply { .. } | SaveError::Invalid { .. }
        ),
        "{err:?}"
    );
}

#[test]
fn the_values_show_none_and_the_provider_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.toml");
    std::fs::write(&path, "[grant_limits]\nanswer_cap = \"none\"\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let view = view_deck(&path, &[]).unwrap();
    assert_eq!(view.values["grant_limits.answer_cap"], json!("none"));
    assert_eq!(view.values["grant_limits.spawn_cap"], json!(50));
    assert_eq!(view.values["worker_grant.spawn_cap"], json!(2));
    assert_eq!(view.values["worker_grant.idle_minutes"], json!(30));
    assert_eq!(view.values["worker_grant.allow_bypass"], json!(false));
    let fx = Fixture::new(None);
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    assert_eq!(
        view.values["default_mode.codex"],
        json!("untrusted/workspace-write")
    );
    assert_eq!(view.values["default_mode.claude"], json!("default"));
}

#[test]
fn the_security_fields_follow_the_design_table() {
    let deck = deck_fields();
    let get = |k: &str| deck.iter().find(|f| f.key == k).unwrap().clone();
    for (key, min, max) in [
        ("grant_limits.worker_grants", 1, 100_000),
        ("grant_limits.spawn_cap", 1, 100_000),
        ("grant_limits.worker_spawn_cap", 1, 100_000),
        ("grant_limits.idle_hours", 1, 168),
        ("grant_limits.answer_cap", 1, 100_000),
        ("worker_grant.spawn_cap", 0, 100_000),
        ("worker_grant.idle_minutes", 5, 10_080),
    ] {
        let f = get(key);
        assert_eq!(f.kind, FieldKind::Limit, "{key}");
        assert_eq!((f.min, f.max), (Some(min), Some(max)), "{key}");
        assert!(f.danger, "{key}");
    }
    assert_eq!(get("worker_grant.allow_bypass").kind, FieldKind::Bool);
    assert_eq!(
        get("worker_grant.claude_mode")
            .options
            .last()
            .map(String::as_str),
        Some("bypassPermissions")
    );
    assert_eq!(
        get("worker_grant.codex_mode")
            .options
            .last()
            .map(String::as_str),
        Some("never/danger-full-access")
    );
    let station = station_fields();
    let claude = station
        .iter()
        .find(|f| f.key == "default_mode.claude")
        .unwrap();
    assert_eq!(claude.options.len(), 6);
    let codex = station
        .iter()
        .find(|f| f.key == "default_mode.codex")
        .unwrap();
    assert_eq!(codex.options.len(), 9);
    assert!(claude.danger && codex.danger);
}

#[test]
fn default_within_lowers_the_default_to_the_limit() {
    assert_eq!(default_within(500, Some(1)), 1);
    assert_eq!(default_within(500, Some(5000)), 500);
    assert_eq!(default_within(500, None), 500);
}
