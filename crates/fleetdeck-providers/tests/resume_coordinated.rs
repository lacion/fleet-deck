//! `resume_coordinated` (DL-165, resume): a resumed session gets the same
//! handoff as a start. The fake agents write their argv and the credential
//! of their environment to a launch log.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test harness: a panic is the failure report"
)]

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::{ChildEnv, Secret};
use fleetdeck_proto::command::SessionSpec;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::coordinator::{CoordinatorLaunch, TOKEN_ENV, mcp_args};
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_providers::{EventSink, NativeSessionId, ProviderAdapter, ProviderError};
use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::fake::{LAUNCH_LOG_ENV, SCENARIO_ENV};

const URL: &str = "http://127.0.0.1:41234/mcp";
const TOKEN: &str = "fdc1_resume_test_credential";
const WAIT: Duration = Duration::from_secs(20);

fn spec(instance: &str, cwd: &std::path::Path) -> SessionSpec {
    SessionSpec {
        provider_instance: instance.into(),
        cwd: cwd.to_string_lossy().into_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
    }
}

fn launch(provider: ProviderKind) -> CoordinatorLaunch {
    CoordinatorLaunch {
        args: mcp_args(provider, URL).unwrap(),
        token: Secret::new(TOKEN.to_owned()),
    }
}

/// The argv and the credential variable that the child wrote.
fn read_log(path: &std::path::Path) -> (String, String) {
    let log = std::fs::read_to_string(path).unwrap();
    let (argv, token) = log.split_once("env:").unwrap();
    (argv.to_owned(), token.trim().to_owned())
}

fn env_for(log: &std::path::Path) -> ChildEnv {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    env.set(LAUNCH_LOG_ENV, log).unwrap();
    env
}

#[tokio::test]
async fn a_claude_resume_with_a_launch_has_the_resume_and_the_mcp_args_and_the_token_only_in_env() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("launch.log");
    let config = ClaudeAdapterConfig::new(
        "claude".into(),
        FakeAgent::Claude.locate().unwrap(),
        env_for(&log),
    );
    let adapter = ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()));
    let (sink, _rx) = EventSink::channel(256);
    let session = tokio::time::timeout(
        WAIT,
        adapter.resume_coordinated(
            &NativeSessionId("native-1".into()),
            spec("claude", tmp.path()),
            sink,
            launch(ProviderKind::Claude),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    // The child writes its log right after it starts.
    for _ in 0..200 {
        if log.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (argv, token) = read_log(&log);
    let args: Vec<&str> = argv.lines().collect();
    let resume = args.iter().position(|a| *a == "--resume").unwrap();
    assert_eq!(args[resume + 1], "native-1");
    assert!(args.contains(&"--mcp-config"));
    assert!(args.contains(&"--allowedTools"));
    assert!(args.contains(&"mcp__fleetdeck"));
    assert!(!argv.contains("fdc1_"), "argv never holds the credential");
    assert!(argv.contains(&format!("${{{TOKEN_ENV}}}")));
    assert_eq!(token, TOKEN, "the credential is in the child env");
    drop(session);
}

#[tokio::test]
async fn a_codex_resume_with_a_launch_has_the_mcp_args_and_the_token_only_in_env() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("launch.log");
    let config = CodexAdapterConfig::new(
        "codex".into(),
        FakeAgent::Codex.locate().unwrap(),
        env_for(&log),
    );
    let adapter = CodexAdapter::new(config, Arc::new(DirectChildHost::new()));
    let (sink, _rx) = EventSink::channel(256);
    let session = tokio::time::timeout(
        WAIT,
        adapter.resume_coordinated(
            &NativeSessionId("t-old".into()),
            spec("codex", tmp.path()),
            sink,
            launch(ProviderKind::Codex),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let (argv, token) = read_log(&log);
    assert!(argv.contains("app-server"));
    assert!(argv.contains("mcp_servers.fleetdeck.url="));
    assert!(argv.contains("mcp_servers.fleetdeck.bearer_token_env_var"));
    assert!(!argv.contains("fdc1_"), "argv never holds the credential");
    assert_eq!(token, TOKEN, "the credential is in the child env");
    drop(session);
}

/// A provider without a handoff refuses: the trait default.
#[tokio::test]
async fn an_adapter_without_a_handoff_refuses_a_coordinated_resume() {
    struct Bare;
    #[async_trait::async_trait]
    impl ProviderAdapter for Bare {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Claude
        }
        fn capabilities(&self) -> fleetdeck_proto::Capabilities {
            fleetdeck_proto::Capabilities::minimal()
        }
        async fn probe(&self) -> Result<fleetdeck_providers::ProviderProbe, ProviderError> {
            unreachable!()
        }
        async fn start(
            &self,
            _: SessionSpec,
            _: EventSink,
        ) -> Result<Box<dyn fleetdeck_providers::LiveSession>, ProviderError> {
            unreachable!()
        }
        async fn resume(
            &self,
            _: &NativeSessionId,
            _: SessionSpec,
            _: EventSink,
        ) -> Result<Box<dyn fleetdeck_providers::LiveSession>, ProviderError> {
            unreachable!()
        }
    }
    let (sink, _rx) = EventSink::channel(4);
    let r = Bare
        .resume_coordinated(
            &NativeSessionId("n".into()),
            spec("claude", std::path::Path::new("/w")),
            sink,
            launch(ProviderKind::Claude),
        )
        .await;
    assert!(matches!(r, Err(ProviderError::CoordinatorHandoff(_))));
}
