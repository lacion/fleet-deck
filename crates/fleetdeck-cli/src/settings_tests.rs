#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;

use fleetdeck_proto::settings::{Durability, FieldKind, FieldMeta};
use serde_json::json;

use super::*;

#[test]
fn a_value_that_parses_as_json_is_json_else_a_string() {
    assert_eq!(parse_value("true"), json!(true));
    assert_eq!(parse_value("5"), json!(5));
    assert_eq!(parse_value("-1"), json!(-1));
    assert_eq!(parse_value(r#"["a","b"]"#), json!(["a", "b"]));
    assert_eq!(parse_value("null"), Value::Null);
    assert_eq!(parse_value("hello"), json!("hello"));
    assert_eq!(parse_value("/home/me/work"), json!("/home/me/work"));
    assert_eq!(parse_value("\"quoted\""), json!("quoted"));
    assert_eq!(parse_value(""), json!(""));
    assert_eq!(parse_value("[unclosed"), json!("[unclosed"));
}

#[test]
fn the_secret_line_loses_only_its_line_end() {
    assert_eq!(
        value_line("tok en \nrest\n".as_bytes()).unwrap().expose(),
        "tok en "
    );
    assert_eq!(value_line("abc\r\n".as_bytes()).unwrap().expose(), "abc");
    assert_eq!(value_line("abc".as_bytes()).unwrap().expose(), "abc");
    assert!(value_line("\n".as_bytes()).is_err());
    assert!(value_line("".as_bytes()).is_err());
}

#[test]
fn a_huge_value_is_refused_without_echoing_it() {
    let huge = "x".repeat(10_000);
    let e = format!("{:#}", value_line(huge.as_bytes()).unwrap_err());
    assert!(!e.contains("xxxx"), "{e}");
}

fn field(key: &str, apply: Apply, secret: bool) -> FieldMeta {
    FieldMeta {
        key: key.to_owned(),
        label: key.to_owned(),
        help: String::new(),
        kind: FieldKind::Text,
        min: None,
        max: None,
        options: Vec::new(),
        apply,
        danger: false,
        secret,
        read_only: false,
        hidden: false,
        cli_flag: None,
    }
}

fn view(revision: &str) -> SettingsView {
    let mut values = Map::new();
    values.insert("name".into(), json!("laptop"));
    values.insert("conflict_mail".into(), json!(true));
    values.insert(
        "provider_instance".into(),
        json!({ "work": { "env": { "TOKEN": { "set": true, "digest": "ab" } } } }),
    );
    SettingsView {
        values,
        fields: vec![
            field("name", Apply::Live, false),
            field("conflict_mail", Apply::Live, false),
            field("link_listen", Apply::Restart, false),
            field("provider_instance", Apply::Restart, true),
        ],
        revision: revision.to_owned(),
        overrides: Vec::new(),
        relaxed: None,
    }
}

#[test]
fn the_table_shows_key_value_and_apply_and_hides_secrets() {
    let text = render_view(&view("r1"), false).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("key"), "{text}");
    assert!(lines[0].ends_with("apply"), "{text}");
    assert!(lines[1].contains("name") && lines[1].contains("laptop") && lines[1].ends_with("live"));
    assert!(lines[2].contains("true"), "{text}");
    assert!(
        lines[3].contains("link_listen") && lines[3].contains('-') && lines[3].ends_with("restart")
    );
    assert!(lines[4].contains("(set)"), "{text}");
    assert!(!text.contains("digest"), "{text}");
}

#[test]
fn json_output_is_the_view() {
    let v = view("r1");
    let text = render_view(&v, true).unwrap();
    assert_eq!(serde_json::from_str::<SettingsView>(&text).unwrap(), v);
}

fn saved(restart: &[&str]) -> SettingsSaved {
    SettingsSaved {
        values: Map::new(),
        applied: Vec::new(),
        restart_needed: restart.iter().map(|s| (*s).to_owned()).collect(),
        unapplied: Vec::new(),
        revision: "r2".into(),
        durability: Durability::Confirmed,
        relaxed: Vec::new(),
    }
}

#[test]
fn the_saved_text_says_live_or_restart() {
    assert_eq!(render_saved(&saved(&[])), "Saved and applied.");
    assert_eq!(
        render_saved(&saved(&["link_listen", "listen"])),
        "Saved. Applies after a restart: link_listen, listen."
    );
}

/// A holder whose revision moves once under the first save.
struct Racing {
    revision: RefCell<u32>,
    saves: RefCell<Vec<(String, String)>>,
    race_times: u32,
}

impl Racing {
    fn new(race_times: u32) -> Self {
        Self {
            revision: RefCell::new(1),
            saves: RefCell::default(),
            race_times,
        }
    }

    fn attempt(&self, revision: &str, what: &str) -> Result<SettingsSaved, Failure> {
        self.saves
            .borrow_mut()
            .push((revision.to_owned(), what.to_owned()));
        if *self.revision.borrow() <= self.race_times {
            *self.revision.borrow_mut() += 1;
            return Err(Failure::Settings {
                code: CHANGED.to_owned(),
                message: "the settings changed since you read them".to_owned(),
            });
        }
        Ok(saved(&[]))
    }
}

impl Holder for Racing {
    async fn view(&self) -> Result<SettingsView, Failure> {
        Ok(view(&format!("r{}", self.revision.borrow())))
    }

    async fn save(&self, change: SettingsChange) -> Result<SettingsSaved, Failure> {
        let keys: Vec<&str> = change.changes.keys().map(String::as_str).collect();
        self.attempt(&change.revision, &keys.join(","))
    }

    async fn secret(&self, secret: SecretSet, revision: String) -> Result<SettingsSaved, Failure> {
        self.attempt(&revision, &secret.key)
    }
}

#[tokio::test]
async fn an_old_revision_is_retried_once_with_the_new_one() {
    let h = Racing::new(1);
    set(&h, "name", json!("x")).await.unwrap();
    assert_eq!(
        *h.saves.borrow(),
        vec![("r1".into(), "name".into()), ("r2".into(), "name".into())]
    );
}

#[tokio::test]
async fn a_second_conflict_is_an_error() {
    let h = Racing::new(2);
    let e = set(&h, "name", json!("x")).await.unwrap_err();
    assert!(
        matches!(e, Failure::Settings { ref code, .. } if code == CHANGED),
        "{e:?}"
    );
    assert_eq!(h.saves.borrow().len(), 2, "one retry, not more");
    let text = anyhow::Error::from(e).to_string();
    assert!(text.starts_with("settings_changed: "), "{text}");
}

#[tokio::test]
async fn a_secret_is_retried_once_and_never_shown_in_the_failure() {
    let h = Racing::new(1);
    let value = Secret::new("canary-secret-1".to_owned());
    secret(&h, "provider_instance.work.env.TOKEN", &value)
        .await
        .unwrap();
    assert_eq!(h.saves.borrow().len(), 2);
    let h = Racing::new(2);
    let e = secret(&h, "provider_instance.work.env.TOKEN", &value)
        .await
        .unwrap_err();
    let debug = format!("{e:?}");
    let text = format!("{debug} {:#}", anyhow::Error::from(e));
    assert!(!text.contains("canary-secret-1"), "{text}");
}

#[tokio::test]
async fn another_error_is_not_retried() {
    struct Invalid;
    impl Holder for Invalid {
        async fn view(&self) -> Result<SettingsView, Failure> {
            Ok(view("r1"))
        }
        async fn save(&self, _: SettingsChange) -> Result<SettingsSaved, Failure> {
            Err(Failure::Settings {
                code: "invalid".into(),
                message: "`name` has an invalid value".into(),
            })
        }
        async fn secret(&self, _: SecretSet, _: String) -> Result<SettingsSaved, Failure> {
            unreachable!()
        }
    }
    let e = set(&Invalid, "name", json!(1)).await.unwrap_err();
    assert_eq!(
        anyhow::Error::from(e).to_string(),
        "invalid: `name` has an invalid value"
    );
}
