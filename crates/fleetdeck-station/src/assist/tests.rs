//! The model help with a fake runner (R10, DL-167): no CLI starts.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::config::{Driver, ModelHelpConfig};
use fleetdeck_proto::event::{ProviderKind, RunOrigin};
use fleetdeck_proto::link::StationProvider;
use fleetdeck_testkit::Det;

use super::*;
use crate::store::{NewSession, OfflineApprovalPolicy, StoreConfig};
use runner::{Run, RunError};

/// Answers with a fixed text and counts its calls.
#[derive(Debug)]
struct Fake {
    text: Result<&'static str, &'static str>,
    tokens: (u64, u64),
    calls: AtomicUsize,
    last: std::sync::Mutex<Option<Call>>,
}

impl Fake {
    fn new(text: &'static str) -> Arc<Self> {
        Arc::new(Self {
            text: Ok(text),
            tokens: (100, 10),
            calls: AtomicUsize::new(0),
            last: std::sync::Mutex::new(None),
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            text: Err("boom"),
            tokens: (0, 0),
            calls: AtomicUsize::new(0),
            last: std::sync::Mutex::new(None),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Runner for Fake {
    async fn run(&self, call: Call) -> Result<Run, RunError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().unwrap() = Some(call);
        match self.text {
            Ok(text) => Ok(Run {
                text: text.to_owned(),
                input_tokens: self.tokens.0,
                output_tokens: self.tokens.1,
            }),
            Err(_) => Err(RunError::Exit("1".to_owned())),
        }
    }
}

fn instance(id: &str, driver: Driver) -> AssistInstance {
    AssistInstance {
        id: id.into(),
        driver,
        binary: id.into(),
        env: ChildEnv::empty(),
    }
}

fn provider(id: &str, auth: &str) -> StationProvider {
    StationProvider {
        instance: id.into(),
        provider: if id == "codex" {
            ProviderKind::Codex
        } else {
            ProviderKind::Claude
        },
        version: None,
        auth: auth.to_owned(),
        capabilities: None,
    }
}

fn signed_in() -> Vec<StationProvider> {
    vec![
        provider("claude", "signed_in"),
        provider("codex", "signed_in"),
    ]
}

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s
}

async fn assist_with(det: &Det, config: ModelHelpConfig, runner: Arc<Fake>) -> Assist {
    Assist::with_runner(
        config,
        store(det).await,
        vec![
            instance("claude", Driver::Claude),
            instance("codex", Driver::Codex),
        ],
        runner,
    )
}

#[tokio::test]
async fn a_title_is_cleaned_cached_and_counted() {
    let det = Det::new();
    let fake = Fake::new("\"Fix the login redirect.\"\n");
    let assist = assist_with(&det, ModelHelpConfig::default(), fake.clone()).await;
    let a = assist
        .generate(AssistKind::Title, "fix the login redirect", &signed_in())
        .await
        .unwrap();
    assert_eq!(
        (a.text.as_str(), a.generated, a.cached),
        ("Fix the login redirect", true, false)
    );
    // The same input again: from the cache, no call, no cost.
    let b = assist
        .generate(AssistKind::Title, "fix the login redirect", &signed_in())
        .await
        .unwrap();
    assert_eq!(
        (b.text.as_str(), b.cached),
        ("Fix the login redirect", true)
    );
    assert_eq!(fake.calls(), 1);
    assert_eq!(assist.usage().await, Some((110, 200_000)));
    // Another input is another call.
    assist
        .generate(AssistKind::Title, "another prompt", &signed_in())
        .await
        .unwrap();
    assert_eq!(fake.calls(), 2);
    assert_eq!(assist.usage().await, Some((220, 200_000)));
}

#[tokio::test]
async fn the_call_uses_the_model_and_the_instance_of_the_signed_in_provider() {
    let det = Det::new();
    let fake = Fake::new("A title");
    let assist = assist_with(&det, ModelHelpConfig::default(), fake.clone()).await;
    // Only the Codex instance is signed in.
    let providers = vec![
        provider("claude", "signed_out"),
        provider("codex", "signed_in"),
    ];
    assist
        .generate(AssistKind::Title, "a prompt", &providers)
        .await
        .unwrap();
    let call = fake.last.lock().unwrap().clone().unwrap();
    assert_eq!(call.driver, Driver::Codex);
    assert_eq!(call.model, "luna");
    assert!(
        call.prompt
            .contains("-----BEGIN INPUT-----\na prompt\n-----END INPUT-----")
    );
    // A configured provider wins over the sign-in order.
    let config = ModelHelpConfig {
        provider: Some(Driver::Claude),
        ..ModelHelpConfig::default()
    };
    let det = Det::new();
    let fake = Fake::new("A title");
    let assist = assist_with(&det, config, fake.clone()).await;
    assist
        .generate(AssistKind::Title, "a prompt", &providers)
        .await
        .unwrap();
    let call = fake.last.lock().unwrap().clone().unwrap();
    assert_eq!(
        (call.driver, call.model.as_str()),
        (Driver::Claude, "claude-haiku-5-5")
    );
    assert_eq!(
        call.env
            .get("CLAUDE_CODE_MAX_OUTPUT_TOKENS")
            .and_then(|v| v.to_str()),
        Some("400")
    );
}

#[tokio::test]
async fn the_switch_the_provider_and_the_input_are_checked() {
    let det = Det::new();
    let fake = Fake::new("A title");
    let off = ModelHelpConfig {
        enabled: false,
        ..ModelHelpConfig::default()
    };
    let assist = assist_with(&det, off, fake.clone()).await;
    let err = assist
        .generate(AssistKind::Title, "x", &signed_in())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "disabled");
    assert_eq!(assist.usage().await, None);

    let assist = assist_with(&det, ModelHelpConfig::default(), fake.clone()).await;
    let none = vec![
        provider("claude", "signed_out"),
        provider("codex", "unknown"),
    ];
    let err = assist
        .generate(AssistKind::Title, "x", &none)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "no_provider");
    for bad in ["", "   ", "a\0b"] {
        let err = assist
            .generate(AssistKind::Title, bad, &signed_in())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_params", "{bad:?}");
    }
    let long = "x".repeat(MAX_INPUT_CHARS + 1);
    let err = assist
        .generate(AssistKind::PlanGist, &long, &signed_in())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_params");
    assert_eq!(fake.calls(), 0, "no check reaches the model");
}

