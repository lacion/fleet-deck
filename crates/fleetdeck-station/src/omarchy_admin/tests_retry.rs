//! Tests of `provision_retry`: when it is allowed, its argv, the `started`
//! answer and the values of `last_retry`.

use std::fs::{self, File};
use std::time::Duration;

use fleetdeck_proto::omarchy_admin::{ActOutcome, OmarchyAction};
use rustix::fs::{FlockOperation, flock};
use serde_json::Value;

use super::tests::{Rig, act, result, status, until, wire_error};

const NOT_PROVISIONED: &str = "Provisioning cannot be retried from the board in this state.";

/// The fake body of a bootstrap run that ends with `exit`, after it wrote
/// the phase `complete` when `complete` is set.
fn bootstrap(rig_state: &str, complete: bool, exit: i32) -> String {
    let write = if complete {
        format!("printf '{{\"phase\":\"complete\",\"error\":null}}' > '{rig_state}'\n")
    } else {
        String::new()
    };
    format!("case \"$*\" in *provision-bootstrap*)\n{write}exit {exit};;\nesac")
}

async fn ended(rig: &Rig) {
    until(|| rig.admin.running().is_none() && rig.admin.try_hold(super::Holder::Tick).is_some())
        .await;
}

#[tokio::test]
async fn omarchy_a_retry_is_refused_unless_the_state_allows_it() {
    let rig = Rig::new("exit 0");
    // No state file.
    let (code, text) = wire_error(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(
        (code.as_str(), text.as_str()),
        ("not_provisioned", NOT_PROVISIONED)
    );
    // Phases before `paired` and `complete`, with or without an error.
    for phase in ["validated", "service", "complete"] {
        for error in [None, Some("integration_failed")] {
            rig.write_state(phase, error);
            let (code, _) = wire_error(act(&rig.admin, "provision_retry", true).await);
            assert_eq!(code, "not_provisioned", "{phase} {error:?}");
        }
    }
    // An unreadable state file.
    fs::write(rig.path("station/provision-state.json"), "{ not json").unwrap();
    let (code, _) = wire_error(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(code, "not_provisioned");
    // A state without an error while a bootstrap holds the lock.
    rig.write_state("link", None);
    let lock = File::create(rig.path("station/provision.lock")).unwrap();
    flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
    let (code, _) = wire_error(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(code, "not_provisioned");
    assert!(rig.runs().is_empty(), "no child ran: {:?}", rig.runs());
    // With an error, the lock does not matter.
    rig.write_state("link", Some("pairing_failed"));
    let got = result(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(got.outcome, ActOutcome::Started);
    ended(&rig).await;
}

#[tokio::test]
async fn omarchy_a_retry_with_an_error_starts_at_once_and_passes_retry() {
    let rig = Rig::new("sleep 1");
    rig.write_state("integration", Some("integration_failed"));
    let got = result(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(got.outcome, ActOutcome::Started);
    assert_eq!((got.code, got.message), (None, None));
    assert_eq!(got.status.running, Some(OmarchyAction::ProvisionRetry));
    let view = got.status.provision.unwrap();
    assert_eq!(view.phase.as_deref(), Some("integration"));
    assert_eq!(view.error.as_deref(), Some("integration_failed"));
    assert_eq!(view.last_retry.as_deref(), Some("running"));
    until(|| !rig.action_runs().is_empty()).await;
    let runs = rig.action_runs();
    let data = rig.path("data").to_string_lossy().into_owned();
    let file = rig
        .path("config/fleetdeck/provision.toml")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        runs[0].args,
        [
            "--data-dir",
            &data,
            "provision-bootstrap",
            "--file",
            &file,
            "--retry"
        ]
    );
    // While the child runs, a second action is busy and status says so.
    let (code, _) = wire_error(act(&rig.admin, "install", true).await);
    assert_eq!(code, "busy");
    let during = status(rig.admin.status_call(Value::Null).await);
    assert_eq!(during.running, Some(OmarchyAction::ProvisionRetry));
    assert_eq!(
        during.provision.unwrap().last_retry.as_deref(),
        Some("running")
    );
    ended(&rig).await;
}

#[tokio::test]
async fn omarchy_a_retry_of_an_abandoned_checkpoint_has_no_retry_flag() {
    let rig = Rig::new("exit 0");
    rig.write_state("paired", None);
    let got = result(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(got.outcome, ActOutcome::Started);
    until(|| !rig.action_runs().is_empty()).await;
    let runs = rig.action_runs();
    assert!(
        !runs[0].args.contains(&"--retry".to_owned()),
        "{:?}",
        runs[0]
    );
    assert_eq!(
        runs[0].args.last().unwrap(),
        &rig.path("config/fleetdeck/provision.toml")
            .to_string_lossy()
            .into_owned()
    );
    ended(&rig).await;
}

#[tokio::test]
async fn omarchy_last_retry_follows_the_end_of_the_child() {
    let state_path = |rig: &Rig| {
        rig.path("station/provision-state.json")
            .to_string_lossy()
            .into_owned()
    };
    // (body, limit in ms, expected last_retry)
    let cases: [(&str, u64, &str); 5] = [
        ("complete-0", 10_000, "complete"),
        ("plain-0", 10_000, "failed"),
        ("plain-2", 10_000, "terminal"),
        ("plain-1", 10_000, "failed"),
        ("hang", 400, "timeout"),
    ];
    for (kind, limit, want) in cases {
        let rig = Rig::with("exit 0", |cfg| {
            cfg.limits.retry = Duration::from_millis(limit)
        });
        let state = state_path(&rig);
        let body = match kind {
            "complete-0" => bootstrap(&state, true, 0),
            "plain-0" => bootstrap(&state, false, 0),
            "plain-2" => bootstrap(&state, false, 2),
            "plain-1" => bootstrap(&state, false, 1),
            _ => "case \"$*\" in *provision-bootstrap*) sleep 30;; esac".to_owned(),
        };
        fs::write(rig.path("fake.sh"), format!("#!/bin/sh\n{body}\n")).unwrap();
        rig.write_state("link", Some("pairing_failed"));
        let got = result(act(&rig.admin, "provision_retry", true).await);
        assert_eq!(got.outcome, ActOutcome::Started, "{kind}");
        ended(&rig).await;
        let after = status(rig.admin.status_call(Value::Null).await);
        // The fake no longer logs: only the last_retry matters here.
        assert_eq!(
            after.provision.unwrap().last_retry.as_deref(),
            Some(want),
            "{kind}"
        );
        assert_eq!(after.running, None, "{kind}");
    }
}

#[tokio::test]
async fn omarchy_a_retry_without_a_config_home_fails_and_runs_nothing() {
    let rig = Rig::with("exit 0", |cfg| cfg.config_home = None);
    rig.write_state("integration", Some("integration_failed"));
    let got = result(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(got.outcome, ActOutcome::Failed);
    assert_eq!(got.code.as_deref(), Some("failed"));
    assert_eq!(got.message, None);
    assert_eq!(got.status.running, None);
    assert_eq!(rig.admin.running(), None);
    assert!(rig.action_runs().is_empty());
    // The retry rule comes first: a state that does not allow it is
    // `not_provisioned`, also without a config home.
    rig.write_state("service", None);
    let (code, _) = wire_error(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(code, "not_provisioned");
}

#[tokio::test]
async fn omarchy_a_retry_needs_no_omarchy() {
    let rig = Rig::with("exit 0", |cfg| {
        cfg.omarchy_env = fleetdeck_core::childenv::ChildEnv::empty();
    });
    rig.write_state("link", Some("pairing_failed"));
    let got = result(act(&rig.admin, "provision_retry", true).await);
    assert_eq!(got.outcome, ActOutcome::Started);
    assert!(!got.status.omarchy.present);
    ended(&rig).await;
}
