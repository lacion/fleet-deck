//! The save protocol (design 1.3) without injected failures.

use std::os::unix::fs::PermissionsExt;

use fleetdeck_proto::settings::Durability;
use serde_json::json;

use super::{CANARY, Fixture, change_with, inline_station, ok_prepare};
use crate::settings::{
    DeckHolder, SaveError, SettingsWriter, StationHolder, danger_keys, live_keys, restart_keys,
    view_deck,
};

const COMMENTED: &str = "# top note\nname = \"box\"   # the name\n\n# roots\nallowed_roots = [\"/tmp\"]\nmax_sessions = 4 # four\n\n[model_help]\n# keep me\nenabled = true\n";

#[test]
fn a_save_keeps_the_comments_and_the_order() {
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    let change = fx.change(json!({ "max_sessions": 8, "model_help.enabled": false }));
    let (saved, new, ()) = w.save(&change, false, ok_prepare).unwrap();
    let text = fx.text();
    assert!(text.contains("# top note"), "{text}");
    assert!(text.contains("name = \"box\"   # the name"), "{text}");
    assert!(text.contains("max_sessions = 8 # four"), "{text}");
    assert!(text.contains("# keep me\nenabled = false"), "{text}");
    let name_at = text.find("name =").unwrap();
    let roots_at = text.find("allowed_roots").unwrap();
    let max_at = text.find("max_sessions").unwrap();
    assert!(name_at < roots_at && roots_at < max_at);
    assert_eq!(new.max_sessions, 8);
    assert_eq!(saved.values["max_sessions"], json!(8));
    assert_eq!(saved.applied, vec!["max_sessions", "model_help.enabled"]);
    assert!(saved.restart_needed.is_empty() && saved.unapplied.is_empty());
    assert_eq!(saved.durability, Durability::Confirmed);
    assert_eq!(saved.revision, fx.revision());
    assert!(fx.leftovers().is_empty(), "{:?}", fx.leftovers());
    let mode = std::fs::metadata(&fx.config).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn the_header_is_added_once() {
    let fx = Fixture::new(None);
    let w = fx.writer();
    w.save(&fx.change(json!({ "max_sessions": 3 })), false, ok_prepare)
        .unwrap();
    w.save(&fx.change(json!({ "max_sessions": 5 })), false, ok_prepare)
        .unwrap();
    let text = fx.text();
    assert!(text.starts_with("# Edit with the board"), "{text}");
    assert_eq!(text.matches("Edit with the board").count(), 1, "{text}");
    assert!(fx.load().is_ok());
}

#[test]
fn restart_keys_are_listed_apart() {
    let fx = Fixture::new(Some("listen = [\"127.0.0.1:7421\"]\n"));
    let w = fx.writer();
    let change = fx.change(json!({ "listen": ["127.0.0.1:7422"], "hibernate_after_min": 30 }));
    let (saved, _, ()) = w.save(&change, false, ok_prepare).unwrap();
    assert_eq!(saved.applied, vec!["hibernate_after_min"]);
    assert_eq!(saved.restart_needed, vec!["listen"]);
}

#[test]
fn an_old_revision_gives_settings_changed() {
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    let stale = fx.revision();
    w.save(&fx.change(json!({ "max_sessions": 9 })), false, ok_prepare)
        .unwrap();
    let again = w.save(
        &change_with(&stale, json!({ "max_sessions": 2 })),
        false,
        ok_prepare,
    );
    assert_eq!(again.unwrap_err(), SaveError::SettingsChanged);
    assert!(fx.text().contains("max_sessions = 9"));
}

#[test]
fn a_hand_edit_gives_settings_changed_on_disk() {
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    // A hand edit while the holder runs; the board then reads the new text.
    std::fs::write(&fx.config, COMMENTED.replace("four", "edited by hand")).unwrap();
    let err = w
        .save(&fx.change(json!({ "max_sessions": 2 })), false, ok_prepare)
        .unwrap_err();
    assert_eq!(err, SaveError::ChangedOnDisk);
    assert_eq!(err.code(), "settings_changed_on_disk");
    assert!(fx.text().contains("edited by hand") && fx.text().contains("max_sessions = 4"));
    // A hand edit after the board read is caught by the revision.
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    let rev = fx.revision();
    std::fs::write(&fx.config, COMMENTED.replace("four", "edited")).unwrap();
    let err = w
        .save(
            &change_with(&rev, json!({ "max_sessions": 2 })),
            false,
            ok_prepare,
        )
        .unwrap_err();
    assert_eq!(err, SaveError::SettingsChanged);
}

#[test]
fn resync_takes_the_hand_edit_as_the_new_base() {
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    std::fs::write(&fx.config, COMMENTED.replace("= 4", "= 6")).unwrap();
    assert_eq!(w.resync().unwrap().max_sessions, 6);
    w.save(&fx.change(json!({ "max_sessions": 7 })), false, ok_prepare)
        .unwrap();
    assert!(fx.text().contains("max_sessions = 7"));
}

#[test]
fn a_bad_change_names_the_key_and_leaves_the_file() {
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    let before = fx.text();
    let cases = [
        (json!({ "nope": 1 }), "nope"),
        (json!({ "trackers": [] }), "trackers"),
        (json!({ "max_sessions": "many" }), "max_sessions"),
        (json!({ "max_sessions": 0 }), "max_sessions"),
        (json!({ "max_sessions": 257 }), "max_sessions"),
        (json!({ "host": "elsewhere" }), "host"),
        (json!({ "allow_shell": 1 }), "allow_shell"),
        (json!({ "listen": ["not-an-address"] }), "listen"),
        (json!({ "listen": ["0.0.0.0:7421"] }), "listen"),
        (json!({ "name": "bad name!" }), "name"),
        (
            json!({ "offline_approval_policy": "deny_after(0s)" }),
            "offline_approval_policy",
        ),
        (
            json!({ "allowed_roots": ["relative/path"] }),
            "allowed_roots",
        ),
        (json!({ "repo_groups": { "/r": 5 } }), "repo_groups"),
        (
            json!({ "model_help.claude_model": "--bad" }),
            "model_help.claude_model",
        ),
    ];
    for (changes, key) in cases {
        let err = w.save(&fx.change(changes), true, ok_prepare).unwrap_err();
        assert_eq!(
            err,
            SaveError::Invalid {
                key: key.to_owned()
            },
            "{key}"
        );
        assert_eq!(err.code(), "invalid");
        assert_eq!(fx.text(), before);
    }
    assert!(fx.leftovers().is_empty());
}

#[test]
fn an_invalid_message_holds_no_value() {
    let fx = Fixture::new(Some(COMMENTED));
    let err = fx
        .writer()
        .save(
            &fx.change(json!({ "name": "secret value!!" })),
            false,
            ok_prepare,
        )
        .unwrap_err();
    assert!(!err.to_string().contains("secret value"), "{err}");
    assert!(err.to_string().contains("`name`"));
}

#[test]
fn null_removes_a_key_and_the_default_applies() {
    let fx = Fixture::new(Some(
        "max_sessions = 4\n[model_help]\nprovider = \"codex\"\nenabled = false\n",
    ));
    let w = fx.writer();
    let change = fx.change(json!({ "max_sessions": null, "model_help.provider": null }));
    let (saved, new, ()) = w.save(&change, false, ok_prepare).unwrap();
    assert_eq!(new.max_sessions, 12);
    assert!(new.model_help.provider.is_none());
    assert!(!fx.text().contains("max_sessions") && !fx.text().contains("provider ="));
    assert!(fx.text().contains("enabled = false"));
    assert_eq!(saved.values["max_sessions"], json!(12));
}

#[test]
fn a_value_equal_to_the_view_is_not_written() {
    let fx = Fixture::new(Some("allowed_roots = [\"~/work\"]\n"));
    let w = fx.writer();
    let view = crate::settings::view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    let change = change_with(
        &view.revision,
        json!({ "allowed_roots": view.values["allowed_roots"], "max_sessions": 5 }),
    );
    w.save(&change, false, ok_prepare).unwrap();
    assert!(
        fx.text().contains("allowed_roots = [\"~/work\"]"),
        "{}",
        fx.text()
    );
}

#[test]
fn a_danger_change_needs_elevation_before_any_write() {
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    let before = fx.text();
    let change = fx.change(json!({ "allow_shell": true, "allowed_roots": ["/tmp", "/home/x"] }));
    let err = w.save(&change, false, ok_prepare).unwrap_err();
    assert_eq!(
        err,
        SaveError::ElevationRequired {
            keys: vec!["allowed_roots".into(), "allow_shell".into()]
        }
    );
    assert_eq!(fx.text(), before);
    assert!(fx.leftovers().is_empty());
    let (saved, new, ()) = w.save(&change, true, ok_prepare).unwrap();
    assert!(new.allow_shell);
    assert_eq!(saved.applied, vec!["allowed_roots", "allow_shell"]);
}

#[test]
fn prepare_failure_gives_cannot_apply_and_changes_nothing() {
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    let before = fx.text();
    let change = fx.change(json!({ "max_sessions": 9 }));
    let err = w
        .save::<()>(&change, false, |_| {
            Err(("max_sessions".into(), "too many".into()))
        })
        .unwrap_err();
    assert_eq!(
        err,
        SaveError::CannotApply {
            key: "max_sessions".into(),
            message: "too many".into()
        }
    );
    assert_eq!(err.code(), "cannot_apply");
    assert_eq!(fx.text(), before);
    assert!(fx.leftovers().is_empty());
}

#[test]
fn prepare_gets_the_new_config_and_its_value_is_returned() {
    let fx = Fixture::new(Some(COMMENTED));
    let w = fx.writer();
    let (_, new, seen) = w
        .save(&fx.change(json!({ "max_sessions": 9 })), false, |c| {
            Ok(c.max_sessions)
        })
        .unwrap();
    assert_eq!((new.max_sessions, seen), (9, 9));
}

#[test]
fn the_danger_rules() {
    let env = crate::paths::Env {
        home: Some("/home/t".into()),
        ..Default::default()
    };
    let parse = |t: &str| crate::config::StationConfig::parse(t, &env).unwrap();
    let d = |a: &str, b: &str| danger_keys::<StationHolder>(&parse(a), &parse(b));
    assert!(d("", "allow_shell = false").is_empty());
    assert_eq!(d("", "allow_shell = true"), vec!["allow_shell"]);
    assert!(d("allow_shell = true", "allow_shell = false").is_empty());
    assert_eq!(d("", "allow_setup_cmd = true"), vec!["allow_setup_cmd"]);
    assert_eq!(
        d("", "allow_bypass_permissions = true"),
        vec!["allow_bypass_permissions"]
    );
    assert!(
        d(
            "allow_bypass_permissions = true",
            "allow_bypass_permissions = false"
        )
        .is_empty()
    );
    assert!(d("", "allow_bypass_permissions = false").is_empty());
    // Fleet control (ADR-008 §3): turning it on is a danger change, off is not.
    assert_eq!(
        d("", "allow_fleet_control = true"),
        vec!["allow_fleet_control"]
    );
    assert!(d("allow_fleet_control = true", "allow_fleet_control = false").is_empty());
    assert!(d("", "allow_fleet_control = false").is_empty());
    assert_eq!(
        d(
            "allowed_roots = [\"/a\"]",
            "allowed_roots = [\"/a\", \"/b\"]"
        ),
        vec!["allowed_roots"]
    );
    // A new entry is an addition, also inside an old root.
    assert_eq!(
        d("allowed_roots = [\"/a\"]", "allowed_roots = [\"/a/b\"]"),
        vec!["allowed_roots"]
    );
    assert!(
        d(
            "allowed_roots = [\"/a\", \"/b\"]",
            "allowed_roots = [\"/b\"]"
        )
        .is_empty()
    );
    assert_eq!(
        d("listen = [\"127.0.0.1:1\"]", "listen = [\"192.168.1.5:1\"]"),
        vec!["listen"]
    );
    assert!(
        d(
            "listen = [\"192.168.1.5:1\"]",
            "listen = [\"192.168.1.5:1\", \"127.0.0.1:2\"]"
        )
        .is_empty()
    );
    assert!(d("listen = [\"192.168.1.5:1\"]", "listen = [\"127.0.0.1:2\"]").is_empty());
    assert_eq!(d("", "host = \"shim\""), vec!["host"]);
    let policy = |a: &str, b: &str| {
        d(
            &format!("offline_approval_policy = \"{a}\""),
            &format!("offline_approval_policy = \"{b}\""),
        )
    };
    assert_eq!(
        policy("deny_after(5m)", "deny_after(1h)"),
        vec!["offline_approval_policy"]
    );
    assert_eq!(
        policy("deny_after(5m)", "wait"),
        vec!["offline_approval_policy"]
    );
    assert!(policy("wait", "deny_after(5m)").is_empty());
    assert!(policy("deny_after(1h)", "deny_after(30s)").is_empty());
    let inst = |bin: &str, env: &str| {
        format!(
            "[[provider_instance]]\nid = \"a\"\ndriver = \"claude\"\nbinary = \"{bin}\"\nenv = {{ K = \"{env}\" }}\n"
        )
    };
    assert_eq!(
        d(&inst("x", "1"), &inst("y", "1")),
        vec!["provider_instance"]
    );
    assert_eq!(
        d(&inst("x", "1"), &inst("x", "2")),
        vec!["provider_instance"]
    );
    assert!(d(&inst("x", "1"), &inst("x", "1")).is_empty());
    let a = parse("max_sessions = 1\nlisten = [\"127.0.0.1:1\"]");
    let b = parse("max_sessions = 2\nlisten = [\"127.0.0.1:2\"]");
    assert_eq!(live_keys::<StationHolder>(&a, &b), vec!["max_sessions"]);
    assert_eq!(restart_keys::<StationHolder>(&a, &b), vec!["listen"]);
}

#[test]
fn the_deck_holder_saves_and_checks_its_danger_keys() {
    let fx = Fixture::new(None);
    let deck = fx.dir.path().join("cfg").join("deck.toml");
    std::fs::write(&deck, "name = \"d\"\n").unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&deck, None, &fx.env);
    let view = view_deck(&deck, &[]).unwrap();
    let change = change_with(
        &view.revision,
        json!({ "conflict_mail": false, "link_listen": ["192.168.1.9:7421"] }),
    );
    let err = w.save(&change, false, ok_prepare).unwrap_err();
    assert_eq!(
        err,
        SaveError::ElevationRequired {
            keys: vec!["link_listen".into()]
        }
    );
    let change = change_with(
        &view.revision,
        json!({ "conflict_mail": false, "tailnet_hostname": "d.example" }),
    );
    let (saved, new, ()) = w.save(&change, false, ok_prepare).unwrap();
    assert!(!new.conflict_mail);
    assert_eq!(saved.applied, vec!["conflict_mail"]);
    assert_eq!(saved.restart_needed, vec!["tailnet_hostname"]);
    let text = std::fs::read_to_string(&deck).unwrap();
    assert!(text.contains("name = \"d\""), "{text}");
}

