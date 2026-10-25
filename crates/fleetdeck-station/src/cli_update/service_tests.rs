//! The link methods, the live config, the instance targets and the real
//! parts that need no network. The attempt itself is in `run_tests.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::super::run::tests::{Rig, rig};
use super::*;
use crate::manager::policy_hub::test_config;

/// The service over the engine of a rig (no manager, no schedule task).
fn service(r: &Rig) -> Arc<CliUpdates> {
    Arc::new(CliUpdates {
        engine: Arc::clone(&r.engine),
        schedule: Arc::new(Schedule::new(Arc::clone(&r.engine), "Lighting".to_owned())),
    })
}

fn code(e: &(&'static str, String)) -> &'static str {
    e.0
}

#[tokio::test]
async fn without_a_service_every_method_is_unsupported() {
    for method in ["cli_update.status", "cli_update.run", "cli_update.attempts"] {
        let err = call(None, method, json!({})).await.unwrap_err();
        assert_eq!(code(&err), "unsupported", "{method}");
    }
}

#[tokio::test]
async fn status_answers_the_report_with_the_quarantine_reason() {
    let r = rig(ProviderKind::Claude).await;
    let svc = service(&r);
    let v = call(Some(Arc::clone(&svc)), "cli_update.status", json!({}))
        .await
        .unwrap();
    assert_eq!(v["providers"].as_array().unwrap().len(), 1);
    assert_eq!(v["providers"][0]["quarantined"], json!(null));
    assert_eq!(v["enabled"], json!(true));
    assert_eq!(v["last_seq"], json!(0));
    r.quarantine_now().await;
    let v = call(Some(svc), "cli_update.status", json!({}))
        .await
        .unwrap();
    assert_eq!(v["providers"][0]["quarantined"], json!("rollback_failed"));
}

#[tokio::test]
async fn run_with_bad_params_is_invalid_params() {
    let r = rig(ProviderKind::Claude).await;
    let svc = service(&r);
    for params in [
        json!({}),
        json!({"provider": "claude", "extra": 1}),
        json!(7),
    ] {
        let err = call(Some(Arc::clone(&svc)), "cli_update.run", params)
            .await
            .unwrap_err();
        assert_eq!(code(&err), "invalid_params");
    }
}

#[tokio::test]
async fn run_check_only_answers_the_state_and_writes_no_row() {
    let r = rig(ProviderKind::Claude).await;
    let svc = service(&r);
    let v = call(
        Some(svc),
        "cli_update.run",
        json!({"provider": "claude", "check_only": true}),
    )
    .await
    .unwrap();
    assert_eq!(v["state"]["available"], json!("2.1.310"));
    assert_eq!(v["attempt_id"], json!(null));
    assert!(r.last().await.is_none());
}

#[tokio::test]
async fn run_for_a_provider_without_an_instance_is_refused_not_found() {
    let r = rig(ProviderKind::Claude).await;
    let svc = service(&r);
    let v = call(Some(svc), "cli_update.run", json!({"provider": "codex"}))
        .await
        .unwrap();
    assert_eq!(v["refused"], json!("not_found"));
}

#[tokio::test]
async fn attempts_clamps_the_limit_and_carries_the_incarnation() {
    let r = rig(ProviderKind::Claude).await;
    for id in ["x1", "x2", "x3"] {
        let a = new_attempt(
            id,
            ProviderKind::Claude,
            UpdateTrigger::Owner,
            InstallMethod::Native,
            "stable",
            None,
            None,
        );
        r.store
            .cli_update_insert(a, Some(end_of(UpdateOutcome::UpToDate, None, None)))
            .await
            .unwrap();
    }
    let svc = service(&r);
    let page = |after: u64, limit: u32| {
        let svc = Arc::clone(&svc);
        async move {
            call(
                Some(svc),
                "cli_update.attempts",
                json!({"after_seq": after, "limit": limit}),
            )
            .await
            .unwrap()
        }
    };
    let zero = page(0, 0).await;
    assert_eq!(zero["rows"].as_array().unwrap().len(), 1);
    let big = page(0, 1000).await;
    assert_eq!(big["rows"].as_array().unwrap().len(), 3);
    let seq = big["rows"][0]["seq"].as_u64().unwrap();
    assert_eq!(page(seq, 100).await["rows"].as_array().unwrap().len(), 2);
    let inc = serde_json::to_value(r.store.incarnation()).unwrap();
    assert_eq!(big["incarnation"], inc);
}

#[tokio::test]
async fn an_unknown_method_is_not_implemented() {
    let r = rig(ProviderKind::Claude).await;
    let err = call(Some(service(&r)), "cli_update.nope", json!({}))
        .await
        .unwrap_err();
    assert_eq!(code(&err), "not_implemented");
}

#[test]
fn the_live_config_follows_the_file_and_falls_back_to_the_start_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station.toml");
    let start = ProviderUpdatesConfig {
        min_age_hours: 48,
        ..ProviderUpdatesConfig::default()
    };
    let source = FileConfig::new(path.clone(), dir.path().join("secrets"), start);
    assert_eq!(source.current().min_age_hours, 48, "no file");
    let roots = format!("allowed_roots = [\"{}\"]\n", dir.path().display());
    std::fs::write(
        &path,
        format!("{roots}[provider_updates]\nmin_age_hours = 7\n"),
    )
    .unwrap();
    assert_eq!(source.current().min_age_hours, 7);
    std::fs::write(&path, "this is not toml = = =").unwrap();
    assert_eq!(source.current().min_age_hours, 48, "a bad file");
}

#[test]
fn the_targets_follow_the_configured_instances() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path(), "");
    let targets = targets_from_config(&config);
    assert!(targets.iter().any(|t| t.kind == ProviderKind::Claude));
    assert!(targets.iter().any(|t| t.kind == ProviderKind::Codex));
}

#[test]
fn a_shim_host_needs_both_the_setting_and_the_effective_host() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path(), "host = \"shim\"\n");
    assert!(shim_host(&config, HostChoice::Shim));
    assert!(!shim_host(&config, HostChoice::Direct));
    let direct = test_config(dir.path(), "");
    assert!(!shim_host(&direct, HostChoice::Direct));
}

#[tokio::test]
async fn a_manager_that_is_gone_refuses_the_hold_and_lists_no_sessions() {
    let host = ManagerHost {
        manager: Weak::new(),
    };
    assert!(host.live_sessions(ProviderKind::Claude).await.is_empty());
    assert_eq!(
        host.begin_hold(ProviderKind::Claude).await.err(),
        Some(reason::STATION_RESTARTING)
    );
    assert_eq!(host.installed(ProviderKind::Claude).await, None);
}

#[tokio::test]
async fn the_real_channel_refuses_native_claude_without_gpgv_and_report_only_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let channel = RealChannel::new(dir.path());
    let mut install = crate::cli_update::detect::Install {
        kind: ProviderKind::Claude,
        method: InstallMethod::Native,
        launcher: "/x/claude".into(),
        chain: vec!["/x/claude".into()],
        real: "/x/claude".into(),
        tool: Tool::Native {
            link: "/x/claude".into(),
            versions: "/x/versions".into(),
            gpgv: None,
        },
        blocked: None,
    };
    assert_eq!(
        channel.resolve(&install, None, "stable").await.err(),
        Some(reason::GPGV_MISSING)
    );
    install.kind = ProviderKind::Unknown;
    install.tool = Tool::None;
    assert_eq!(
        channel.resolve(&install, None, "stable").await.err(),
        Some(reason::REPORT_ONLY_METHOD)
    );
}
