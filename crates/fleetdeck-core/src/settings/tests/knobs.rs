//! The owner knobs in the settings registry (knobs design sections 2 to 4):
//! the field entries, the elevation rules and the writer round trip.

use std::collections::BTreeSet;

use fleetdeck_proto::settings::{Apply, FieldKind, FieldMeta};
use serde_json::{Map, Value, json};

use super::{Fixture, change_with, ok_prepare};
use crate::config::knobs_tests::{DECK_KNOBS, Knob, STATION_KNOBS};
use crate::config::{DeckConfig, StationConfig};
use crate::paths::Env;
use crate::settings::{
    DeckHolder, Holder, SaveError, SettingsWriter, StationHolder, danger_keys, deck_fields,
    station_fields, view_deck, view_station,
};

fn env() -> Env {
    Env {
        home: Some("/home/t".into()),
        ..Env::default()
    }
}

fn field_of(fields: &[FieldMeta], key: &str) -> FieldMeta {
    fields
        .iter()
        .find(|f| f.key == key)
        .unwrap_or_else(|| panic!("no field `{key}`"))
        .clone()
}

fn check_entries(fields: &[FieldMeta], knobs: &[Knob]) {
    for k in knobs {
        let f = field_of(fields, k.key);
        assert_eq!(f.kind, FieldKind::Int, "{}", k.key);
        assert_eq!(f.min, Some(i64::from(k.min)), "{}", k.key);
        assert_eq!(f.max, Some(i64::from(k.max)), "{}", k.key);
        let apply = if k.restart {
            Apply::Restart
        } else {
            Apply::Live
        };
        assert_eq!(f.apply, apply, "{}", k.key);
        assert_eq!(f.danger, k.danger, "{}", k.key);
        assert!(!f.read_only && !f.secret && !f.hidden, "{}", k.key);
        assert!(!f.label.is_empty(), "{}", k.key);
        assert!(
            f.help.contains(&format!("Default {}.", k.default)),
            "{}: {}",
            k.key,
            f.help
        );
    }
}

#[test]
fn the_deck_knob_entries_follow_the_design_table() {
    let fields = deck_fields();
    check_entries(&fields, DECK_KNOBS);
    // The new entries come last, in the table order.
    let tail: Vec<&str> = fields[fields.len() - DECK_KNOBS.len()..]
        .iter()
        .map(|f| f.key.as_str())
        .collect();
    let want: Vec<&str> = DECK_KNOBS.iter().map(|k| k.key).collect();
    assert_eq!(tail, want);
}

#[test]
fn the_station_knob_entries_follow_the_design_table() {
    let fields = station_fields();
    check_entries(&fields, STATION_KNOBS);
    // The bool knob `analytics.transcripts` is not a number knob: it is not
    // in the table. The `[provider_updates]` fields (B28) come after the knobs.
    let knobs: Vec<&str> = fields
        .iter()
        .map(|f| f.key.as_str())
        .filter(|k| *k != "analytics.transcripts" && !k.starts_with("provider_updates."))
        .collect();
    let tail = &knobs[knobs.len() - STATION_KNOBS.len()..];
    let want: Vec<&str> = STATION_KNOBS.iter().map(|k| k.key).collect();
    assert_eq!(tail, want);
    // Texts of the design that carry a promise.
    assert_eq!(
        field_of(&fields, "retention.raw_log_mib").label,
        "Raw agent logs per session (MiB)"
    );
    assert!(
        field_of(&fields, "retention.raw_log_mib")
            .help
            .contains("It can hold prompt text. 0 turns it off.")
    );
    assert!(
        field_of(&fields, "capture.recording_max_seconds")
            .help
            .contains("A long recording that reaches 256 MiB is discarded.")
    );
}

#[test]
fn the_views_show_every_knob_with_its_default() {
    let fx = Fixture::new(None);
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    for k in STATION_KNOBS {
        assert_eq!(view.values[k.key], json!(k.default), "{}", k.key);
    }
    let deck = fx.dir.path().join("cfg").join("deck.toml");
    let view = view_deck(&deck, &[]).unwrap();
    for k in DECK_KNOBS {
        assert_eq!(view.values[k.key], json!(k.default), "{}", k.key);
    }
    let keys: BTreeSet<String> = view.values.keys().cloned().collect();
    assert_eq!(keys, deck_fields().into_iter().map(|f| f.key).collect());
}

/// The danger keys of a change of one key from its default to `default + delta`.
fn deck_danger(key: &str, default: u32, new: u32) -> (Vec<String>, Vec<String>) {
    let old = DeckConfig::parse("").unwrap();
    let changed = DeckConfig::parse(&format!("{key} = {new}\n")).unwrap();
    assert_ne!(default, new);
    (
        danger_keys::<DeckHolder>(&old, &changed),
        danger_keys::<DeckHolder>(&changed, &old),
    )
}

