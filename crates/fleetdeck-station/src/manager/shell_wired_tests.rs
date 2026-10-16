//! The `!` shell commands at the station (B8): gates, refusals, the root-busy
//! rule, release, interrupt, drain, restart, retirement, the advance loop and
//! the start-up settlement. No command runs yet: the execution is a later
//! task, so the rows that these tests need are planted in the store.

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{CommandResult, StopMode};
use fleetdeck_proto::event::{ProviderKind, ToolStatus};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::CommandAuthority;

use super::*;
use crate::store::shell_runs::ShellState;
use crate::store::{Caller, EffectPayload};

#[path = "shell_revert_tests.rs"]
mod shell_revert;
#[path = "shell_work_tests.rs"]
mod shell_work;

const BOARD_ONLY: &str = "Shell commands come only from the board.";
const NO_COORDINATOR: &str = "A coordinator cannot run shell commands.";

impl Station {
    /// A paired deck with `drive` (G5: the grant is read in the decision).
    async fn paired_deck(&self) -> Caller {
        let key = Identity::generate().unwrap().public_key();
        self.manager
            .store()
            .pin_peer(crate::store::peers::PairedKey {
                key,
                role: fleetdeck_link::tls::Role::Deck,
                scopes: [Scope::Read, Scope::Drive, Scope::Admin]
                    .into_iter()
                    .collect(),
                dial_url: None,
            })
            .await
            .unwrap();
        Caller::peer(key.fingerprint(), Scope::Admin)
    }

    async fn submit_as(
        &mut self,
        caller: &Caller,
        origin: CommandOrigin,
        authority: Option<CommandAuthority>,
        command: SessionCommand,
    ) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority,
            command_id: CommandId::from(format!("shell-cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin,
            command,
        };
        self.manager.submit(envelope, caller.clone()).await.unwrap()
    }

    /// A command from the board through a paired deck.
    async fn as_board(&mut self, deck: &Caller, command: SessionCommand) -> Decided {
        self.submit_as(deck, CommandOrigin::Board, None, command)
            .await
    }

    /// A session in `cwd` that finished its first run, effects settled. (The
    /// Claude CLI answers only after the first user message.)
    async fn idle_at(&mut self, cwd: &str) -> SessionId {
        let s = self.start(cwd, Some("hello")).await;
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let done = self.events().await.into_iter().any(|e| {
                e.session_id == s && matches!(e.event, EventBody::Known(Event::RunCompleted { .. }))
            });
            if done {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "no run.completed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.settled(&s).await;
        s
    }

    async fn idle_here(&mut self) -> SessionId {
        let ws = self.workspace();
        self.idle_at(&ws).await
    }

    /// The canonical root of the workspace, as the station names it.
    fn ws_root(&self) -> String {
        std::fs::canonicalize(self.workspace())
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// Stores a shell row without an effect, so no worker touches it.
    async fn plant(&self, id: &str, session: &SessionId, root: &str, state: ShellState) {
        let store = self.manager.store().clone();
        let (effect, owner, root) = (id.to_owned(), session.clone(), root.to_owned());
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("plant-{id}")),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command: SessionCommand::Interrupt {
                session_id: session.clone(),
            },
        };
        let d = store
            .decide(envelope, Caller::local(), move |d| {
                d.shell_insert(&effect, &owner, ProviderKind::Claude, &root, "sleep 9")?;
                Ok(CommandResult {
                    session_id: Some(owner.clone()),
                    input_id: None,
                })
            })
            .await
            .unwrap();
        assert_eq!(d.receipt.state, ReceiptState::Accepted);
        match state {
            ShellState::Queued => {}
            ShellState::Running => assert!(store.shell_set_running(id.to_owned()).await.unwrap()),
            ShellState::AwaitingProof => assert!(
                store
                    .shell_set_awaiting_proof(id.to_owned(), ShellState::Queued)
                    .await
                    .unwrap()
            ),
            other => panic!("plant a non-terminal state, not {other:?}"),
        }
    }

