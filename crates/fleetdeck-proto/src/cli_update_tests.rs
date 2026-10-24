use super::*;
use crate::event::ProviderKind;
use serde_json::json;

fn round_trip<T>(value: T, text: &str)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_value(&value).unwrap(), json!(text));
    assert_eq!(serde_json::from_value::<T>(json!(text)).unwrap(), value);
}

#[test]
fn install_method_round_trips_and_an_unknown_string_is_unknown() {
    round_trip(InstallMethod::Native, "native");
    round_trip(InstallMethod::Npm, "npm");
    round_trip(InstallMethod::Bun, "bun");
    round_trip(InstallMethod::Homebrew, "homebrew");
    round_trip(InstallMethod::SystemPackage, "system_package");
    assert_eq!(
        serde_json::from_value::<InstallMethod>(json!("snap")).unwrap(),
        InstallMethod::Unknown
    );
}

#[test]
fn update_trigger_round_trips_and_an_unknown_string_is_unknown() {
    round_trip(UpdateTrigger::Schedule, "schedule");
    round_trip(UpdateTrigger::Owner, "owner");
    round_trip(UpdateTrigger::Recovery, "recovery");
    round_trip(UpdateTrigger::External, "external");
    assert_eq!(
        serde_json::from_value::<UpdateTrigger>(json!("later")).unwrap(),
        UpdateTrigger::Unknown
    );
}

#[test]
fn update_outcome_round_trips_and_an_unknown_string_is_unknown() {
    let all = [
        (UpdateOutcome::Running, "running"),
        (UpdateOutcome::Updated, "updated"),
        (UpdateOutcome::UpToDate, "up_to_date"),
        (UpdateOutcome::SkippedBusy, "skipped_busy"),
        (UpdateOutcome::Refused, "refused"),
        (UpdateOutcome::Failed, "failed"),
        (UpdateOutcome::RolledBack, "rolled_back"),
        (UpdateOutcome::RollbackFailed, "rollback_failed"),
        (UpdateOutcome::ReportOnly, "report_only"),
        (UpdateOutcome::Interrupted, "interrupted"),
        (UpdateOutcome::Verified, "verified"),
    ];
    for (value, text) in all {
        round_trip(value, text);
    }
    assert_eq!(
        serde_json::from_value::<UpdateOutcome>(json!("exploded")).unwrap(),
        UpdateOutcome::Unknown
    );
}

#[test]
fn cli_update_run_refuses_an_unknown_field_and_defaults_its_flags() {
    let run: CliUpdateRun = serde_json::from_value(json!({"provider": "claude"})).unwrap();
    assert!(!run.check_only && !run.force);
    assert!(serde_json::from_value::<CliUpdateRun>(json!({"provider": "claude", "x": 1})).is_err());
}

#[test]
fn the_attempts_page_round_trips_with_its_incarnation() {
    let page = CliUpdateAttemptsPage {
        incarnation: IncarnationId::from("01J0000000000000000000000I"),
        rows: vec![CliUpdateAttempt {
            seq: 3,
            attempt_id: "a1".into(),
            provider: ProviderKind::Claude,
            trigger: UpdateTrigger::Schedule,
            method: InstallMethod::Native,
            channel: "stable".into(),
            from_version: Some("2.1.300".into()),
            target_version: Some("2.1.305".into()),
            to_version: Some("2.1.305".into()),
            outcome: UpdateOutcome::Updated,
            reason: None,
            started_at: 10,
            ended_at: Some(20),
        }],
    };
    let back: CliUpdateAttemptsPage =
        serde_json::from_value(serde_json::to_value(&page).unwrap()).unwrap();
    assert_eq!(back, page);
}

#[test]
fn is_release_accepts_three_numeric_parts_only() {
    for ok in ["2.1.310", "0.164.0", "999999.999999.999999"] {
        assert!(is_release(ok), "{ok}");
    }
    for bad in [
        "2.1.310-beta",
        "--help",
        "2.1",
        "1.2.3.4",
        "1234567.0.0",
        "",
        "1..2",
        "a.b.c",
        "+1.2.3",
    ] {
        assert!(!is_release(bad), "{bad}");
    }
}
