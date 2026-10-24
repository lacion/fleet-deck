use super::*;
use crate::config::StationConfig;
use crate::paths::Env;

fn env() -> Env {
    Env {
        home: Some("/home/ana".into()),
        ..Env::default()
    }
}

fn parse(text: &str) -> Result<StationConfig, ConfigError> {
    StationConfig::parse(text, &env())
}

#[test]
fn the_defaults_are_off_at_three_for_two_hours() {
    let c = parse("").unwrap().provider_updates;
    assert_eq!(c, ProviderUpdatesConfig::default());
    assert!(!c.enabled);
    assert_eq!(c.window_start_min, 180);
    assert_eq!(c.window_hours, 2);
    assert_eq!(c.timezone, UpdateZone::Local);
    assert_eq!(c.claude_channel, ClaudeChannel::Stable);
    assert_eq!((c.claude_pin, c.codex_pin), (None, None));
    assert_eq!(c.min_age_hours, 48);
}

#[test]
fn a_full_table_is_read() {
    let text = r#"
[provider_updates]
enabled = true
window_start = "23:30"
window_hours = 12
timezone = "-05:30"
claude_channel = "latest"
claude_pin = "2.1.305"
codex_pin = "0.163.0"
min_age_hours = 336
"#;
    let c = parse(text).unwrap().provider_updates;
    assert!(c.enabled);
    assert_eq!(c.window_start_min, 23 * 60 + 30);
    assert_eq!(c.window_start_text(), "23:30");
    assert_eq!(c.window_hours, 12);
    assert_eq!(c.timezone, UpdateZone::Fixed(-(5 * 60 + 30)));
    assert_eq!(c.timezone.text(), "-05:30");
    assert_eq!(c.claude_channel, ClaudeChannel::Latest);
    assert_eq!(c.claude_pin.as_deref(), Some("2.1.305"));
    assert_eq!(c.codex_pin.as_deref(), Some("0.163.0"));
    assert_eq!(c.min_age_hours, 336);
}

#[test]
fn the_zone_has_three_forms_and_an_empty_pin_is_no_pin() {
    let zone = |z: &str| {
        parse(&format!("[provider_updates]\ntimezone = \"{z}\"\n"))
            .unwrap()
            .provider_updates
            .timezone
    };
    assert_eq!(zone("local"), UpdateZone::Local);
    assert_eq!(zone("UTC"), UpdateZone::Fixed(0));
    assert_eq!(UpdateZone::Fixed(0).text(), "UTC");
    assert_eq!(zone("+14:00"), UpdateZone::Fixed(14 * 60));
    assert_eq!(UpdateZone::Local.text(), "local");
    let c = parse("[provider_updates]\nclaude_pin = \"\"\n").unwrap();
    assert_eq!(c.provider_updates.claude_pin, None);
}

#[test]
fn each_bad_value_names_its_field() {
    let bad = [
        ("window_start = \"24:00\"", "provider_updates.window_start"),
        ("window_start = \"3:00\"", "provider_updates.window_start"),
        ("window_hours = 0", "provider_updates.window_hours"),
        ("window_hours = 13", "provider_updates.window_hours"),
        ("timezone = \"+15:00\"", "provider_updates.timezone"),
        ("timezone = \"CET\"", "provider_updates.timezone"),
        (
            "claude_channel = \"beta\"",
            "provider_updates.claude_channel",
        ),
        ("claude_pin = \"2.1\"", "provider_updates.claude_pin"),
        ("codex_pin = \"--help\"", "provider_updates.codex_pin"),
        ("min_age_hours = 337", "provider_updates.min_age_hours"),
    ];
    for (line, field) in bad {
        let err = parse(&format!("[provider_updates]\n{line}\n")).unwrap_err();
        assert!(err.to_string().contains(field), "{line}: {err}");
    }
}

#[test]
fn an_unknown_key_is_refused() {
    assert!(parse("[provider_updates]\nnope = 1\n").is_err());
}