#[test]
fn turning_fleet_control_on_needs_elevation_and_off_does_not() {
    let fx = Fixture::new(None);
    let deck = fx.dir.path().join("cfg").join("deck.toml");
    std::fs::write(&deck, "name = \"d\"\n").unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&deck, None, &fx.env);
    let view = view_deck(&deck, &[]).unwrap();
    assert_eq!(view.values["fleet_control"], json!(false));
    let on = change_with(&view.revision, json!({ "fleet_control": true }));
    let err = w.save(&on, false, ok_prepare).unwrap_err();
    assert_eq!(
        err,
        SaveError::ElevationRequired {
            keys: vec!["fleet_control".into()]
        }
    );
    let (saved, new, ()) = w.save(&on, true, ok_prepare).unwrap();
    assert!(new.fleet_control);
    assert_eq!(saved.applied, vec!["fleet_control"]);
    let view = view_deck(&deck, &[]).unwrap();
    let off = change_with(&view.revision, json!({ "fleet_control": false }));
    let (saved, new, ()) = w.save(&off, false, ok_prepare).unwrap();
    assert!(!new.fleet_control);
    assert_eq!(saved.applied, vec!["fleet_control"]);
}

#[test]
fn turning_tailscale_cert_on_needs_elevation_and_off_does_not() {
    let fx = Fixture::new(None);
    let deck = fx.dir.path().join("cfg").join("deck.toml");
    std::fs::write(&deck, "name = \"d\"\n").unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&deck, None, &fx.env);
    let view = view_deck(&deck, &[]).unwrap();
    assert_eq!(view.values["tailscale_cert"], json!(false));
    let on = change_with(&view.revision, json!({ "tailscale_cert": true }));
    let err = w.save(&on, false, ok_prepare).unwrap_err();
    assert_eq!(
        err,
        SaveError::ElevationRequired {
            keys: vec!["tailscale_cert".into()]
        }
    );
    let (saved, new, ()) = w.save(&on, true, ok_prepare).unwrap();
    assert!(new.tailscale_cert);
    assert_eq!(saved.applied, vec!["tailscale_cert"]);
    let view = view_deck(&deck, &[]).unwrap();
    assert_eq!(view.values["tailscale_cert"], json!(true));
    let off = change_with(&view.revision, json!({ "tailscale_cert": false }));
    let (saved, new, ()) = w.save(&off, false, ok_prepare).unwrap();
    assert!(!new.tailscale_cert);
    assert_eq!(saved.applied, vec!["tailscale_cert"]);
}