#[test]
fn a_weaker_deck_knob_needs_the_elevation_and_the_other_direction_does_not() {
    // `true`: a higher value is weaker. `false`: a lower value is weaker.
    let rules = [
        ("security.min_password_chars", false),
        ("security.login_idle_days", true),
        ("security.login_max_days", true),
        ("security.elevation_minutes", true),
        ("security.api_token_max_days", true),
        ("security.display_idle_days", true),
        ("security.display_max_days", true),
        ("security.invite_minutes", true),
        ("security.desktop_screenshots_per_minute", true),
        ("security.desktop_actions_per_minute", true),
        ("retention.audit_days", false),
        ("retention.audit_max_rows", false),
    ];
    assert_eq!(rules.len() + 2, DECK_KNOBS.len());
    for (key, raise_is_weaker) in rules {
        let k = DECK_KNOBS.iter().find(|k| k.key == key).unwrap();
        let (raised, lowered) = (k.default + 1, k.default - 1);
        let (weaker, safer) = if raise_is_weaker {
            (raised, lowered)
        } else {
            (lowered, raised)
        };
        let (forward, back) = deck_danger(key, k.default, weaker);
        assert_eq!(forward, vec![key.to_owned()], "{key} weaker");
        assert!(back.is_empty(), "{key} weaker, reverse: {back:?}");
        let (forward, back) = deck_danger(key, k.default, safer);
        assert!(forward.is_empty(), "{key} safer: {forward:?}");
        assert_eq!(back, vec![key.to_owned()], "{key} safer, reverse");
    }
    // The delivery time and the radar interval never need it.
    for key in ["limits.delivery_hours", "limits.radar_minutes"] {
        let k = DECK_KNOBS.iter().find(|k| k.key == key).unwrap();
        for new in [k.default + 1, k.default - 1] {
            let (forward, back) = deck_danger(key, k.default, new);
            assert!(forward.is_empty() && back.is_empty(), "{key}");
        }
    }
}

#[test]
fn a_weaker_station_knob_needs_the_elevation_and_the_other_direction_does_not() {
    let old = StationConfig::parse("", &env()).unwrap();
    let danger = |key: &str, new: u32| {
        let changed = StationConfig::parse(&format!("{key} = {new}\n"), &env()).unwrap();
        (
            danger_keys::<StationHolder>(&old, &changed),
            danger_keys::<StationHolder>(&changed, &old),
        )
    };
    // A shorter session history needs it; a longer one does not.
    let (forward, back) = danger("retention.history_days", 89);
    assert_eq!(forward, vec!["retention.history_days".to_owned()]);
    assert!(back.is_empty());
    let (forward, back) = danger("retention.history_days", 91);
    assert!(forward.is_empty());
    assert_eq!(back, vec!["retention.history_days".to_owned()]);
    // A longer raw log needs it, also from 0; a shorter one does not.
    let (forward, back) = danger("retention.raw_log_mib", 17);
    assert_eq!(forward, vec!["retention.raw_log_mib".to_owned()]);
    assert!(back.is_empty());
    let (forward, back) = danger("retention.raw_log_mib", 15);
    assert!(forward.is_empty());
    assert_eq!(back, vec!["retention.raw_log_mib".to_owned()]);
    let off = StationConfig::parse("retention.raw_log_mib = 0\n", &env()).unwrap();
    assert!(danger_keys::<StationHolder>(&old, &off).is_empty());
    assert_eq!(
        danger_keys::<StationHolder>(&off, &old),
        vec!["retention.raw_log_mib".to_owned()]
    );
    // Every other station knob never needs it.
    for k in STATION_KNOBS
        .iter()
        .filter(|k| !["retention.history_days", "retention.raw_log_mib"].contains(&k.key))
    {
        for new in [k.default + 1, k.default - 1] {
            let (forward, back) = danger(k.key, new);
            assert!(forward.is_empty() && back.is_empty(), "{}", k.key);
        }
    }
}

fn all_changes(knobs: &[Knob]) -> Value {
    let map: Map<String, Value> = knobs
        .iter()
        .map(|k| (k.key.to_owned(), json!(k.default + 1)))
        .collect();
    Value::Object(map)
}

