//! The run: the link, the providers, the integration and the `Complete`
//! fast path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io;
use std::time::Duration;

use fleetdeck_proto::link::VersionStatus;

use super::fake::{Fake, JoinStep, OTHER_FP, STATION_FP_2, go, provider, status_json};
use super::run::Exit;
use super::state::Phase;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn set_providers(fake: &Fake, list: Vec<fleetdeck_proto::link::StationProvider>) {
    fake.station.lock().unwrap().providers = Some(list);
}

async fn provider_terminal(list: Vec<fleetdeck_proto::link::StationProvider>) {
    let fake = Fake::new();
    set_providers(&fake, list);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("provider_probe_failed"));
    assert_eq!(state.phase, Phase::Link);
    assert_eq!(
        fake.lines().last().unwrap(),
        "provision: link: provider_probe_failed"
    );
    assert_eq!(fake.count("omarchy_install"), 0);
}

#[tokio::test]
async fn a_link_that_stays_down_is_polled_again_without_a_new_join() {
    let fake = Fake::new();
    {
        let mut st = fake.station.lock().unwrap();
        // The first answer is step 8; then the link is down for 70 answers.
        st.link_script.push_back(true);
        st.link_script.extend(std::iter::repeat_n(false, 70));
    }
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("join"), 1);
    let sleeps = fake.sleeps.lock().unwrap().clone();
    assert!(sleeps.contains(&secs(30)));
    assert!(fake.elapsed() >= secs(90));
    assert!(sleeps.iter().filter(|d| **d == secs(1)).count() >= 60);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn a_link_that_flaps_is_not_saved_until_it_is_up() {
    let fake = Fake::new();
    {
        let mut st = fake.station.lock().unwrap();
        st.link_script.extend([true, false, false, true]);
    }
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(*fake.sleeps.lock().unwrap(), [secs(1), secs(1)]);
    // One answer for step 8, three for the link and one before `Complete`.
    assert_eq!(fake.count("fingerprint"), 5);
}

#[tokio::test]
async fn a_pairing_that_vanishes_during_the_link_goes_back_to_the_join() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::Paired, JoinStep::Paired]);
    {
        let mut st = fake.station.lock().unwrap();
        // Step 8 sees nothing paired; the join pairs; the first link answer
        // shows no pairing again.
        st.edits.push_back(Box::new(|_| {}));
        st.edits.push_back(Box::new(|s| s.paired = None));
    }
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("join"), 2);
}

#[tokio::test]
async fn another_deck_during_the_link_is_paired_elsewhere() {
    let fake = Fake::new();
    {
        let mut st = fake.station.lock().unwrap();
        st.edits.push_back(Box::new(|_| {}));
        st.edits
            .push_back(Box::new(|s| s.paired = Some(OTHER_FP.to_owned())));
    }
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("paired_elsewhere"));
    assert_eq!(state.phase, Phase::Paired);
    assert_eq!(
        fake.lines().last().unwrap(),
        "provision: paired: paired_elsewhere"
    );
}

#[tokio::test]
async fn a_station_key_that_changes_during_the_link_is_terminal() {
    let fake = Fake::new();
    {
        let mut st = fake.station.lock().unwrap();
        st.edits.push_back(Box::new(|_| {}));
        st.edits
            .push_back(Box::new(|s| s.fingerprint = STATION_FP_2.to_owned()));
    }
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(
        fake.state().unwrap().error.as_deref(),
        Some("station_identity_changed")
    );
}

#[tokio::test]
async fn a_broken_provider_is_terminal() {
    provider_terminal(vec![provider(
        "claude",
        Some("9.9.9"),
        "signed_in",
        VersionStatus::Broken,
    )])
    .await;
}

#[tokio::test]
async fn an_unknown_status_or_a_missing_version_is_terminal() {
    provider_terminal(vec![provider(
        "claude",
        Some("1.0.0"),
        "signed_in",
        VersionStatus::Unknown,
    )])
    .await;
    provider_terminal(vec![provider(
        "claude",
        None,
        "signed_in",
        VersionStatus::Ok,
    )])
    .await;
    // One bad provider among good ones.
    provider_terminal(vec![
        provider("a", Some("1.0.0"), "signed_in", VersionStatus::Ok),
        provider("b", None, "signed_in", VersionStatus::Ok),
    ])
    .await;
    // No provider list although it was asked for.
    let fake = Fake::new();
    fake.station.lock().unwrap().providers = None;
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(
        fake.state().unwrap().error.as_deref(),
        Some("provider_probe_failed")
    );
}

