//! The model help wired to the manager (R10.1, R10.2, DL-167): a session
//! without a title gets one after its first turn, an ended session gets a
//! digest, and a titled session is left alone. The model call is a fake.

use std::sync::atomic::{AtomicUsize, Ordering};

use fleetdeck_core::config::{Driver, ModelHelpConfig};

use super::*;
use crate::assist::runner::{Call, Run, RunError, Runner};
use crate::assist::{Assist, AssistInstance, watch};

#[derive(Debug, Default)]
struct Fake {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Runner for Fake {
    async fn run(&self, call: Call) -> Result<Run, RunError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = if call.prompt.contains("coding session") {
            "\"Greet the user.\""
        } else {
            "- Said hello\n- Ended the turn"
        };
        Ok(Run {
            text: text.to_owned(),
            input_tokens: 50,
            output_tokens: 5,
        })
    }
}

fn assist(st: &Station) -> (Arc<Assist>, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let config = ModelHelpConfig {
        // The fake CLI's sign-in state is not under test.
        provider: Some(Driver::Claude),
        ..ModelHelpConfig::default()
    };
    let instance = AssistInstance {
        id: "claude".into(),
        driver: Driver::Claude,
        binary: "claude".into(),
        env: ChildEnv::empty(),
    };
    let assist = Assist::with_runner(
        config,
        st.manager.store().clone(),
        vec![instance],
        fake.clone(),
    );
    (Arc::new(assist), fake)
}

async fn summary(st: &Station, session: &SessionId) -> fleetdeck_proto::SessionSummary {
    st.manager
        .store()
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap()
}

async fn wait_turn_done(st: &Station) {
    st.wait_for("turn done", |e| {
        matches!(
            e,
            Event::TurnCompleted {
                status: TurnStatus::Done,
                ..
            }
        )
        .then_some(())
    })
    .await;
}

#[tokio::test]
async fn a_session_without_a_title_gets_a_generated_one_after_its_first_turn() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    wait_turn_done(&st).await;
    let (assist, fake) = assist(&st);

    watch::title_session(&assist, &st.manager, s.clone()).await;

    let summary = summary(&st, &s).await;
    assert_eq!(summary.title.as_deref(), Some("Greet the user"));
    assert!(summary.title_generated);
    let names = kinds(&st.events().await);
    let renamed = names.iter().position(|k| k == "session.renamed").unwrap();
    assert_eq!(names[renamed + 1], "session.title_generated");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    // The title is set: a second pass makes no call and no event.
    let before = names.len();
    watch::title_session(&assist, &st.manager, s.clone()).await;
    assert_eq!(st.events().await.len(), before);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_session_that_has_a_title_is_not_renamed_and_costs_nothing() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let mut named = spec(&ws, "claude");
    named.title = Some("mine".to_owned());
    let s = st.start_with(named, Some("hello")).await;
    wait_turn_done(&st).await;
    let (assist, fake) = assist(&st);

    watch::title_session(&assist, &st.manager, s.clone()).await;

    let summary = summary(&st, &s).await;
    assert_eq!(summary.title.as_deref(), Some("mine"));
    assert!(!summary.title_generated);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_owner_rename_removes_the_generated_mark() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    wait_turn_done(&st).await;
    let (assist, _) = assist(&st);
    watch::title_session(&assist, &st.manager, s.clone()).await;
    assert!(summary(&st, &s).await.title_generated);
    st.accepted(SessionCommand::Rename {
        session_id: s.clone(),
        title: Some("My own title".to_owned()),
    })
    .await;
    let summary = summary(&st, &s).await;
    assert_eq!(summary.title.as_deref(), Some("My own title"));
    assert!(!summary.title_generated);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_ended_session_gets_a_digest() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    wait_turn_done(&st).await;
    let (assist, fake) = assist(&st);

    watch::digest_session(&assist, &st.manager, s.clone()).await;

    let summary = summary(&st, &s).await;
    assert_eq!(
        summary.digest.as_deref(),
        Some("- Said hello\n- Ended the turn")
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_watcher_titles_a_session_once_and_digests_it_when_it_ends() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    // The session starts without a prompt, so no turn is stored yet.
    let s = st.start(&ws, None).await;
    let (assist, fake) = assist(&st);
    let Station { manager, det, .. } = st;
    let manager = Arc::new(manager);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn(watch::run(assist, Arc::clone(&manager), stop_rx));
    let send = |n: u32, command| {
        let envelope = CommandEnvelope {
            command_id: CommandId::from(format!("watch-{n}")),
            target_incarnation: None,
            retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command,
        };
        let manager = Arc::clone(&manager);
        async move { manager.submit(envelope, Caller::local()).await.unwrap() }
    };
    // The first prompt is rejected until the provider process runs.
    let deadline = tokio::time::Instant::now() + WAIT;
    for n in 10.. {
        let decided = send(
            n,
            SessionCommand::Prompt {
                session_id: s.clone(),
                input: text("hello"),
            },
        )
        .await;
        if decided.receipt.state == ReceiptState::Accepted {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the prompt was never accepted: {:?}",
            decided.receipt
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    loop {
        let summary = manager.store().summary(s.clone()).await.unwrap().unwrap();
        if summary.title_generated {
            assert_eq!(summary.title.as_deref(), Some("Greet the user"));
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no generated title");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // A second turn does not ask for a second title.
    send(
        2,
        SessionCommand::Prompt {
            session_id: s.clone(),
            input: text("again"),
        },
    )
    .await;
    send(
        3,
        SessionCommand::Stop {
            session_id: s.clone(),
            how: StopMode::Graceful,
        },
    )
    .await;
    loop {
        let summary = manager.store().summary(s.clone()).await.unwrap().unwrap();
        if summary.digest.is_some() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no digest");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // One title call and one digest call; the watcher ignores its own events.
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    stop_tx.send(true).unwrap();
    watcher.await.unwrap();
    manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_publish_that_turns_model_help_off_refuses_the_next_call() {
    use fleetdeck_proto::assist::AssistKind;

    let st = Station::new(2).await;
    let root = st.root.path().join("projects");
    let fake = Arc::new(Fake::default());
    let instance = AssistInstance {
        id: "claude".into(),
        driver: Driver::Claude,
        binary: "claude".into(),
        env: ChildEnv::empty(),
    };
    let assist = Assist::with_hub_runner(
        Arc::clone(st.manager.policy_hub()),
        st.manager.store().clone(),
        vec![instance],
        fake.clone(),
    );
    let apply = |extra: &str| {
        let config = super::policy_hub::test_config(&root, extra);
        st.manager.publish(st.manager.prepare(&config).unwrap());
    };

    apply("[model_help]\nprovider = \"claude\"\n");
    let answer = assist
        .generate(AssistKind::Title, "fix the login redirect", &[])
        .await
        .unwrap();
    assert_eq!(answer.text, "Greet the user");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);

    apply("[model_help]\nprovider = \"claude\"\nenabled = false\n");
    let err = assist
        .generate(AssistKind::Title, "another prompt", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "disabled");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);

    apply("[model_help]\nprovider = \"claude\"\n");
    assist
        .generate(AssistKind::Title, "another prompt", &[])
        .await
        .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    st.manager.shutdown(StopMode::Kill).await;
}