    async fn shell_row(&self, id: &str) -> crate::store::shell_runs::ShellRow {
        self.manager
            .store()
            .shell_run(id.to_owned())
            .await
            .unwrap()
            .unwrap()
    }

    /// Waits until no row waits for its advance.
    async fn advanced(&self) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            self.manager.shell_notify.notify_one();
            if self
                .manager
                .store()
                .shell_pending_advances()
                .await
                .unwrap()
                .is_empty()
            {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "no advance");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn shell(s: &SessionId, command: &str) -> SessionCommand {
    SessionCommand::ShellCommand {
        session_id: s.clone(),
        command: command.to_owned(),
    }
}

fn release(s: &SessionId, effect_id: &str) -> SessionCommand {
    SessionCommand::ReleaseShell {
        session_id: s.clone(),
        effect_id: effect_id.to_owned(),
    }
}

fn refused(d: &Decided) -> (RejectCode, String) {
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    let r = d.receipt.reject.clone().unwrap();
    (r.code, r.message)
}

fn pairs(code: RejectCode, text: &str) -> (RejectCode, String) {
    (code, text.to_owned())
}

#[tokio::test]
async fn the_station_start_opens_the_shell_admission() {
    let st = Station::new(4).await;
    assert!(st.manager.store().shell_admission_open().await.unwrap());
    st.manager.shutdown(StopMode::Kill).await;
    // The shutdown closes it before anything else.
    assert!(!st.manager.store().shell_admission_open().await.unwrap());
}

#[tokio::test]
async fn only_the_board_through_a_paired_deck_runs_a_shell_command() {
    let mut st = Station::new(4).await;
    let s = st.idle_here().await;
    let deck = st.paired_deck().await;
    // The local CLI is no peer.
    let d = st
        .submit_as(
            &Caller::local(),
            CommandOrigin::Board,
            None,
            shell(&s, "ls"),
        )
        .await;
    assert_eq!(refused(&d), pairs(RejectCode::Forbidden, BOARD_ONLY));
    // Another origin of a peer.
    for origin in [CommandOrigin::Local, CommandOrigin::Mail] {
        let d = st.submit_as(&deck, origin, None, shell(&s, "ls")).await;
        assert_eq!(refused(&d), pairs(RejectCode::Forbidden, BOARD_ONLY));
    }
    // A coordinator origin, and an authority on a board command.
    let d = st
        .submit_as(&deck, CommandOrigin::Coordinator, None, shell(&s, "ls"))
        .await;
    assert_eq!(refused(&d), pairs(RejectCode::Forbidden, NO_COORDINATOR));
    let authority = CommandAuthority {
        grant_id: "g".to_owned(),
        caller_station: "alpha".into(),
        caller_session: s.clone(),
        generation: 1,
    };
    let d = st
        .submit_as(
            &deck,
            CommandOrigin::Board,
            Some(authority),
            shell(&s, "ls"),
        )
        .await;
    assert_eq!(refused(&d), pairs(RejectCode::Forbidden, NO_COORDINATOR));
    // The same gates for the release.
    let d = st
        .submit_as(
            &Caller::local(),
            CommandOrigin::Board,
            None,
            release(&s, "e"),
        )
        .await;
    assert_eq!(refused(&d), pairs(RejectCode::Forbidden, BOARD_ONLY));
    let d = st
        .submit_as(&deck, CommandOrigin::Coordinator, None, release(&s, "e"))
        .await;
    assert_eq!(refused(&d), pairs(RejectCode::Forbidden, NO_COORDINATOR));
    assert!(
        st.manager
            .store()
            .shell_runs_unsettled()
            .await
            .unwrap()
            .is_empty()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_shell_command_needs_allow_shell_and_a_text_of_one_to_4096_characters() {
    let mut st = Station::new(4).await;
    let s = st.idle_here().await;
    let deck = st.paired_deck().await;
    let hub = Arc::clone(st.manager.policy_hub());
    let policy = hub.current().as_ref().clone();
    hub.swap(policy.clone().with_shell(false, 4));
    let d = st.as_board(&deck, shell(&s, "ls")).await;
    assert_eq!(
        refused(&d),
        pairs(
            RejectCode::Forbidden,
            "Shells are off on this station (allow_shell = false)."
        )
    );
    hub.swap(policy.with_shell(true, 4));
    for bad in [String::new(), "a".repeat(4097), "a\0b".to_owned()] {
        let d = st.as_board(&deck, shell(&s, &bad)).await;
        assert_eq!(
            refused(&d),
            pairs(
                RejectCode::InvalidCommand,
                "A shell command has 1 to 4096 characters and no NUL."
            )
        );
    }
    assert!(
        st.manager
            .store()
            .shell_runs_unsettled()
            .await
            .unwrap()
            .is_empty()
    );
    // The longest text is accepted.
    let d = st.as_board(&deck, shell(&s, &"a".repeat(4096))).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_accepted_shell_command_stores_its_row_and_effect_and_a_retirement_settles_it() {
    let mut st = Station::new(4).await;
    let s = st.idle_here().await;
    let deck = st.paired_deck().await;
    let d = st.as_board(&deck, shell(&s, "ls -la")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert_eq!(d.effects.len(), 1);
    let effect = &d.effects[0];
    let EffectPayload::ShellCommand {
        command,
        provider,
        worktree_root,
    } = &effect.payload
    else {
        panic!("{:?}", effect.payload);
    };
    assert_eq!(command, "ls -la");
    assert_eq!(*provider, ProviderKind::Claude);
    assert_eq!(*worktree_root, st.ws_root());
    assert_eq!(effect.payload.kind(), "shell_command");
    let row = st.shell_row(effect.effect_id.as_str()).await;
    assert_eq!(row.session_id, s);
    assert_eq!(row.command, "ls -la");
    assert_eq!(row.worktree_root, st.ws_root());

    // The execution is not built: the dispatch refuses, the effect retires
    // and the same transaction settles the row `cancelled`; the advance
    // loop then clears the pending marker and counts the generation.
    let id = effect.effect_id.as_str().to_owned();
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.shell_row(&id).await.state != ShellState::Cancelled {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            st.shell_row(&id).await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    st.advanced().await;
    let row = st.shell_row(&id).await;
    assert!(!row.advance_pending);
    assert_eq!(
        st.manager
            .store()
            .root_generation(st.ws_root())
            .await
            .unwrap(),
        1
    );
    assert!(
        !st.manager
            .store()
            .shell_root_unsettled(st.ws_root())
            .await
            .unwrap()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_shell_command_is_refused_for_a_busy_ended_hibernated_or_unknown_session() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let deck = st.paired_deck().await;
    // An unknown session.
    let d = st.as_board(&deck, shell(&"nope".into(), "ls")).await;
    assert_eq!(
        refused(&d),
        pairs(RejectCode::UnknownSession, "no such session")
    );
    // A session with a running turn.
    let busy = st.start(&ws, Some("hello")).await;
    st.wait_for("run completed", |e| {
        matches!(e, Event::RunCompleted { .. }).then_some(())
    })
    .await;
    st.settled(&busy).await;
    st.accepted(SessionCommand::Prompt {
        session_id: busy.clone(),
        input: text("wait for the host"),
    })
    .await;
    st.wait_for("streaming", |e| {
        matches!(e, Event::MessageDelta { .. }).then_some(())
    })
    .await;
    let d = st.as_board(&deck, shell(&busy, "ls")).await;
    assert_eq!(
        refused(&d),
        pairs(
            RejectCode::InvalidState,
            "Wait for the current turn or command to end."
        )
    );
    // An ended session.
    st.accepted(SessionCommand::Stop {
        session_id: busy.clone(),
        how: StopMode::Kill,
    })
    .await;
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    let d = st.as_board(&deck, shell(&busy, "ls")).await;
    assert_eq!(
        refused(&d),
        pairs(RejectCode::InvalidState, "the session has ended")
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_shell_command_in_a_hibernated_session_asks_to_resume_it_first() {
    use crate::coordinator::Bindings;
    use crate::manager::hibernate;
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let deck = st.paired_deck().await;
    let s = st.start(&ws, Some("hello")).await;
    st.wait_for("run completed", |e| {
        matches!(e, Event::RunCompleted { .. }).then_some(())
    })
    .await;
    st.det.advance(Duration::from_millis(30 * 60_000 + 1));
    let store = st.manager.store().clone();
    let policy = st.manager.policy().clone();
    assert_eq!(
        hibernate::sweep(&store, &policy, &Bindings::default(), 30)
            .await
            .unwrap(),
        1
    );
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    st.settled(&s).await;
    let d = st.as_board(&deck, shell(&s, "ls")).await;
    assert_eq!(
        refused(&d),
        pairs(RejectCode::InvalidState, "Resume the session first.")
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_unsettled_row_makes_the_whole_root_busy_for_every_session_of_it() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let root = st.ws_root();
    let deck = st.paired_deck().await;
    let first = st.idle_here().await;
    let second = st.idle_here().await;
    let other_dir = st.root.path().join("projects/other");
    std::fs::create_dir_all(&other_dir).unwrap();
    let other = st.idle_at(&other_dir.to_string_lossy()).await;
    st.plant("row-1", &first, &root, ShellState::Running).await;

    let busy = pairs(
        RejectCode::InvalidState,
        "A shell command is running in this folder.",
    );
    let commands = [
        SessionCommand::Prompt {
            session_id: second.clone(),
            input: text("hi"),
        },
        SessionCommand::FollowUp {
            session_id: second.clone(),
            input: text("hi"),
        },
        SessionCommand::Steer {
            session_id: second.clone(),
            input: text("hi"),
        },
        SessionCommand::Start {
            spec: spec(&ws, "claude"),
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        },
        SessionCommand::Import {
            spec: spec(&ws, "claude"),
            native_session_id: "native-1".into(),
            prompt: None,
            transcript: None,
        },
        SessionCommand::Resume {
            session_id: second.clone(),
        },
        SessionCommand::Revert {
            session_id: second.clone(),
            turn_id: "turn-1".into(),
        },
        SessionCommand::ResolveRevert {
            session_id: second.clone(),
            revert_id: "rv-1".to_owned(),
            resolution: fleetdeck_proto::command::RevertResolution::RetryRestore,
        },
    ];
    for command in commands {
        let name = command.method();
        let d = st.send(command).await;
        assert_eq!(refused(&d), busy, "{name}");
    }
    // The shell command of a second session of the root.
    let d = st.as_board(&deck, shell(&second, "ls")).await;
    assert_eq!(refused(&d), busy);
    // A session of another root is not busy.
    st.accepted(SessionCommand::Prompt {
        session_id: other.clone(),
        input: text("hi"),
    })
    .await;
    // Stop is admitted.
    st.accepted(SessionCommand::Stop {
        session_id: second.clone(),
        how: StopMode::Kill,
    })
    .await;
    // The row settles, and the root stays busy until its advance ran.
    let store = st.manager.store().clone();
    assert!(
        store
            .shell_settle("row-1".to_owned(), ShellState::Done, Some(0), None, None)
            .await
            .unwrap()
    );
    assert!(store.shell_root_unsettled(root.clone()).await.unwrap());
    st.advanced().await;
    assert!(!store.shell_root_unsettled(root).await.unwrap());
    st.accepted(SessionCommand::Prompt {
        session_id: first,
        input: text("hi"),
    })
    .await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_interrupt_cancels_the_shell_of_its_session_and_needs_no_run() {
    let mut st = Station::new(4).await;
    let root = st.ws_root();
    let s = st.idle_here().await;
    // No row and no run: the usual refusal.
    assert_eq!(
        st.rejected(SessionCommand::Interrupt {
            session_id: s.clone()
        })
        .await,
        RejectCode::InvalidState
    );
    st.plant("row-1", &s, &root, ShellState::Running).await;
    let d = st
        .accepted(SessionCommand::Interrupt {
            session_id: s.clone(),
        })
        .await;
    assert!(d.effects.is_empty(), "no run, so no interrupt effect");
    let row = st.shell_row("row-1").await;
    assert!(row.cancel_requested);
    assert_eq!(row.state, ShellState::Running);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_owner_releases_a_command_that_awaits_proof_and_the_advance_follows() {
    let mut st = Station::new(4).await;
    let root = st.ws_root();
    let deck = st.paired_deck().await;
    let s = st.idle_here().await;
    let other = st.idle_here().await;
    st.plant("proof", &s, &root, ShellState::AwaitingProof)
        .await;
    st.plant("alien", &other, &root, ShellState::AwaitingProof)
        .await;

    // Unknown session, unknown id, the row of another session.
    let d = st.as_board(&deck, release(&"nope".into(), "proof")).await;
    assert_eq!(
        refused(&d),
        pairs(RejectCode::UnknownSession, "no such session")
    );
    for id in ["missing", "alien"] {
        let d = st.as_board(&deck, release(&s, id)).await;
        assert_eq!(
            refused(&d),
            pairs(
                RejectCode::InvalidCommand,
                "No shell command with this id in this session."
            ),
            "{id}"
        );
    }

    let d = st.as_board(&deck, release(&s, "proof")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let row = st.shell_row("proof").await;
    assert_eq!(row.state, ShellState::Unknown);
    assert!(row.advance_pending);
    let completed: Vec<_> = d
        .events
        .iter()
        .filter_map(|e| match &e.event {
            EventBody::Known(Event::ToolCompleted {
                node_id,
                status,
                exit_code,
                provider_refs,
                ..
            }) => Some((
                node_id.as_str().to_owned(),
                *status,
                *exit_code,
                provider_refs.get("shell").cloned(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        completed,
        vec![(
            "shell-proof".to_owned(),
            ToolStatus::Failed,
            None,
            Some("released".into())
        )]
    );
    let notice = d.events.iter().find_map(|e| match &e.event {
        EventBody::Known(Event::SessionNotice { code, text }) => Some((code.clone(), text.clone())),
        _ => None,
    });
    assert_eq!(
        notice,
        Some(("shell_released".to_owned(), format!("proof {root}")))
    );
    // The advance follows the release, and the folder is busy until then
    // (the other session's row still blocks it, so look at the generation).
    st.advanced().await;
    assert!(!st.shell_row("proof").await.advance_pending);
    assert_eq!(st.manager.store().root_generation(root).await.unwrap(), 1);

    // A settled row is never released again.
    let d = st.as_board(&deck, release(&s, "proof")).await;
    assert_eq!(
        refused(&d),
        pairs(
            RejectCode::InvalidState,
            "This command has already settled."
        )
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_running_row_is_released_only_when_its_session_has_ended() {
    let mut st = Station::new(4).await;
    let root = st.ws_root();
    let deck = st.paired_deck().await;
    let s = st.idle_here().await;
    st.plant("run", &s, &root, ShellState::Running).await;
    let d = st.as_board(&deck, release(&s, "run")).await;
    assert_eq!(
        refused(&d),
        pairs(
            RejectCode::InvalidState,
            "This command has already settled."
        )
    );
    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Kill,
    })
    .await;
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    // The ended session takes the release, also with no live process.
    let d = st.as_board(&deck, release(&s, "run")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert_eq!(st.shell_row("run").await.state, ShellState::Unknown);
    st.advanced().await;
    st.manager.shutdown(StopMode::Kill).await;
}
