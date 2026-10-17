//! Provider instances, secret references, the relay shape and the store.

use std::os::unix::fs::PermissionsExt;

use fleetdeck_proto::settings::SecretSet;
use serde_json::json;

use super::{CANARY, Fixture, change_with, inline_station, ok_prepare};
use crate::settings::SaveError;

const NEW_VALUE: &str = "canary-new-value-9915";

fn set(key: &str, value: &str) -> SecretSet {
    SecretSet {
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn stored(fx: &Fixture, var: &str) -> String {
    let cfg = fx.load().unwrap();
    cfg.provider_instances[0].env[var].expose().clone()
}

#[test]
fn set_secret_turns_an_inline_value_into_a_reference() {
    let fx = Fixture::new(Some(&inline_station()));
    assert!(!fx.store.exists());
    let w = fx.writer();
    assert_eq!(mode(&fx.store), 0o700, "the store is created at start");
    let key = "provider_instance.work.env.ANTHROPIC_API_KEY";
    let (saved, new, ()) = w
        .set_secret(&set(key, NEW_VALUE), &fx.revision(), true, ok_prepare)
        .unwrap();
    let text = fx.text();
    assert!(
        !text.contains(CANARY) && !text.contains(NEW_VALUE),
        "{text}"
    );
    assert!(
        text.contains("{ secret = \"provider-work-anthropic-api-key-"),
        "{text}"
    );
    assert!(text.contains("# my station"));
    let files = fx.secrets();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(crate::settings::refs::valid_secret_name(&files[0]));
    assert_eq!(mode(&fx.store.join(&files[0])), 0o600);
    assert_eq!(
        std::fs::read_to_string(fx.store.join(&files[0])).unwrap(),
        NEW_VALUE
    );
    assert_eq!(
        new.provider_instances[0].env["ANTHROPIC_API_KEY"].expose(),
        NEW_VALUE
    );
    assert_eq!(stored(&fx, "ANTHROPIC_API_KEY"), NEW_VALUE);
    assert_eq!(saved.restart_needed, vec!["provider_instance"]);
    let all = format!("{saved:?} {}", serde_json::to_string(&saved).unwrap());
    assert!(!all.contains(NEW_VALUE) && !all.contains(CANARY), "{all}");
    assert_eq!(
        saved.values["provider_instance"][0]["env"]["ANTHROPIC_API_KEY"],
        json!({ "set": true })
    );
    assert!(fx.leftovers().is_empty());
}

#[test]
fn set_secret_replaces_a_reference_and_deletes_the_old_file_after_the_commit() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    let key = "provider_instance.work.env.ANTHROPIC_API_KEY";
    w.set_secret(&set(key, "first-value"), &fx.revision(), true, ok_prepare)
        .unwrap();
    let first = fx.secrets();
    w.set_secret(&set(key, "second-value"), &fx.revision(), true, ok_prepare)
        .unwrap();
    let second = fx.secrets();
    assert_eq!(second.len(), 1, "{second:?}");
    assert_ne!(first, second, "a new name for each write");
    assert_eq!(stored(&fx, "ANTHROPIC_API_KEY"), "second-value");
}

#[test]
fn set_secret_creates_a_new_variable_of_an_existing_instance() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    w.set_secret(
        &set("provider_instance.work.env.EXTRA_TOKEN", "x-1"),
        &fx.revision(),
        true,
        ok_prepare,
    )
    .unwrap();
    assert_eq!(stored(&fx, "EXTRA_TOKEN"), "x-1");
    assert_eq!(
        stored(&fx, "ANTHROPIC_API_KEY"),
        CANARY,
        "the other inline value stays"
    );
}

#[test]
fn set_secret_on_a_default_instance_writes_the_defaults_out() {
    let fx = Fixture::new(None);
    let w = fx.writer();
    let (_, new, ()) = w
        .set_secret(
            &set("provider_instance.codex.env.OPENAI_API_KEY", "k-1"),
            &fx.revision(),
            true,
            ok_prepare,
        )
        .unwrap();
    let ids: Vec<&str> = new
        .provider_instances
        .iter()
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(ids, vec!["claude", "codex"]);
    assert_eq!(
        new.provider_instances[1].env["OPENAI_API_KEY"].expose(),
        "k-1"
    );
}