#[tokio::test]
async fn a_signed_out_provider_is_a_warning() {
    let fake = Fake::new();
    set_providers(
        &fake,
        vec![
            provider(
                "claude-work",
                Some("1.0.0"),
                "signed_out",
                VersionStatus::Ok,
            ),
            provider(
                "codex",
                Some("0.5.0"),
                "signed_in",
                VersionStatus::OlderThanTested,
            ),
        ],
    );
    assert_eq!(go(&fake, false).await, Exit::Complete);
    let lines = fake.lines();
    assert!(lines.contains(&"provision: providers: signed_out claude-work".to_owned()));
    assert!(!lines.iter().any(|l| l.contains("signed_out codex")));
    assert!(!lines.contains(&"No providers are configured.".to_owned()));
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn zero_providers_succeed_with_a_fixed_line() {
    let fake = Fake::new();
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert!(
        fake.lines()
            .contains(&"No providers are configured.".to_owned())
    );
}

#[tokio::test]
async fn a_probe_that_fails_to_answer_is_repeated_after_30_seconds() {
    let fake = Fake::new();
    fake.station.lock().unwrap().probe_errors = 2;
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(*fake.sleeps.lock().unwrap(), [secs(30), secs(30)]);
    assert_eq!(fake.count("fingerprint:probe"), 3);
}

#[tokio::test]
async fn an_integration_failure_after_pairing_is_terminal_and_keeps_the_pairing() {
    let changed = || status_json(1, true, &["ok", "changed"], true, "ok");
    let cases = [
        (1, status_json(0, true, &["ok"], true, "ok")),
        (0, changed()),
        (0, status_json(1, true, &["missing"], true, "ok")),
        // The exit code is not 0 although the JSON looks clean.
        (0, status_json(1, true, &["ok"], true, "ok")),
        (0, status_json(0, false, &[], false, "ok")),
        (0, (0, b"not json".to_vec())),
        (0, (0, Vec::new())),
        (2, status_json(0, true, &["ok"], true, "ok")),
    ];
    for (install, status) in cases {
        let fake = Fake::new();
        fake.install.lock().unwrap().push_back(Ok(install));
        fake.status.lock().unwrap().push_back(Ok(status));
        assert_eq!(go(&fake, false).await, Exit::Terminal);
        let state = fake.state().unwrap();
        assert_eq!(state.error.as_deref(), Some("integration_failed"));
        assert_eq!(state.phase, Phase::Integration);
        assert_eq!(fake.count("omarchy_install"), 1);
        assert_eq!(
            fake.lines().last().unwrap(),
            "provision: integration: integration_failed"
        );
        // A failed install does not run the status.
        assert_eq!(fake.count("omarchy_status"), usize::from(install == 0));
        // The rerun keeps the pairing: no new join.
        assert_eq!(go(&fake, true).await, Exit::Complete);
        assert_eq!(fake.count("join"), 1);
    }
}

#[tokio::test]
async fn a_failure_to_run_the_installer_is_exit_1_and_saves_no_error() {
    let fake = Fake::new();
    fake.install
        .lock()
        .unwrap()
        .push_back(Err(io::Error::other("no process")));
    assert_eq!(go(&fake, false).await, Exit::Unexpected);
    assert_eq!(
        fake.lines().last().unwrap(),
        "provision: integration: unexpected_error"
    );
    let state = fake.state().unwrap();
    assert_eq!(state.error, None);
    assert_eq!(state.phase, Phase::Integration);
}

#[tokio::test]
async fn a_failure_to_run_the_status_is_exit_1() {
    let fake = Fake::new();
    fake.status
        .lock()
        .unwrap()
        .push_back(Err(io::Error::other("no process")));
    assert_eq!(go(&fake, false).await, Exit::Unexpected);
    assert_eq!(fake.state().unwrap().error, None);
}

async fn complete(fake: &Fake) {
    assert_eq!(go(fake, false).await, Exit::Complete);
    fake.calls.lock().unwrap().clear();
    fake.stderr.lock().unwrap().clear();
}

#[tokio::test]
async fn a_complete_state_checks_cheaply_and_has_no_other_effect() {
    let fake = Fake::new();
    complete(&fake).await;
    let before = std::fs::read(fake.state_file()).unwrap();
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(
        *fake.calls.lock().unwrap(),
        ["fingerprint", "omarchy_status"]
    );
    assert!(fake.lines().is_empty());
    assert_eq!(fake.notices.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read(fake.state_file()).unwrap(), before);
}

#[tokio::test]
async fn a_status_drift_runs_the_integration_again() {
    let fake = Fake::new();
    complete(&fake).await;
    fake.status
        .lock()
        .unwrap()
        .push_back(Ok(status_json(1, true, &["ok", "changed"], true, "ok")));
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(
        *fake.calls.lock().unwrap(),
        [
            "fingerprint",
            "omarchy_status",
            "omarchy_install",
            "omarchy_status",
            // The read before `Complete`.
            "fingerprint"
        ]
    );
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn a_status_drift_that_the_installer_cannot_fix_is_terminal_and_keeps_the_phase() {
    let fake = Fake::new();
    complete(&fake).await;
    fake.status.lock().unwrap().extend([
        Ok(status_json(1, true, &["changed"], true, "ok")),
        Ok(status_json(1, true, &["changed"], true, "ok")),
    ]);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("integration_failed"));
    assert_eq!(state.phase, Phase::Complete);
    assert_eq!(
        fake.lines().last().unwrap(),
        "provision: complete: integration_failed"
    );
}

#[tokio::test]
async fn a_complete_state_with_another_station_key_or_deck_is_terminal() {
    let fake = Fake::new();
    complete(&fake).await;
    fake.station.lock().unwrap().fingerprint = STATION_FP_2.to_owned();
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(
        fake.state().unwrap().error.as_deref(),
        Some("station_identity_changed")
    );
    assert_eq!(fake.count("omarchy_status"), 0);

    let fake = Fake::new();
    complete(&fake).await;
    fake.set_paired(Some(OTHER_FP));
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(
        fake.state().unwrap().error.as_deref(),
        Some("paired_elsewhere")
    );
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
    assert_eq!(fake.count("omarchy_status"), 0);
}

#[tokio::test]
async fn a_complete_state_without_a_pairing_runs_the_sequence_again() {
    let fake = Fake::new();
    complete(&fake).await;
    fake.set_paired(None);
    fake.set_join(&[JoinStep::Paired]);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("join"), 1);
    assert_eq!(fake.count("omarchy_install"), 1);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
    assert!(fake.state().unwrap().station_fingerprint.is_some());
}

