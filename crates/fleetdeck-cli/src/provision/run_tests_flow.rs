//! The run: the file, the release check, the configuration, the service and
//! the station.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use super::effects::{CallError, ConfigState, ServiceState, Terminal};
use super::fake::{Fake, JoinStep, STATION_FP, STATION_FP_2, file_text, go, write_state};
use super::release::{DESKTOP_PATH, MANIFEST_PATH};
use super::run::Exit;
use super::sha256_hex;
use super::state::Phase;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn other_deck_file() -> Vec<u8> {
    String::from_utf8(file_text("laptop"))
        .unwrap()
        .replace("deck.example.ts.net", "other.example.ts.net")
        .into_bytes()
}

#[tokio::test]
async fn a_file_that_is_not_valid_is_file_invalid_and_nothing_is_saved() {
    let valid = String::from_utf8(file_text("laptop")).unwrap();
    let cases = [
        "not toml".to_owned(),
        valid.replace("version = 1", "version = 2"),
        valid.replace("\"laptop\"", "\"\""),
        valid.replace("\"laptop\"", &format!("\"{}\"", "x".repeat(65))),
        valid.replace("\"laptop\"", "\"a\\nb\""),
        valid.replace("deck.example.ts.net:7421", "deck.example.ts.net"),
        valid.replace("deck.example.ts.net:7421", "deck.example.ts.net:0"),
        valid.replace("deck.example.ts.net:7421", " deck.example.ts.net:7421"),
        valid.replace("deck.example.ts.net:7421", "fd7a::5:7421"),
        valid.replace("deck.example.ts.net:7421", "[fd7a::5]"),
        format!("{valid}extra = 1\n"),
    ];
    for text in cases {
        let fake = Fake::new();
        *fake.provision.lock().unwrap() = Ok(text.clone().into_bytes());
        assert_eq!(go(&fake, false).await, Exit::Terminal, "{text}");
        assert_eq!(
            fake.lines(),
            ["provision: validated: file_invalid"],
            "{text}"
        );
        assert!(fake.state().is_none());
        assert!(fake.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_file_that_cannot_be_read_is_the_terminal_of_the_reader() {
    let fake = Fake::new();
    *fake.provision.lock().unwrap() = Err(Terminal::FILE_INVALID);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(fake.lines(), ["provision: validated: file_invalid"]);
    assert!(fake.state().is_none());
}

#[tokio::test]
async fn an_ipv6_deck_in_brackets_is_a_canonical_target() {
    let fake = Fake::new();
    let text = String::from_utf8(file_text("laptop"))
        .unwrap()
        .replace("deck.example.ts.net:7421", "[fd7a:115c:a1e0::5]:9000");
    *fake.provision.lock().unwrap() = Ok(text.into_bytes());
    assert_eq!(go(&fake, false).await, Exit::Complete);
    let args = fake.join_args.lock().unwrap().clone();
    assert_eq!(args[0].0, "fd7a:115c:a1e0::5");
    assert_eq!(args[0].1, 9000);
}

#[tokio::test]
async fn a_file_error_keeps_an_older_state_file_unchanged() {
    let fake = Fake::new();
    let before = r#"{"version":1,"config_sha256":"x","station_fingerprint":null,"phase":"service","error":null}"#;
    let path = write_state(&fake, before.as_bytes(), 0o600);
    *fake.provision.lock().unwrap() = Ok(b"nope".to_vec());
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(std::fs::read(&path).unwrap(), before.as_bytes());
}

#[tokio::test]
async fn an_untrusted_release_is_a_saved_terminal_before_any_effect() {
    let fake = Fake::new();
    fake.edit_file(MANIFEST_PATH, |f| f.uid = 1000);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert!(fake.calls.lock().unwrap().is_empty());
    assert_eq!(fake.lines(), ["provision: validated: release_untrusted"]);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("release_untrusted"));
    assert_eq!(state.phase, Phase::Validated);
    // The same for a hook and for a build.
    let fake = Fake::new();
    fake.edit_file(DESKTOP_PATH, |f| f.bytes.push(b'#'));
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(fake.lines(), ["provision: validated: release_untrusted"]);
    let fake = Fake::new();
    fake.build.lock().unwrap().commit = "other".to_owned();
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(fake.lines(), ["provision: validated: build_mismatch"]);
}

#[tokio::test]
async fn the_station_configuration_is_created_or_verified_never_replaced() {
    // Absent: created with the name of the file.
    let fake = Fake::new();
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("create_config:laptop"), 1);
    // The same name, or no name: kept.
    for config in [
        ConfigState::Named("laptop".to_owned()),
        ConfigState::Unnamed,
    ] {
        let fake = Fake::new();
        *fake.config.lock().unwrap() = config;
        assert_eq!(go(&fake, false).await, Exit::Complete);
        assert_eq!(fake.count("create_config:laptop"), 0);
    }
    // Another name, or a file that cannot be loaded: a conflict.
    for config in [ConfigState::Named("desk".to_owned()), ConfigState::Invalid] {
        let fake = Fake::new();
        *fake.config.lock().unwrap() = config;
        assert_eq!(go(&fake, false).await, Exit::Terminal);
        assert_eq!(fake.lines(), ["provision: validated: config_conflict"]);
        let state = fake.state().unwrap();
        assert_eq!(state.error.as_deref(), Some("config_conflict"));
        assert_eq!(state.phase, Phase::Validated);
        assert!(fake.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn the_service_is_installed_started_kept_or_refused() {
    let fake = Fake::new();
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(
        (fake.count("install_service"), fake.count("start_service")),
        (1, 0)
    );

    let fake = Fake::new();
    *fake.service.lock().unwrap() = ServiceState::Matching { active: false };
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(
        (fake.count("install_service"), fake.count("start_service")),
        (0, 1)
    );

    let fake = Fake::new();
    *fake.service.lock().unwrap() = ServiceState::Matching { active: true };
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(
        (fake.count("install_service"), fake.count("start_service")),
        (0, 0)
    );

    // A wrong service stops with a terminal; nothing else is touched.
    let fake = Fake::new();
    *fake.service.lock().unwrap() = ServiceState::Conflict;
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(fake.lines(), ["provision: service: service_conflict"]);
    assert_eq!(fake.count("fingerprint"), 0);
    assert_eq!(fake.count("join"), 0);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("service_conflict"));
    assert_eq!(state.phase, Phase::Service);
}

#[tokio::test]
async fn a_station_that_answers_after_70_seconds_is_waited_for() {
    let fake = Fake::new();
    fake.station.lock().unwrap().up_at = secs(70);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    let lines = fake.lines();
    let waits = lines
        .iter()
        .filter(|l| *l == "provision: service: station_unavailable")
        .count();
    assert_eq!(waits, 1);
    assert!(fake.sleeps.lock().unwrap().contains(&secs(30)));
    assert!(fake.elapsed() >= secs(90));
    // The service step ran again and found the service installed.
    assert_eq!(fake.count("install_service"), 1);
    // The 500 ms polls ran for 60 s.
    let polls = fake
        .sleeps
        .lock()
        .unwrap()
        .iter()
        .filter(|d| **d == Duration::from_millis(500))
        .count();
    assert_eq!(polls, 120);
    assert_eq!(fake.notices.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn every_call_error_counts_as_no_answer() {
    for error in [
        CallError::Unavailable,
        CallError::Timeout,
        CallError::Invalid,
    ] {
        let fake = Fake::new();
        {
            let mut st = fake.station.lock().unwrap();
            st.up_at = secs(70);
            st.down_error = error;
        }
        assert_eq!(go(&fake, false).await, Exit::Complete, "{error:?}");
        assert!(
            fake.lines()
                .contains(&"provision: service: station_unavailable".to_owned()),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn a_station_that_never_answers_keeps_the_loop_and_a_stopped_service_restarts() {
    let fake = Fake::new();
    *fake.service.lock().unwrap() = ServiceState::Matching { active: false };
    fake.station.lock().unwrap().up_at = secs(200);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    let lines = fake.lines();
    let waits = lines
        .iter()
        .filter(|l| *l == "provision: service: station_unavailable")
        .count();
    assert!(waits >= 2, "{waits}");
    assert_eq!(fake.count("start_service"), 1);
}

#[tokio::test]
async fn a_station_with_another_build_or_binary_is_a_build_mismatch() {
    for edit in [
        (|s: &mut super::fake::Station| s.build.commit = "other".to_owned())
            as fn(&mut super::fake::Station),
        |s| s.build.version = "2.0.1".to_owned(),
        |s| s.build.release = false,
        |s| s.binary_sha256 = sha256_hex(b"other"),
    ] {
        let fake = Fake::new();
        edit(&mut fake.station.lock().unwrap());
        assert_eq!(go(&fake, false).await, Exit::Terminal);
        assert_eq!(fake.lines(), ["provision: service: build_mismatch"]);
        assert_eq!(fake.count("join"), 0);
        assert!(fake.notices.lock().unwrap().is_empty());
        assert_eq!(
            fake.state().unwrap().error.as_deref(),
            Some("build_mismatch")
        );
    }
}

#[tokio::test]
async fn a_changed_file_before_pairing_restarts_at_validated() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::Code("pending"), JoinStep::Paired]);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(fake.state().unwrap().phase, Phase::Service);
    // The owner changes the file (another deck address) and retries.
    *fake.provision.lock().unwrap() = Ok(other_deck_file());
    assert_eq!(go(&fake, true).await, Exit::Complete);
    let args = fake.join_args.lock().unwrap().clone();
    assert_eq!(args[1].0, "other.example.ts.net");
    let state = fake.state().unwrap();
    assert_eq!(state.config_sha256, sha256_hex(&other_deck_file()));
    assert_eq!(state.phase, Phase::Complete);
}

#[tokio::test]
async fn a_changed_file_after_pairing_is_config_changed() {
    let fake = Fake::new();
    // The run pairs, then stops at a broken provider (phase `link`).
    fake.station.lock().unwrap().providers = Some(vec![super::fake::provider(
        "claude",
        None,
        "signed_in",
        fleetdeck_proto::link::VersionStatus::Unknown,
    )]);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let old_hash = sha256_hex(&file_text("laptop"));
    assert_eq!(fake.state().unwrap().phase, Phase::Link);
    *fake.provision.lock().unwrap() = Ok(other_deck_file());
    assert_eq!(go(&fake, true).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("config_changed"));
    assert_eq!(state.config_sha256, old_hash);
    assert_eq!(state.phase, Phase::Link);
    assert_eq!(
        fake.lines().last().unwrap(),
        "provision: link: config_changed"
    );
    assert_eq!(fake.count("join"), 1);
}

#[tokio::test]
async fn a_changed_file_after_complete_is_config_changed() {
    let fake = Fake::new();
    assert_eq!(go(&fake, false).await, Exit::Complete);
    *fake.provision.lock().unwrap() = Ok(other_deck_file());
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("config_changed"));
    assert_eq!(state.phase, Phase::Complete);
}

#[tokio::test]
async fn a_changed_station_key_is_station_identity_changed() {
    let fake = Fake::new();
    fake.station.lock().unwrap().providers = Some(vec![super::fake::provider(
        "claude",
        Some("1.0.0"),
        "signed_in",
        fleetdeck_proto::link::VersionStatus::Broken,
    )]);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(
        fake.state().unwrap().station_fingerprint.as_deref(),
        Some(STATION_FP)
    );
    fake.station.lock().unwrap().fingerprint = STATION_FP_2.to_owned();
    assert_eq!(go(&fake, true).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("station_identity_changed"));
    // The stored key stays.
    assert_eq!(state.station_fingerprint.as_deref(), Some(STATION_FP));
    assert_eq!(fake.count("join"), 1);
}