#[test]
fn set_secret_refuses_bad_input_and_writes_nothing() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    let before = fx.text();
    let long = "x".repeat(8193);
    let cases = [
        ("provider_instance.nobody.env.K", "v"),
        ("provider_instance.work.env.", "v"),
        ("provider_instance.work.env.1BAD", "v"),
        ("provider_instance.work.env.A.B", "v"),
        ("provider_instance.work.K", "v"),
        ("max_sessions", "v"),
        ("provider_instance.work.env.K", ""),
        ("provider_instance.work.env.K", "a\0b"),
        ("provider_instance.work.env.K", "a\nb"),
        ("provider_instance.work.env.K", &long),
    ];
    for (key, value) in cases {
        let err = w
            .set_secret(&set(key, value), &fx.revision(), true, ok_prepare)
            .unwrap_err();
        assert_eq!(
            err,
            SaveError::Invalid {
                key: key.to_owned()
            },
            "{key}"
        );
        assert!(!err.to_string().contains("a\0b"));
    }
    assert_eq!(fx.text(), before);
    assert!(fx.secrets().is_empty() && fx.leftovers().is_empty());
    let ok = "x".repeat(8192);
    w.set_secret(
        &set("provider_instance.work.env.K", &ok),
        &fx.revision(),
        true,
        ok_prepare,
    )
    .unwrap();
}

#[test]
fn set_secret_needs_elevation_and_the_current_revision() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    let key = "provider_instance.work.env.K";
    let err = w
        .set_secret(&set(key, "v"), &fx.revision(), false, ok_prepare)
        .unwrap_err();
    assert_eq!(
        err,
        SaveError::ElevationRequired {
            keys: vec!["provider_instance".into()]
        }
    );
    let err = w
        .set_secret(&set(key, "v"), "0000", true, ok_prepare)
        .unwrap_err();
    assert_eq!(err, SaveError::SettingsChanged);
    assert!(fx.secrets().is_empty());
}

#[test]
fn set_secret_prepare_failure_removes_the_new_secret() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    let before = fx.text();
    let err = w
        .set_secret::<()>(
            &set("provider_instance.work.env.K", "v"),
            &fx.revision(),
            true,
            |_| Err(("provider_instance".into(), "no".into())),
        )
        .unwrap_err();
    assert_eq!(err.code(), "cannot_apply");
    assert_eq!(fx.text(), before);
    assert!(fx.secrets().is_empty());
}

fn two_instances(
    fx: &Fixture,
) -> (
    crate::settings::SettingsWriter<crate::settings::StationHolder>,
    String,
) {
    let w = fx.writer();
    let a = "provider_instance.work.env.ANTHROPIC_API_KEY";
    w.set_secret(&set(a, "stored-a"), &fx.revision(), true, ok_prepare)
        .unwrap();
    (w, a.to_owned())
}

#[test]
fn an_instance_change_keeps_absent_env_entries_and_the_comments() {
    let text = "[[provider_instance]]\n# the work login\nid = \"work\"\ndriver = \"claude\"   # claude\nenv = { ANTHROPIC_API_KEY = \"plain-1\", OTHER = \"plain-2\" }\n";
    let fx = Fixture::new(Some(text));
    let (w, _) = two_instances(&fx);
    let view = crate::settings::view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
    let change = change_with(
        &view.revision,
        json!({ "provider_instance": [{ "id": "work", "driver": "claude", "binary": "/usr/bin/claude", "env": {} }] }),
    );
    let (saved, new, ()) = w.save(&change, true, ok_prepare).unwrap();
    let t = fx.text();
    assert!(
        t.contains("# the work login") && t.contains("driver = \"claude\"   # claude"),
        "{t}"
    );
    assert_eq!(new.provider_instances[0].env.len(), 2);
    assert_eq!(new.provider_instances[0].env["OTHER"].expose(), "plain-2");
    assert_eq!(
        new.provider_instances[0].binary.to_str(),
        Some("/usr/bin/claude")
    );
    assert_eq!(saved.restart_needed, vec!["provider_instance"]);
}

#[test]
fn an_instance_change_removes_an_env_entry_and_its_secret() {
    let fx = Fixture::new(Some(&inline_station()));
    let (w, _) = two_instances(&fx);
    assert_eq!(fx.secrets().len(), 1);
    let change = fx.change(json!({
        "provider_instance": [{ "id": "work", "driver": "claude", "env": { "ANTHROPIC_API_KEY": null } }]
    }));
    let (_, new, ()) = w.save(&change, true, ok_prepare).unwrap();
    assert!(new.provider_instances[0].env.is_empty());
    assert!(
        fx.secrets().is_empty(),
        "the unreferenced secret is deleted"
    );
}

#[test]
fn removing_an_instance_removes_its_secrets_and_adding_one_works() {
    let fx = Fixture::new(Some(&inline_station()));
    let (w, _) = two_instances(&fx);
    let change = fx.change(json!({
        "provider_instance": [{ "id": "second", "driver": "codex", "binary": "codex" }]
    }));
    let (_, new, ()) = w.save(&change, true, ok_prepare).unwrap();
    assert_eq!(new.provider_instances.len(), 1);
    assert_eq!(new.provider_instances[0].id.as_str(), "second");
    assert!(fx.secrets().is_empty());
}