#[tokio::test]
async fn a_complete_state_with_a_silent_station_runs_the_sequence_again() {
    let fake = Fake::new();
    complete(&fake).await;
    fake.station.lock().unwrap().up_at = fake.elapsed() + secs(100);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert!(
        fake.lines()
            .contains(&"provision: service: station_unavailable".to_owned())
    );
    assert_eq!(fake.count("omarchy_install"), 1);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn a_complete_state_still_checks_the_release() {
    let fake = Fake::new();
    complete(&fake).await;
    fake.edit_file(super::release::HOOK_PATH, |f| f.bytes.push(b'#'));
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("release_untrusted"));
    assert_eq!(state.phase, Phase::Complete);
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_plugin_that_is_not_enabled_repeats_the_integration_every_30_seconds() {
    let fake = Fake::new();
    fake.status.lock().unwrap().extend([
        Ok(status_json(0, true, &["ok"], false, "ok")),
        Ok(status_json(0, true, &["ok"], false, "ok")),
        Ok(status_json(0, true, &["ok"], true, "ok")),
    ]);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("omarchy_install"), 3);
    assert_eq!(fake.count("omarchy_status"), 3);
    assert_eq!(*fake.sleeps.lock().unwrap(), [secs(30), secs(30)]);
    let waits = fake
        .lines()
        .iter()
        .filter(|l| *l == "provision: integration: plugin_not_enabled")
        .count();
    assert_eq!(waits, 2);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn a_missing_hyprland_line_completes_with_one_stderr_line() {
    let fake = Fake::new();
    fake.status
        .lock()
        .unwrap()
        .push_back(Ok(status_json(0, true, &["ok"], true, "missing")));
    assert_eq!(go(&fake, false).await, Exit::Complete);
    let lines = fake.lines();
    let missing = lines
        .iter()
        .filter(|l| *l == "provision: integration: hyprland_line_missing")
        .count();
    assert_eq!(missing, 1);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
    assert_eq!(fake.state().unwrap().error, None);
}

#[tokio::test]
async fn a_station_key_that_changes_before_the_provider_answer_is_terminal() {
    let fake = Fake::new();
    fake.station
        .lock()
        .unwrap()
        .probe_edits
        .push_back(Box::new(|s| s.fingerprint = STATION_FP_2.to_owned()));
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("station_identity_changed"));
    assert_eq!(state.phase, Phase::Link);
    assert_eq!(fake.count("omarchy_install"), 0);
}

