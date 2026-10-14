//! The deck keys of "Use Tailscale automatically" (tailnet design 14.4):
//! the field table, the danger rule and the parser.

use fleetdeck_proto::settings::Apply;
use serde_json::json;

use super::{change_with, ok_prepare};
use crate::config::DeckConfig;
use crate::settings::{DeckHolder, SaveError, SettingsWriter, deck_fields, view_deck};

fn deck_writer(
    text: &str,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    SettingsWriter<DeckHolder>,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.toml");
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&path, None, &crate::paths::Env::default());
    (dir, path, w)
}

#[test]
fn the_field_table_has_the_three_keys() {
    let fields = deck_fields();
    let get = |k: &str| fields.iter().find(|f| f.key == k).unwrap().clone();
    let auto = get("tailnet_auto");
    assert!(auto.danger && auto.apply == Apply::Restart && !auto.hidden);
    for key in ["tailnet_auto_listen", "tailnet_auto_link_listen"] {
        let f = get(key);
        // Writable, restart, not danger, and hidden from the board.
        assert!(f.hidden && !f.danger && !f.read_only, "{key}");
        assert_eq!(f.apply, Apply::Restart, "{key}");
    }
    // No other deck field is hidden.
    assert_eq!(fields.iter().filter(|f| f.hidden).count(), 2);
}

#[test]
fn the_mode_defaults_to_on_and_the_lists_to_empty() {
    let c = DeckConfig::parse("").unwrap();
    assert!(c.tailnet_auto);
    // A saved `false` stays `false`.
    assert!(
        !DeckConfig::parse("tailnet_auto = false\n")
            .unwrap()
            .tailnet_auto
    );
    assert!(c.tailnet_auto_listen.is_empty() && c.tailnet_auto_link_listen.is_empty());
    let c = DeckConfig::parse(
        "tailnet_auto = true\ntailnet_auto_listen = [\"100.64.0.5:7420\"]\ntailnet_auto_link_listen = [\"[fd7a:115c:a1e0::5]:7421\"]\n",
    )
    .unwrap();
    assert!(c.tailnet_auto);
    assert_eq!(
        c.tailnet_auto_listen,
        vec!["100.64.0.5:7420".parse().unwrap()]
    );
    assert_eq!(
        c.tailnet_auto_link_listen,
        vec!["[fd7a:115c:a1e0::5]:7421".parse().unwrap()]
    );
    // An entry follows the bind rule of the listen lists.
    let e = DeckConfig::parse("tailnet_auto_listen = [\"8.8.8.8:7420\"]\n").unwrap_err();
    assert!(e.to_string().contains("tailnet_auto_listen"), "{e}");
}

#[test]
fn the_view_shows_the_values_of_the_three_keys() {
    let (_dir, path, _w) =
        deck_writer("tailnet_auto = true\ntailnet_auto_listen = [\"100.64.0.5:7420\"]\n");
    let view = view_deck(&path, &[]).unwrap();
    assert_eq!(view.values["tailnet_auto"], json!(true));
    assert_eq!(
        view.values["tailnet_auto_listen"],
        json!(["100.64.0.5:7420"])
    );
    assert_eq!(view.values["tailnet_auto_link_listen"], json!([]));
}

#[test]
fn turning_the_mode_on_needs_the_elevation_and_off_does_not() {
    let (_dir, path, w) = deck_writer("name = \"d\"\ntailnet_auto = false\n");
    let view = view_deck(&path, &[]).unwrap();
    let on = change_with(&view.revision, json!({ "tailnet_auto": true }));
    assert_eq!(
        w.save(&on, false, ok_prepare).unwrap_err(),
        SaveError::ElevationRequired {
            keys: vec!["tailnet_auto".into()]
        }
    );
    let (saved, new, ()) = w.save(&on, true, ok_prepare).unwrap();
    assert!(new.tailnet_auto);
    assert_eq!(saved.restart_needed, vec!["tailnet_auto"]);
    let view = view_deck(&path, &[]).unwrap();
    let off = change_with(&view.revision, json!({ "tailnet_auto": false }));
    let (_, new, ()) = w.save(&off, false, ok_prepare).unwrap();
    assert!(!new.tailnet_auto);
}

#[test]
fn an_absent_key_is_on_and_a_reset_of_a_saved_false_needs_the_elevation() {
    // Absent: on. The view shows the effective value.
    let (_dir, path, w) = deck_writer("name = \"d\"\n");
    let view = view_deck(&path, &[]).unwrap();
    assert_eq!(view.values["tailnet_auto"], json!(true));
    // On to the default (`null`), and on to `true`: no elevation.
    let c = change_with(&view.revision, json!({ "tailnet_auto": null }));
    w.save(&c, false, ok_prepare).unwrap();
    let view = view_deck(&path, &[]).unwrap();
    let c = change_with(&view.revision, json!({ "tailnet_auto": true }));
    w.save(&c, false, ok_prepare).unwrap();
    // A saved `false` reset to the default (on) needs the elevation.
    let (_dir, path, w) = deck_writer("name = \"d\"\ntailnet_auto = false\n");
    let view = view_deck(&path, &[]).unwrap();
    assert_eq!(view.values["tailnet_auto"], json!(false));
    let reset = change_with(&view.revision, json!({ "tailnet_auto": null }));
    assert_eq!(
        w.save(&reset, false, ok_prepare).unwrap_err(),
        SaveError::ElevationRequired {
            keys: vec!["tailnet_auto".into()]
        }
    );
    let (_, new, ()) = w.save(&reset, true, ok_prepare).unwrap();
    assert!(new.tailnet_auto);
    // Off from on does not need it.
    let (_dir, path, w) = deck_writer("name = \"d\"\n");
    let view = view_deck(&path, &[]).unwrap();
    let off = change_with(&view.revision, json!({ "tailnet_auto": false }));
    let (_, new, ()) = w.save(&off, false, ok_prepare).unwrap();
    assert!(!new.tailnet_auto);
}

#[test]
fn the_provenance_lists_save_without_the_elevation() {
    let (_dir, path, w) = deck_writer("name = \"d\"\n");
    let view = view_deck(&path, &[]).unwrap();
    let set = change_with(
        &view.revision,
        json!({
            "tailnet_auto_listen": ["100.64.0.5:7420"],
            "tailnet_auto_link_listen": ["100.64.0.5:7421"],
        }),
    );
    let (saved, new, ()) = w.save(&set, false, ok_prepare).unwrap();
    assert_eq!(new.tailnet_auto_listen.len(), 1);
    assert_eq!(new.tailnet_auto_link_listen.len(), 1);
    assert_eq!(
        saved.restart_needed,
        vec!["tailnet_auto_listen", "tailnet_auto_link_listen"]
    );
    // An empty list is written as an empty list.
    let view = view_deck(&path, &[]).unwrap();
    let clear = change_with(
        &view.revision,
        json!({ "tailnet_auto_listen": [], "tailnet_auto_link_listen": [] }),
    );
    let (_, new, ()) = w.save(&clear, false, ok_prepare).unwrap();
    assert!(new.tailnet_auto_listen.is_empty() && new.tailnet_auto_link_listen.is_empty());
}
