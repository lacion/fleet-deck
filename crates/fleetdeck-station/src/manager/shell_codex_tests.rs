//! The `!` shell commands of Codex sessions at the station (B8): the
//! dispatch through `thread/shellCommand` against `fake-codex sim`, the
//! settlement from the provider events, the watch, the start-up and the
//! proof tick, auto-continue and the revert barrier.

use fleetdeck_proto::event::RunOrigin;
use fleetdeck_providers::codex::capabilities as codex_capabilities;

use super::*;
use crate::manager::auto_continue::evaluate;
use crate::manager::turns::Fences;
use crate::store::shell_runs::ShellRow;
use crate::store::shims::{NewShim, ShimState};
use crate::store::{EffectState, NewSession, OfflineApprovalPolicy, Store, StoreConfig};

const BUSY: &str = "A shell command is running in this folder.";

/// A Codex session that finished its first run, effects settled.
async fn codex_idle(st: &mut Station) -> SessionId {
    let ws = st.workspace();
    let mut spec = spec(&ws, "codex");
    spec.mode = None;
    let s = st.start_with(spec, Some("hello")).await;
    st.wait_for("run completed", |e| {
        matches!(e, Event::RunCompleted { .. }).then_some(())
    })
    .await;
    st.settled(&s).await;
    s
}

/// Submits `!command` as the board; returns the effect id.
async fn bang(st: &mut Station, deck: &Caller, s: &SessionId, command: &str) -> String {
    let d = st.as_board(deck, shell(s, command)).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    d.effects[0].effect_id.as_str().to_owned()
}