#[tokio::test]
async fn a_build_that_changes_in_the_provider_answer_is_terminal() {
    for edit in [
        (|s: &mut super::fake::Station| s.build.commit = "other".to_owned())
            as fn(&mut super::fake::Station),
        |s| s.binary_sha256 = super::sha256_hex(b"other"),
    ] {
        let fake = Fake::new();
        fake.station
            .lock()
            .unwrap()
            .probe_edits
            .push_back(Box::new(edit));
        assert_eq!(go(&fake, false).await, Exit::Terminal);
        let state = fake.state().unwrap();
        assert_eq!(state.error.as_deref(), Some("build_mismatch"));
        assert_eq!(fake.count("omarchy_install"), 0);
    }
}

#[tokio::test]
async fn another_deck_in_the_provider_answer_is_paired_elsewhere() {
    let fake = Fake::new();
    fake.station
        .lock()
        .unwrap()
        .probe_edits
        .push_back(Box::new(|s| s.paired = Some(OTHER_FP.to_owned())));
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(
        fake.state().unwrap().error.as_deref(),
        Some("paired_elsewhere")
    );
}

#[tokio::test]
async fn a_provider_answer_without_a_pairing_goes_back_to_the_join() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::Paired, JoinStep::Paired]);
    fake.station
        .lock()
        .unwrap()
        .probe_edits
        .push_back(Box::new(|s| s.paired = None));
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("join"), 2);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn a_link_that_drops_before_complete_goes_back_to_the_link_step() {
    let fake = Fake::new();
    {
        let mut st = fake.station.lock().unwrap();
        // Step 8 and the link step see the link up; the read before
        // `Complete` sees it down; then it is up again.
        st.link_script.extend([true, true, false, true, true]);
    }
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("join"), 1);
    // The sequence goes on from the link step: providers and integration
    // run again, then the read before `Complete`.
    assert_eq!(fake.count("fingerprint:probe"), 2);
    assert_eq!(fake.count("omarchy_install"), 2);
    assert_eq!(fake.count("fingerprint"), 5);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn an_unpaired_answer_before_complete_goes_back_to_the_join() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::Paired, JoinStep::Paired]);
    {
        let mut st = fake.station.lock().unwrap();
        st.edits.push_back(Box::new(|_| {}));
        st.edits.push_back(Box::new(|_| {}));
        st.edits.push_back(Box::new(|s| s.paired = None));
    }
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("join"), 2);
}

#[tokio::test]
async fn an_identity_change_before_complete_is_terminal_and_nothing_is_complete() {
    let fake = Fake::new();
    {
        let mut st = fake.station.lock().unwrap();
        st.edits.push_back(Box::new(|_| {}));
        st.edits.push_back(Box::new(|_| {}));
        st.edits
            .push_back(Box::new(|s| s.fingerprint = STATION_FP_2.to_owned()));
    }
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("station_identity_changed"));
    assert_eq!(state.phase, Phase::Integration);
}

#[tokio::test]
async fn a_failed_read_before_complete_is_repeated_after_30_seconds() {
    let fake = Fake::new();
    {
        let mut st = fake.station.lock().unwrap();
        st.edits.push_back(Box::new(|_| {}));
        // The station stops answering for 100 s after the link read.
        st.edits
            .push_back(Box::new(|s| s.up_at = std::time::Duration::from_secs(100)));
    }
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(*fake.sleeps.lock().unwrap(), [secs(30); 4]);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn the_manifest_binary_is_trusted_only_after_the_release_check() {
    let fake = Fake::new();
    assert!(fake.trusted.lock().unwrap().is_none());
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(
        fake.trusted.lock().unwrap().as_deref(),
        Some(super::fake::binary_sha().as_str())
    );
    // An untrusted release sets nothing.
    let fake = Fake::new();
    fake.edit_file(super::release::HOOK_PATH, |f| f.bytes.push(b'#'));
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert!(fake.trusted.lock().unwrap().is_none());
}

#[tokio::test]
async fn a_join_code_build_mismatch_is_the_terminal_build_mismatch() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::Code("build_mismatch")]);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("build_mismatch"));
    assert_eq!(fake.count("join"), 1);
}
