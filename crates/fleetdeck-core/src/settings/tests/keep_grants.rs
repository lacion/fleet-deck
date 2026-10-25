//! The deck setting `keep_grants_on_update` (B26).

use serde_json::json;

use super::{Fixture, change_with, ok_prepare};
use crate::config::DeckConfig;
use crate::settings::{DeckHolder, Holder, SaveError, SettingsWriter, deck_fields, view_deck};
use fleetdeck_proto::settings::{Apply, FieldKind};

#[test]
fn keep_grants_on_update_is_on_by_default_and_parses_false() {
    assert!(DeckConfig::parse("").unwrap().keep_grants_on_update);
    let off = DeckConfig::parse("keep_grants_on_update = false\n").unwrap();
    assert!(!off.keep_grants_on_update);
}

#[test]
fn the_field_is_a_live_danger_bool_after_fleet_control() {
    let fields = deck_fields();
    let at = |key: &str| fields.iter().position(|f| f.key == key).unwrap();
    assert_eq!(at("keep_grants_on_update"), at("fleet_control") + 1);
    let f = &fields[at("keep_grants_on_update")];
    assert_eq!(f.kind, FieldKind::Bool);
    assert_eq!(f.apply, Apply::Live);
    assert!(f.danger);
}

#[test]
fn turning_it_on_needs_the_elevation_and_turning_it_off_does_not() {
    let fx = Fixture::new(None);
    let deck = fx.dir.path().join("cfg").join("deck.toml");
    std::fs::write(&deck, "name = \"d\"\nkeep_grants_on_update = false\n").unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&deck, None, &fx.env);
    let view = view_deck(&deck, &[]).unwrap();
    assert_eq!(view.values["keep_grants_on_update"], json!(false));
    let on = change_with(&view.revision, json!({ "keep_grants_on_update": true }));
    let err = w.save(&on, false, ok_prepare).unwrap_err();
    assert_eq!(
        err,
        SaveError::ElevationRequired {
            keys: vec!["keep_grants_on_update".into()]
        }
    );
    let (saved, new, ()) = w.save(&on, true, ok_prepare).unwrap();
    assert!(new.keep_grants_on_update);
    assert_eq!(saved.applied, vec!["keep_grants_on_update"]);
    let view = view_deck(&deck, &[]).unwrap();
    let off = change_with(&view.revision, json!({ "keep_grants_on_update": false }));
    let (saved, new, ()) = w.save(&off, false, ok_prepare).unwrap();
    assert!(!new.keep_grants_on_update);
    assert_eq!(saved.applied, vec!["keep_grants_on_update"]);
}

#[test]
fn the_holder_shows_the_value() {
    let on = DeckConfig::parse("").unwrap();
    assert_eq!(
        DeckHolder::values(&on)["keep_grants_on_update"],
        json!(true)
    );
    let off = DeckConfig::parse("keep_grants_on_update = false\n").unwrap();
    assert_eq!(
        DeckHolder::values(&off)["keep_grants_on_update"],
        json!(false)
    );
}