async fn row_is(st: &Station, id: &str, pick: impl Fn(&ShellRow) -> bool) -> ShellRow {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let row = st.shell_row(id).await;
        if pick(&row) {
            return row;
        }
        assert!(tokio::time::Instant::now() < deadline, "{row:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The shell effect of `s` and its state.
async fn shell_effect_state(st: &Station, s: &SessionId) -> EffectState {
    st.effect_states(s)
        .await
        .into_iter()
        .find(|(kind, _)| *kind == "shell_command")
        .map(|(_, state)| state)
        .unwrap()
}

fn node_completed(id: &str) -> impl Fn(&Event) -> Option<String> + '_ {
    move |e| match e {
        Event::ToolCompleted {
            node_id,
            provider_refs,
            ..
        } if node_id.as_str() == format!("shell-{id}") => provider_refs
            .get("shell")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        _ => None,
    }
}

/// A Codex command that the sim never answers: its row is `running` and its
/// effect `claimed`, as after a crash of the station.
async fn sent_and_lost(st: &mut Station, deck: &Caller, s: &SessionId) -> String {
    let id = bang(st, deck, s, "noack").await;
    row_is(st, &id, |r| r.state == ShellState::Running).await;
    assert_eq!(shell_effect_state(st, s).await, EffectState::Claimed);
    id
}

async fn shim_for(st: &Station, session: &SessionId, shim_id: &str) {
    st.manager
        .store()
        .shim_starting(NewShim {
            shim_id: shim_id.to_owned(),
            session_id: session.clone(),
            socket_path: "/tmp/none.sock".into(),
            launch_boot_id: None,
            native_target: None,
        })
        .await
        .unwrap();
}

async fn shim_proves_end(st: &Station, shim_id: &str) {
    assert!(
        st.manager
            .store()
            .shim_end(
                shim_id.to_owned(),
                ShimState::Exited,
                "exited".to_owned(),
                1
            )
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_codex_shell_command_runs_natively_and_settles_done() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = bang(&mut st, &deck, &s, "pwd").await;
    let row = row_is(&st, &id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Done);
    assert_eq!(row.exit_code, Some(0));
    st.advanced().await;
    assert!(!st.shell_row(&id).await.advance_pending);
    // The generation advanced at the start and at the settlement.
    let store = st.manager.store().clone();
    assert_eq!(store.root_generation(st.ws_root()).await.unwrap(), 2);
    // The first provider evidence confirmed the effect.
    let deadline = tokio::time::Instant::now() + WAIT;
    while shell_effect_state(&st, &s).await != EffectState::Confirmed {
        assert!(tokio::time::Instant::now() < deadline, "not confirmed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        st.wait_for("shell end", node_completed(&id)).await,
        "done".to_owned()
    );
    st.wait_for("shell run", |e| match e {
        Event::RunStarted {
            origin: RunOrigin::Shell,
            input_summary,
            ..
        } if input_summary == "!pwd" => Some(()),
        _ => None,
    })
    .await;
    st.wait_for("shell node output", |e| match e {
        Event::ToolOutput { node_id, chunk, .. }
            if node_id.as_str() == format!("shell-{id}") && chunk == "sim: pwd\n" =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_running_codex_shell_blocks_input_and_an_interrupt_cancels_it() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = bang(&mut st, &deck, &s, "hang").await;
    st.wait_for("shell node", |e| match e {
        Event::ToolStarted { node_id, .. } if node_id.as_str() == format!("shell-{id}") => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(st.shell_row(&id).await.state, ShellState::Running);
    let d = st
        .send(SessionCommand::Prompt {
            session_id: s.clone(),
            input: text("while it runs"),
        })
        .await;
    assert_eq!(refused(&d), pairs(RejectCode::InvalidState, BUSY));
    st.as_board(
        &deck,
        SessionCommand::Interrupt {
            session_id: s.clone(),
        },
    )
    .await;
    let row = row_is(&st, &id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Cancelled);
    assert_eq!(
        st.wait_for("shell end", node_completed(&id)).await,
        "cancelled".to_owned()
    );
    // The folder and the session are free again.
    st.advanced().await;
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("after"),
    })
    .await;
    st.wait_for("the prompt echo", |e| match e {
        Event::MessageCompleted { text, .. } if text == "echo: after" => Some(()),
        _ => None,
    })
    .await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_codex_command_without_a_reply_awaits_proof_and_the_owner_releases_it() {
    let mut st = Station::new(4).await;
    st.manager
        .target
        .set_shell_watch(Duration::from_millis(200));
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = bang(&mut st, &deck, &s, "noack").await;
    let row = row_is(&st, &id, |r| r.state == ShellState::AwaitingProof).await;
    assert!(!row.advance_pending);
    // (B8 PRE-PR r1) The watch gives the command a card for the Release.
    let cards = cards(&st, &id).await;
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["station"], "awaiting_proof");
    // The card does not confirm the effect.
    assert_eq!(shell_effect_state(&st, &s).await, EffectState::Claimed);
    let d = st.as_board(&deck, release(&s, &id)).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let row = row_is(&st, &id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Unknown);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_refused_codex_command_fails_with_the_text_of_codex() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = bang(&mut st, &deck, &s, "refuse it").await;
    let row = row_is(&st, &id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Failed);
    st.wait_for("the refusal text", |e| match e {
        Event::ToolOutput { node_id, chunk, .. }
            if node_id.as_str() == format!("shell-{id}")
                && chunk == "Codex refused the command: refused (-32600)" =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    st.advanced().await;
    st.manager.shutdown(StopMode::Kill).await;
}

/// The `tool.started` events of the node of `id` (B8 PRE-PR r1).
async fn cards(st: &Station, id: &str) -> Vec<serde_json::Map<String, serde_json::Value>> {
    match st.manager.store().read_after(0, 100_000).await.unwrap() {
        ReadResult::Events { events, .. } => events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::ToolStarted {
                    node_id,
                    provider_refs,
                    ..
                }) if node_id.as_str() == format!("shell-{id}") => Some(provider_refs),
                _ => None,
            })
            .collect(),
        ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
    }
}

#[tokio::test]
async fn the_start_up_awaits_the_proof_of_a_direct_host_row_and_gives_it_a_card() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = sent_and_lost(&mut st, &deck, &s).await;
    assert!(cards(&st, &id).await.is_empty());
    let store = st.manager.store().clone();
    shell_advance::start_up(&store, &Fences::default())
        .await
        .unwrap();
    let row = st.shell_row(&id).await;
    assert_eq!(row.state, ShellState::AwaitingProof);
    assert!(!row.advance_pending);
    let cards = cards(&st, &id).await;
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["station"], "awaiting_proof");
    // The station card is no evidence: the effect stays claimed.
    assert_eq!(shell_effect_state(&st, &s).await, EffectState::Claimed);
    // The tick leaves a direct-host row alone.
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(st.shell_row(&id).await.state, ShellState::AwaitingProof);
    let d = st.as_board(&deck, release(&s, &id)).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let row = row_is(&st, &id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Unknown);
    assert_eq!(
        st.wait_for("the release end", node_completed(&id)).await,
        "released".to_owned()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_start_up_adds_no_card_to_a_row_that_has_one() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = bang(&mut st, &deck, &s, "hang").await;
    st.wait_for("shell node", |e| match e {
        Event::ToolStarted { node_id, .. } if node_id.as_str() == format!("shell-{id}") => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(cards(&st, &id).await.len(), 1);
    let store = st.manager.store().clone();
    shell_advance::start_up(&store, &Fences::default())
        .await
        .unwrap();
    assert_eq!(st.shell_row(&id).await.state, ShellState::AwaitingProof);
    let cards = cards(&st, &id).await;
    assert_eq!(cards.len(), 1);
    assert!(!cards[0].contains_key("station"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_codex_row_stays_running_until_the_turn_completes() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = bang(&mut st, &deck, &s, "slowend").await;
    // The item completed; the turn has not.
    st.wait_for("shell output", |e| match e {
        Event::ToolOutput { node_id, .. } if node_id.as_str() == format!("shell-{id}") => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(st.shell_row(&id).await.state, ShellState::Running);
    let d = st
        .send(SessionCommand::Prompt {
            session_id: s.clone(),
            input: text("while it ends"),
        })
        .await;
    assert_eq!(refused(&d), pairs(RejectCode::InvalidState, BUSY));
    let row = row_is(&st, &id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Done);
    assert_eq!(
        st.wait_for("shell end", node_completed(&id)).await,
        "done".to_owned()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_start_up_keeps_a_shim_row_without_proof_and_the_tick_settles_it_with_proof() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = sent_and_lost(&mut st, &deck, &s).await;
    shim_for(&st, &s, "shim-1").await;
    let store = st.manager.store().clone();
    shell_advance::start_up(&store, &Fences::default())
        .await
        .unwrap();
    assert_eq!(st.shell_row(&id).await.state, ShellState::AwaitingProof);
    // The tick leaves it while the shim has no proof.
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(st.shell_row(&id).await.state, ShellState::AwaitingProof);
    shim_proves_end(&st, "shim-1").await;
    let row = row_is(&st, &id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Unknown);
    assert_eq!(
        st.wait_for("the unknown end", node_completed(&id)).await,
        "unknown".to_owned()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_start_up_settles_a_shim_row_with_proof_unknown() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let id = sent_and_lost(&mut st, &deck, &s).await;
    shim_for(&st, &s, "shim-1").await;
    shim_proves_end(&st, "shim-1").await;
    let store = st.manager.store().clone();
    shell_advance::start_up(&store, &Fences::default())
        .await
        .unwrap();
    assert_eq!(st.shell_row(&id).await.state, ShellState::Unknown);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn auto_continue_skips_a_lost_run_of_a_shell_command() {
    let det = fleetdeck_testkit::Det::new();
    det.set(1_000_000);
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("projects/app");
    std::fs::create_dir_all(&cwd).unwrap();
    let s = SessionId::from("shell-lost");
    store
        .create_session(NewSession {
            session_id: s.clone(),
            provider: ProviderKind::Codex,
            provider_instance: "codex".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    store
        .process_started(s.clone(), "proc-1".to_owned().into(), None)
        .await
        .unwrap();
    let mut refs = serde_json::Map::new();
    refs.insert("thread_id".into(), "native-shell".into());
    let events = vec![
        Event::SessionStarted {
            provider: ProviderKind::Codex,
            provider_instance: "codex".into(),
            provider_version: None,
            model: None,
            cwd: cwd.to_string_lossy().into_owned(),
            mode: None,
            capabilities: Box::new(codex_capabilities(false)),
            resumed: None,
            provider_refs: refs,
            title: None,
            branch: None,
            repo: None,
        },
        Event::AutoContinueChanged { enabled: true },
        Event::RunStarted {
            author: None,
            input_id: None,
            run_id: "run-1".into(),
            input_summary: "!pwd".to_owned(),
            origin: RunOrigin::Shell,
            input_text: None,
        },
        Event::TurnStarted {
            run_id: "run-1".into(),
            turn_id: "turn-1".into(),
            provider_refs: serde_json::Map::new(),
        },
    ];
    store
        .append(s.clone(), events.into_iter().map(Into::into).collect())
        .await
        .unwrap();
    let head = store.head();
    store.recover(Vec::new()).await.unwrap();
    let policy = Policy::new(
        vec![root.path().join("projects")],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("codex".into(), ProviderKind::Codex)]),
    );
    assert_eq!(evaluate(&store, &policy, head).await.unwrap(), 0);
    let skipped: Vec<String> = match store.read_after(0, 1000).await.unwrap() {
        ReadResult::Events { events, .. } => events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::AutoContinueSkipped { reason }) => Some(reason),
                _ => None,
            })
            .collect(),
        ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
    };
    assert_eq!(skipped, vec!["maintenance_turn".to_owned()]);
}

#[tokio::test]
async fn a_revert_over_a_shell_command_is_refused() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let dir = PathBuf::from(&ws);
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "init"][..],
    ] {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    let deck = st.paired_deck().await;
    let s = codex_idle(&mut st).await;
    let first = st
        .wait_for("the first finalized turn", |e| match e {
            Event::TurnFinalized { turn_id, .. } => Some(turn_id.clone()),
            _ => None,
        })
        .await;
    let id = bang(&mut st, &deck, &s, "pwd").await;
    row_is(&st, &id, |r| r.state.is_terminal()).await;
    st.advanced().await;
    let d = st
        .send(SessionCommand::Revert {
            session_id: s.clone(),
            turn_id: first,
        })
        .await;
    assert_eq!(
        refused(&d),
        pairs(
            RejectCode::InvalidState,
            "A shell command ran in this folder after this turn; the files cannot be reverted across it."
        )
    );
    st.manager.shutdown(StopMode::Kill).await;
}