#[test]
fn a_canary_is_absent_from_every_answer() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    let change = fx.change(json!({ "max_sessions": 3 }));
    let (saved, _, ()) = w.save(&change, false, ok_prepare).unwrap();
    let all = format!("{saved:?} {}", serde_json::to_string(&saved).unwrap());
    assert!(!all.contains(CANARY), "{all}");
    assert_eq!(
        saved.values["provider_instance"][0]["env"]["ANTHROPIC_API_KEY"],
        json!({ "set": true })
    );
    // The file still holds the inline value: only a secret write moves it.
    assert!(fx.text().contains(CANARY));
}

#[test]
fn tailscale_bin_is_a_live_danger_change_in_both_holders() {
    // Any change of the key needs the elevation: a new value, a new path,
    // and also an unset.
    let fx = Fixture::new(Some("tailscale_bin = \"/opt/a/tailscale\"\n"));
    let w = fx.writer();
    for value in [json!("/opt/b/tailscale"), json!(null)] {
        let change = fx.change(json!({ "tailscale_bin": value }));
        let err = w.save(&change, false, ok_prepare).unwrap_err();
        assert_eq!(
            err,
            SaveError::ElevationRequired {
                keys: vec!["tailscale_bin".into()]
            }
        );
    }
    assert!(fx.text().contains("/opt/a/tailscale"));
    let change = fx.change(json!({ "tailscale_bin": "/opt/b/tailscale" }));
    let (saved, new, ()) = w.save(&change, true, ok_prepare).unwrap();
    assert_eq!(saved.applied, vec!["tailscale_bin"]);
    assert!(saved.restart_needed.is_empty());
    assert_eq!(new.tailscale_bin, Some("/opt/b/tailscale".into()));
    assert_eq!(saved.values["tailscale_bin"], json!("/opt/b/tailscale"));
    let change = fx.change(json!({ "tailscale_bin": null }));
    let (saved, new, ()) = w.save(&change, true, ok_prepare).unwrap();
    assert_eq!(new.tailscale_bin, None);
    assert_eq!(saved.values["tailscale_bin"], json!(""));
    // A relative path is refused, also with the elevation.
    let change = fx.change(json!({ "tailscale_bin": "tailscale" }));
    assert!(matches!(
        w.save(&change, true, ok_prepare).unwrap_err(),
        SaveError::Invalid { .. }
    ));
    // The key is in both field tables as a live danger key.
    for fields in [
        crate::settings::station_fields(),
        crate::settings::deck_fields(),
    ] {
        let f = fields.iter().find(|f| f.key == "tailscale_bin").unwrap();
        assert!(f.danger && f.apply == fleetdeck_proto::settings::Apply::Live);
    }

    let deck = fx.dir.path().join("cfg").join("deck.toml");
    std::fs::write(&deck, "name = \"d\"\n").unwrap();
    let w = SettingsWriter::<DeckHolder>::new(&deck, None, &fx.env);
    let view = view_deck(&deck, &[]).unwrap();
    assert_eq!(view.values["tailscale_bin"], json!(""));
    let set = change_with(
        &view.revision,
        json!({ "tailscale_bin": "/opt/ts/tailscale" }),
    );
    assert_eq!(
        w.save(&set, false, ok_prepare).unwrap_err(),
        SaveError::ElevationRequired {
            keys: vec!["tailscale_bin".into()]
        }
    );
    let (saved, new, ()) = w.save(&set, true, ok_prepare).unwrap();
    assert_eq!(saved.applied, vec!["tailscale_bin"]);
    assert_eq!(new.tailscale_bin, Some("/opt/ts/tailscale".into()));
    let view = view_deck(&deck, &[]).unwrap();
    let unset = change_with(&view.revision, json!({ "tailscale_bin": null }));
    assert_eq!(
        w.save(&unset, false, ok_prepare).unwrap_err(),
        SaveError::ElevationRequired {
            keys: vec!["tailscale_bin".into()]
        }
    );
    let (_, new, ()) = w.save(&unset, true, ok_prepare).unwrap();
    assert_eq!(new.tailscale_bin, None);
}