#[tokio::test]
async fn the_daily_cap_refuses_a_new_call_but_not_a_cached_one() {
    let det = Det::new();
    let fake = Fake::new("A title");
    let config = ModelHelpConfig {
        daily_token_cap: 110,
        ..ModelHelpConfig::default()
    };
    let assist = assist_with(&det, config, fake.clone()).await;
    assist
        .generate(AssistKind::Title, "first", &signed_in())
        .await
        .unwrap();
    assert_eq!(assist.usage().await, Some((110, 110)));
    let err = assist
        .generate(AssistKind::Title, "second", &signed_in())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "budget");
    let cached = assist
        .generate(AssistKind::Title, "first", &signed_in())
        .await
        .unwrap();
    assert!(cached.cached);
    assert_eq!(fake.calls(), 1);
    // The next UTC day starts at zero.
    det.advance(std::time::Duration::from_secs(24 * 60 * 60));
    assert_eq!(assist.usage().await, Some((0, 110)));
    assist
        .generate(AssistKind::Title, "second", &signed_in())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failed_call_stores_nothing_and_a_plan_mode_notice_is_no_text() {
    let det = Det::new();
    let fake = Fake::failing();
    let assist = assist_with(&det, ModelHelpConfig::default(), fake.clone()).await;
    let err = assist
        .generate(AssistKind::Title, "a prompt", &signed_in())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "failed");
    assert_eq!(assist.usage().await, Some((0, 200_000)));

    let notice = Fake::new("Plan mode is active. I will not change files.");
    let assist = assist_with(&det, ModelHelpConfig::default(), notice).await;
    let err = assist
        .generate(AssistKind::PlanGist, "a plan", &signed_in())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "failed");
    assert_eq!(assist.usage().await, Some((0, 200_000)));
}