#[test]
fn an_empty_instance_list_means_no_instance() {
    let fx = Fixture::new(Some(&inline_station()));
    let (_, new, ()) = fx
        .writer()
        .save(
            &fx.change(json!({ "provider_instance": [] })),
            true,
            ok_prepare,
        )
        .unwrap();
    assert!(new.provider_instances.is_empty());
    let (_, new, ()) = fx
        .writer()
        .save(
            &fx.change(json!({ "provider_instance": null })),
            true,
            ok_prepare,
        )
        .unwrap();
    assert_eq!(new.provider_instances.len(), 2, "no key gives the defaults");
}

#[test]
fn a_plain_string_in_an_env_change_is_refused() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    let before = fx.text();
    let change = fx.change(json!({
        "provider_instance": [{ "id": "work", "driver": "claude", "env": { "ANTHROPIC_API_KEY": "canary-typed-in-a-save" } }]
    }));
    let err = w.save(&change, true, ok_prepare).unwrap_err();
    assert_eq!(err.code(), "secret_via_relay");
    assert!(!err.to_string().contains("canary-typed"), "{err}");
    assert_eq!(fx.text(), before);
}

#[test]
fn a_set_marker_needs_an_existing_entry() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    for env in [
        json!({ "NEW_VAR": { "set": true } }),
        json!({ "ANTHROPIC_API_KEY": { "set": false } }),
        json!({ "ANTHROPIC_API_KEY": 5 }),
        json!({ "1BAD": null }),
    ] {
        let change = fx.change(json!({
            "provider_instance": [{ "id": "work", "driver": "claude", "env": env }]
        }));
        let err = w.save(&change, true, ok_prepare).unwrap_err();
        assert!(matches!(err, SaveError::Invalid { .. }), "{err}");
    }
    // A renamed instance cannot carry an entry: the match is by id.
    let change = fx.change(json!({
        "provider_instance": [{ "id": "renamed", "driver": "claude", "env": { "ANTHROPIC_API_KEY": { "set": true } } }]
    }));
    assert!(w.save(&change, true, ok_prepare).is_err());
    assert!(fx.text().contains(CANARY));
    // The marker for an existing entry is accepted.
    let change = fx.change(json!({
        "provider_instance": [{ "id": "work", "driver": "claude", "env": { "ANTHROPIC_API_KEY": { "set": true } } }]
    }));
    w.save(&change, true, ok_prepare).unwrap();
    assert_eq!(stored(&fx, "ANTHROPIC_API_KEY"), CANARY);
}

#[test]
fn an_instance_change_needs_elevation() {
    let fx = Fixture::new(Some(&inline_station()));
    let change = fx.change(json!({ "provider_instance": [{ "id": "work", "driver": "claude", "binary": "/bin/evil" }] }));
    let err = fx.writer().save(&change, false, ok_prepare).unwrap_err();
    assert_eq!(
        err,
        SaveError::ElevationRequired {
            keys: vec!["provider_instance".into()]
        }
    );
}

#[test]
fn the_loader_refuses_a_loose_or_linked_secret() {
    let fx = Fixture::new(Some(&inline_station()));
    let (_w, _) = two_instances(&fx);
    let name = fx.secrets().remove(0);
    let path = fx.store.join(&name);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = fx.load().unwrap_err().to_string();
    assert!(
        err.contains("mode 644") && !err.contains("stored-a"),
        "{err}"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(fx.load().is_ok());
    let real = fx.store.join("real-file");
    std::fs::rename(&path, &real).unwrap();
    std::os::unix::fs::symlink(&real, &path).unwrap();
    let err = fx.load().unwrap_err().to_string();
    assert!(
        err.contains("not a regular file") && !err.contains("stored-a"),
        "{err}"
    );
}

#[test]
fn orphan_collection_deletes_only_unreferenced_files() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = fx.writer();
    assert!(fx.secrets().is_empty());
    w.set_secret(
        &set("provider_instance.work.env.ANTHROPIC_API_KEY", "v-1"),
        &fx.revision(),
        true,
        ok_prepare,
    )
    .unwrap();
    let kept = fx.secrets();
    for name in ["orphan-one", ".tmp-abcdef"] {
        let p = fx.store.join(name);
        std::fs::write(&p, "x").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::create_dir(fx.store.join("a-directory")).unwrap();
    let mut gone = w.collect_orphans();
    gone.sort();
    assert_eq!(
        gone,
        vec![".tmp-abcdef".to_owned(), "orphan-one".to_owned()]
    );
    let mut left = fx.secrets();
    left.retain(|n| n != "a-directory");
    assert_eq!(left, kept);
    assert_eq!(stored(&fx, "ANTHROPIC_API_KEY"), "v-1");
    assert!(w.collect_orphans().is_empty());
}