#[test]
fn every_deck_knob_round_trips_through_the_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.toml");
    std::fs::write(&path, "# my deck\nname = \"d\"\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&path, None, &Env::default());
    let view = view_deck(&path, &[]).unwrap();
    let change = change_with(&view.revision, all_changes(DECK_KNOBS));
    // Most of the knobs are weaker when raised: the elevation is needed.
    let SaveError::ElevationRequired { keys } = w.save(&change, false, ok_prepare).unwrap_err()
    else {
        panic!("expected the elevation");
    };
    assert!(
        keys.contains(&"security.login_max_days".to_owned()),
        "{keys:?}"
    );
    let (saved, new, ()) = w.save(&change, true, ok_prepare).unwrap();
    for k in DECK_KNOBS {
        assert!(
            saved.applied.contains(&k.key.to_owned())
                || saved.restart_needed.contains(&k.key.to_owned()),
            "{}",
            k.key
        );
    }
    assert_eq!(saved.restart_needed, vec!["limits.radar_minutes"]);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# my deck"), "{text}");
    for table in ["[security]", "[retention]", "[limits]"] {
        assert!(text.contains(table), "{table}: {text}");
    }
    let loaded = DeckConfig::load(&path).unwrap();
    assert_eq!(loaded, new);
    let values = DeckHolder::values(&loaded);
    for k in DECK_KNOBS {
        assert_eq!(values[k.key], json!(k.default + 1), "{}", k.key);
    }
    // A removed key is the default again (the weaker ones need the elevation
    // in the other direction only when they get weaker: here they get safer
    // or equal, except the lowered audit and password values).
    let view = view_deck(&path, &[]).unwrap();
    let nulls: Map<String, Value> = DECK_KNOBS
        .iter()
        .map(|k| (k.key.to_owned(), Value::Null))
        .collect();
    let reset = change_with(&view.revision, Value::Object(nulls));
    let (_, back, ()) = w.save(&reset, true, ok_prepare).unwrap();
    assert_eq!(back.security, DeckConfig::parse("").unwrap().security);
    assert_eq!(back.retention, DeckConfig::parse("").unwrap().retention);
    assert_eq!(back.limits, DeckConfig::parse("").unwrap().limits);
}

#[test]
fn every_station_knob_round_trips_through_the_writer() {
    let fx = Fixture::new(Some("# my station\nname = \"box\"\n"));
    let w = fx.writer();
    let change = fx.change(all_changes(STATION_KNOBS));
    // A longer raw log needs the elevation.
    assert_eq!(
        w.save(&change, false, ok_prepare).unwrap_err(),
        SaveError::ElevationRequired {
            keys: vec!["retention.raw_log_mib".into()]
        }
    );
    let (saved, new, ()) = w.save(&change, true, ok_prepare).unwrap();
    let restart: Vec<String> = STATION_KNOBS
        .iter()
        .filter(|k| k.restart)
        .map(|k| k.key.to_owned())
        .collect();
    assert_eq!(saved.restart_needed, restart);
    assert_eq!(
        saved.applied.len() + saved.restart_needed.len(),
        STATION_KNOBS.len()
    );
    let text = fx.text();
    assert!(text.contains("# my station"), "{text}");
    for table in ["[limits]", "[retention]", "[capture]"] {
        assert!(text.contains(table), "{table}: {text}");
    }
    let loaded = fx.load().unwrap();
    assert_eq!(loaded.limits, new.limits);
    assert_eq!(loaded.retention, new.retention);
    assert_eq!(loaded.capture, new.capture);
    let values = StationHolder::values(&loaded);
    for k in STATION_KNOBS {
        assert_eq!(values[k.key], json!(k.default + 1), "{}", k.key);
    }
    let view = view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    for k in STATION_KNOBS {
        assert_eq!(view.values[k.key], json!(k.default + 1), "{}", k.key);
    }
    // A removed key is the default again; a shorter history needs the elevation.
    let nulls: Map<String, Value> = STATION_KNOBS
        .iter()
        .map(|k| (k.key.to_owned(), Value::Null))
        .collect();
    let reset = fx.change(Value::Object(nulls));
    assert_eq!(
        w.save(&reset, false, ok_prepare).unwrap_err(),
        SaveError::ElevationRequired {
            keys: vec!["retention.history_days".into()]
        }
    );
    let (_, back, ()) = w.save(&reset, true, ok_prepare).unwrap();
    assert_eq!(back.limits, crate::config::StationLimits::default());
    assert_eq!(back.retention, crate::config::StationRetention::default());
    assert_eq!(back.capture, crate::config::CaptureLimits::default());
}

#[test]
fn a_save_that_breaks_a_pair_or_a_bound_is_refused_and_leaves_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.toml");
    std::fs::write(&path, "name = \"d\"\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&path, None, &Env::default());
    let view = view_deck(&path, &[]).unwrap();
    // The longest login of 6 days is below the default idle time of 7 days.
    let pair = change_with(&view.revision, json!({ "security.login_max_days": 6 }));
    assert_eq!(
        w.save(&pair, true, ok_prepare).unwrap_err(),
        SaveError::Invalid {
            key: "security.login_idle_days".into()
        }
    );
    // A value past the ceiling is refused by the field range.
    let bound = change_with(&view.revision, json!({ "security.elevation_minutes": 61 }));
    assert_eq!(
        w.save(&bound, true, ok_prepare).unwrap_err(),
        SaveError::Invalid {
            key: "security.elevation_minutes".into()
        }
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "name = \"d\"\n");
}