#[test]
fn clean_shapes_the_text_of_each_kind() {
    let t = |raw: &str| clean(AssistKind::Title, raw, 2000);
    assert_eq!(t("Title: **Fix the bug**.").as_deref(), Some("Fix the bug"));
    assert_eq!(
        t("one two three four five six seven eight nine ten").as_deref(),
        Some("one two three four five six seven eight")
    );
    assert_eq!(
        t("\n\nFirst line\nsecond line").as_deref(),
        Some("First line")
    );
    assert_eq!(t("  \n "), None);
    assert_eq!(t("\"\""), None);
    let long = "w".repeat(200);
    assert_eq!(t(&long).map(|s| s.chars().count()), Some(80));
    let digest = clean(AssistKind::Digest, "- one  \n- two\n", 2000).unwrap();
    assert_eq!(digest, "- one\n- two");
    let cut = clean(AssistKind::Digest, &"y".repeat(100), 10).unwrap();
    assert_eq!(cut.chars().count(), 10);
}

#[test]
fn the_cache_key_depends_on_kind_model_and_prompt() {
    let a = input_hash(AssistKind::Title, "m", "p");
    assert_eq!(a.len(), 64);
    assert_eq!(a, input_hash(AssistKind::Title, "m", "p"));
    assert_ne!(a, input_hash(AssistKind::Digest, "m", "p"));
    assert_ne!(a, input_hash(AssistKind::Title, "n", "p"));
    assert_ne!(a, input_hash(AssistKind::Title, "m", "q"));
}

async fn session_with_run(store: &Store, id: &str) -> SessionId {
    store
        .create_session(NewSession {
            session_id: id.into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let session = SessionId::from(id);
    store
        .append(
            session.clone(),
            vec![
                Event::RunStarted {
                    run_id: "r1".into(),
                    input_summary: "fix the login redirect".to_owned(),
                    origin: RunOrigin::Board,
                }
                .into(),
                Event::MessageCompleted {
                    turn_id: "t1".into(),
                    parent_node_id: None,
                    text: "I fixed the redirect in router.rs".to_owned(),
                }
                .into(),
            ],
        )
        .await
        .unwrap();
    session
}

#[tokio::test]
async fn a_digest_is_built_from_the_session_stored_and_not_repeated() {
    let det = Det::new();
    let fake = Fake::new("- Read the router\n- Fixed the redirect\n- Ran the tests");
    let assist = assist_with(&det, ModelHelpConfig::default(), fake.clone()).await;
    let session = session_with_run(&assist.store, "s1").await;
    let a = assist
        .generate(AssistKind::Digest, session.as_str(), &signed_in())
        .await
        .unwrap();
    assert!(a.text.starts_with("- Read the router"));
    // The prompt holds the timeline lines of the session, not the id.
    let prompt = fake.last.lock().unwrap().clone().unwrap().prompt;
    assert!(
        prompt.contains("Prompt: fix the login redirect\nAgent: I fixed the redirect in router.rs")
    );
    let summary = assist
        .store
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.digest.as_deref(), Some(a.text.as_str()));
    // A second request gives the same text from the cache and no new event.
    let head = assist.store.head();
    let b = assist
        .generate(AssistKind::Digest, session.as_str(), &signed_in())
        .await
        .unwrap();
    assert!(b.cached);
    assert_eq!(assist.store.head(), head);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn a_digest_of_an_unknown_or_empty_session_is_refused_before_a_call() {
    let det = Det::new();
    let fake = Fake::new("- x");
    let assist = assist_with(&det, ModelHelpConfig::default(), fake.clone()).await;
    let err = assist
        .generate(AssistKind::Digest, "nope", &signed_in())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_params");
    assist
        .store
        .create_session(NewSession {
            session_id: "empty".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let err = assist
        .generate(AssistKind::Digest, "empty", &signed_in())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_params");
    assert_eq!(fake.calls(), 0);
}

#[test]
fn the_error_codes_are_the_wire_codes() {
    let codes: BTreeMap<&str, AssistError> = BTreeMap::from([
        ("disabled", AssistError::Disabled),
        ("budget", AssistError::Budget),
        ("no_provider", AssistError::NoProvider),
        ("invalid_params", AssistError::InvalidParams(String::new())),
        ("failed", AssistError::Failed(String::new())),
    ]);
    for (code, error) in codes {
        assert_eq!(error.code(), code);
    }
}
