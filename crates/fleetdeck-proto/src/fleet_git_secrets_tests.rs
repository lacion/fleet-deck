// Tests of the secret file wire types (Fleet Sync FS-3).
use super::*;
use serde_json::json;

#[test]
fn the_path_rule_has_a_table() {
    for ok in [
        ".env",
        ".env.local",
        "apps/web/.env.local",
        "keys/server.pem",
        "a/b/secrets.json",
        "config/credentials.prod.json",
        "A/.ENV",
    ] {
        assert_eq!(check_secret_path(ok), Ok(()), "{ok}");
    }
    for bad in [
        "",
        "/.env",
        "a//.env",
        "a/.env/",
        "./.env",
        "a/./.env",
        "../.env",
        "a/../.env",
        "a\\.env",
        ".env\0",
        ".git/.env",
        "a/.GIT/x.pem",
        ".Git",
    ] {
        assert_eq!(
            check_secret_path(bad),
            Err(SecretPathError::Invalid),
            "{bad:?}"
        );
    }
    let long = format!("{}/.env", "a".repeat(MAX_SECRET_PATH));
    assert_eq!(check_secret_path(&long), Err(SecretPathError::Invalid));
    let edge = format!("{}{}", "a/".repeat(125), ".env");
    assert_eq!(edge.len(), 254);
    assert_eq!(check_secret_path(&edge), Ok(()));
    for visible in ["README.md", "src/main.rs", "config/app.yml", "envfile"] {
        assert_eq!(
            check_secret_path(visible),
            Err(SecretPathError::NotMemberHidden),
            "{visible}"
        );
    }
}

#[test]
fn the_limits_have_their_values() {
    assert_eq!(MAX_SECRET_FILES, 20);
    assert_eq!(MAX_SECRET_PATH, 256);
    assert_eq!(MAX_SECRET_BYTES, 524_288);
}

fn key() -> SecretFileKey {
    SecretFileKey {
        path: ".env".into(),
        salt: "ab".repeat(32),
    }
}

fn round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value);
    // An unknown field is refused.
    let mut extra = json;
    extra
        .as_object_mut()
        .unwrap()
        .insert("surprise".into(), json!(1));
    assert!(serde_json::from_value::<T>(extra).is_err());
}

#[test]
fn every_wire_type_round_trips_and_refuses_unknown_fields() {
    round_trip(&key());
    round_trip(&SecretScanParams {
        project_id: "p1".into(),
        generation: 3,
        dest: "https://github.com:443/acme/app".into(),
        remote_name: "origin".into(),
        files: vec![key()],
    });
    let state = SecretFileState {
        path: ".env".into(),
        state: SecretPresence::Present,
        code: None,
        digest: Some("cd".repeat(32)),
        size: Some(12),
        mtime_ms: Some(1_700_000_000_000),
    };
    round_trip(&state);
    round_trip(&SecretFileState {
        path: ".env".into(),
        state: SecretPresence::Refused,
        code: Some("not_ignored".into()),
        digest: None,
        size: None,
        mtime_ms: None,
    });
    round_trip(&SecretScanAnswer {
        files: vec![state.clone()],
    });
    round_trip(&SecretReadParams {
        project_id: "p1".into(),
        generation: 3,
        dest: "d".into(),
        remote_name: "origin".into(),
        path: ".env".into(),
        salt: "ab".repeat(32),
        digest: "cd".repeat(32),
    });
    round_trip(&SecretReadAnswer {
        content_b64: "QT0x".into(),
    });
    round_trip(&SecretWriteParams {
        project_id: "p1".into(),
        generation: 3,
        dest: "d".into(),
        remote_name: "origin".into(),
        path: ".env".into(),
        salt: "ab".repeat(32),
        digest: "cd".repeat(32),
        content_b64: "QT0x".into(),
        expect: Some("ef".repeat(32)),
    });
    round_trip(&SecretWriteAnswer { backup: true });
    let station = SecretStationReport {
        station_id: "st-1".into(),
        state: SecretStationState::Failed,
        code: Some("changed_meanwhile".into()),
        message: Some("The file changed during the sync.".into()),
        mtime_ms: Some(5),
        size: Some(9),
    };
    round_trip(&station);
    let report = SecretFileReport {
        path: ".env".into(),
        synced_at: Some(7),
        source: Some("st-1".into()),
        conflict: true,
        outcome: SecretOutcome::Partial,
        stations: vec![station],
    };
    round_trip(&report);
    round_trip(&SecretFilesView {
        paths: vec![".env".into()],
        files: vec![report.clone()],
    });
    round_trip(&SecretSyncReport {
        files: vec![report],
    });
}

#[test]
fn an_absent_option_is_left_out_and_read_as_none() {
    let write = SecretWriteParams {
        project_id: "p1".into(),
        generation: 1,
        dest: "d".into(),
        remote_name: "origin".into(),
        path: ".env".into(),
        salt: "ab".repeat(32),
        digest: "cd".repeat(32),
        content_b64: String::new(),
        expect: None,
    };
    let json = serde_json::to_value(&write).unwrap();
    assert!(json.get("expect").is_none());
    let back: SecretWriteParams = serde_json::from_value(json).unwrap();
    assert_eq!(back.expect, None);
}

#[test]
fn a_state_of_a_newer_station_reads_as_unknown() {
    let got: SecretPresence = serde_json::from_value(json!("moved")).unwrap();
    assert_eq!(got, SecretPresence::Unknown);
    let got: SecretStationState = serde_json::from_value(json!("different")).unwrap();
    assert_eq!(got, SecretStationState::Unknown);
    let got: SecretOutcome = serde_json::from_value(json!("later")).unwrap();
    assert_eq!(got, SecretOutcome::Unknown);
    assert_eq!(
        serde_json::to_value(SecretOutcome::NotRun).unwrap(),
        json!("not_run")
    );
}

#[test]
fn the_debug_of_a_content_type_never_prints_the_content() {
    let sentinel = "U0VOVElORUwtQ09OVEVOVA==";
    let read = SecretReadAnswer {
        content_b64: sentinel.into(),
    };
    let text = format!("{read:?} {read:#?}");
    assert!(!text.contains(sentinel), "{text}");
    assert!(text.contains("<redacted>"));
    let write = SecretWriteParams {
        project_id: "p1".into(),
        generation: 1,
        dest: "d".into(),
        remote_name: "origin".into(),
        path: ".env".into(),
        salt: "ab".repeat(32),
        digest: "cd".repeat(32),
        content_b64: sentinel.into(),
        expect: Some("ef".repeat(32)),
    };
    let text = format!("{write:?} {write:#?}");
    assert!(!text.contains(sentinel), "{text}");
    assert!(!text.contains(&"ab".repeat(32)), "{text}");
    assert!(!text.contains(&"cd".repeat(32)), "{text}");
    assert!(!text.contains(&"ef".repeat(32)), "{text}");
    assert!(text.contains("<redacted>"));
    // The serde output still carries the content (it is the wire shape).
    assert!(serde_json::to_string(&write).unwrap().contains(sentinel));
}

#[test]
fn the_secrets_feature_is_offered_and_not_required() {
    use crate::version::feature;
    assert_eq!(feature::FLEET_GIT_SECRETS, "fleet_git.secrets.v1");
    assert!(feature::ALL.contains(&"fleet_git.secrets.v1"));
    assert!(!feature::REQUIRED.contains(&"fleet_git.secrets.v1"));
}
