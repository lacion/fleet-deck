//! The station side of fleet control (ADR-008 §3, §5, §6.2; tests 8d, 8e,
//! 11, 11a, 12 and 14 of §9): the rules at the decision, the recheck at the
//! dispatch with the `coordinator.authority_check` round trip, no implicit
//! continuation by a coordinator, the answer rules, and
//! `coordinator.policy_now`.

use std::sync::Mutex;

use async_trait::async_trait;
use fleetdeck_proto::command::CommandOrigin;
use fleetdeck_proto::event::ResolvedBy;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::mutation::{AuthorityCheck, CommandAuthority, PolicyNow};
use tokio::sync::Notify;

use super::policy_hub::test_config;
use super::*;
use crate::coordinator::{Binding, BindingState, Bindings, ToolClass, Transport};
use crate::manager::fleet_control::policy_now_answer;
use crate::manager::hibernate;
use crate::store::{Effect, EffectPayload, NewSession};

fn authority(caller: &str) -> CommandAuthority {
    CommandAuthority {
        grant_id: "g1".to_owned(),
        caller_station: "alpha".into(),
        caller_session: caller.into(),
        generation: 1,
    }
}

/// How the fake deck answers `coordinator.authority_check`.
#[derive(Clone, Copy)]
enum Reply {
    Valid,
    Invalid,
    Forbidden,
    /// `valid: false` for a launch mode above the grant ceiling.
    ModeNotAllowed,
    OtherError,
    LinkGone,
}

struct FakeDeck {
    reply: Mutex<Reply>,
    asked: Mutex<Vec<AuthorityCheck>>,
    /// The first request waits here until the test lets it go.
    gate: Option<Arc<Notify>>,
}

impl FakeDeck {
    fn new(reply: Reply) -> Arc<Self> {
        Arc::new(Self {
            reply: Mutex::new(reply),
            asked: Mutex::default(),
            gate: None,
        })
    }

    fn held(reply: Reply) -> (Arc<Self>, Arc<Notify>) {
        let gate = Arc::new(Notify::new());
        let deck = Arc::new(Self {
            reply: Mutex::new(reply),
            asked: Mutex::default(),
            gate: Some(Arc::clone(&gate)),
        });
        (deck, gate)
    }

    fn set(&self, reply: Reply) {
        *self.reply.lock().unwrap() = reply;
    }

    fn asked(&self) -> Vec<AuthorityCheck> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl Transport for FakeDeck {
    async fn request(
        &self,
        method: &str,
        command_id: Option<&CommandId>,
        params: serde_json::Value,
    ) -> Result<Outcome, String> {
        assert_eq!(method, "coordinator.authority_check");
        assert!(command_id.is_none());
        let check: AuthorityCheck = serde_json::from_value(params).unwrap();
        self.asked.lock().unwrap().push(check);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        let reply = *self.reply.lock().unwrap();
        match reply {
            Reply::Valid => Ok(Outcome::Ok(serde_json::json!({"valid": true}))),
            Reply::Invalid => Ok(Outcome::Ok(
                serde_json::json!({"valid": false, "reason": "revoked"}),
            )),
            Reply::ModeNotAllowed => Ok(Outcome::Ok(
                serde_json::json!({"valid": false, "reason": "mode_not_allowed"}),
            )),
            Reply::Forbidden => Ok(Outcome::Err(WireError::new("forbidden", "no"))),
            Reply::OtherError => Ok(Outcome::Err(WireError::new("protocol_error", "bad"))),
            Reply::LinkGone => Err("the link ended".to_owned()),
        }
    }
}

impl Station {
    /// A command as the deck routes it for a coordinator.
    async fn send_as(
        &mut self,
        origin: CommandOrigin,
        authority: Option<CommandAuthority>,
        command: SessionCommand,
    ) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority,
            command_id: CommandId::from(format!("fc-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin,
            command,
        };
        self.manager
            .submit(envelope, Caller::local())
            .await
            .unwrap()
    }

    /// The same, from the coordinator `coord-1`, and checks the receipt.
    async fn coord(&mut self, command: SessionCommand) -> Decided {
        self.send_as(
            CommandOrigin::Coordinator,
            Some(authority("coord-1")),
            command,
        )
        .await
    }

    async fn refused(&mut self, d: &Decided) -> (RejectCode, String) {
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
        assert!(d.effects.is_empty());
        let r = d.receipt.reject.clone().unwrap();
        (r.code, r.message)
    }

    fn link_deck(&self, deck: &Arc<FakeDeck>) {
        self.manager
            .coordinator_bindings()
            .set_deck(1, Arc::clone(deck) as Arc<dyn Transport>);
    }

    /// Turns `allow_fleet_control` on or off, as a publish does.
    fn switch(&self, on: bool) {
        let hub = self.manager.policy_hub();
        hub.swap(hub.current().as_ref().clone().with_allow_fleet_control(on));
    }

    async fn turns_done(&self, session: &SessionId) -> usize {
        self.events()
            .await
            .into_iter()
            .filter(|e| e.session_id == *session)
            .filter(|e| {
                matches!(
                    &e.event,
                    EventBody::Known(Event::TurnCompleted {
                        status: TurnStatus::Done,
                        ..
                    })
                )
            })
            .count()
    }

    async fn wait_turns(&self, session: &SessionId, n: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.turns_done(session).await < n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{:#?}",
                kinds(&self.events().await)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn effects(&self, session: &SessionId) -> Vec<Effect> {
        self.manager
            .store()
            .session_effects(session.clone())
            .await
            .unwrap()
    }

    /// Waits until the deck was asked `n` times.
    async fn wait_asked(&self, deck: &FakeDeck, n: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while deck.asked().len() < n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the deck was not asked"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The `error` events of a session, by code.
    async fn errors(&self, session: &SessionId) -> Vec<String> {
        self.events()
            .await
            .into_iter()
            .filter(|e| e.session_id == *session)
            .filter_map(|e| match e.event {
                EventBody::Known(Event::Error { code, .. }) => Some(code),
                _ => None,
            })
            .collect()
    }
}

fn follow_up(s: &SessionId, t: &str) -> SessionCommand {
    SessionCommand::FollowUp {
        session_id: s.clone(),
        input: text(t),
    }
}

/// A session that ran one turn and is idle, with the switch on.
async fn idle_session(st: &mut Station) -> SessionId {
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_turns(&s, 1).await;
    st.switch(true);
    s
}

fn coordinator_binding(session: &SessionId, classes: &[ToolClass], state: BindingState) -> Binding {
    Binding {
        binding_id: format!("{session}:g9"),
        grant_id: "g9".to_owned(),
        session_id: session.clone(),
        provider_instance: "claude".to_owned(),
        native_id: Some("native-9".to_owned()),
        generation: 1,
        classes: classes.iter().copied().collect(),
        scope: fleetdeck_proto::command::GrantScope::Fleet,
        state,
        control_skill: String::new(),
        kind: fleetdeck_proto::command::GrantKind::Coordinator,
    }
}

// ADR-008 §6.2, at the decision (tests 8e and 11, the decision part).
#[tokio::test]
async fn a_command_with_authority_is_refused_at_the_decision_by_the_three_station_rules() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_turns(&s, 1).await;
    let before = st.effects(&s).await.len();

    // The live switch is off: a stored `forbidden`, nothing queued.
    let d = st.coord(follow_up(&s, "x")).await;
    let (code, message) = st.refused(&d).await;
    assert_eq!(code, RejectCode::Forbidden);
    assert_eq!(message, "fleet control is off on this station");
    // The same command without authority is not a fleet-control command.
    st.accepted(follow_up(&s, "no authority")).await;
    st.wait_turns(&s, 2).await;
    let before = st.effects(&s).await.len().max(before);

    st.switch(true);
    // Rule 2: a coordinator never acts on its own session.
    let d = st
        .send_as(
            CommandOrigin::Coordinator,
            Some(authority(s.as_str())),
            follow_up(&s, "x"),
        )
        .await;
    let (code, message) = st.refused(&d).await;
    assert_eq!(code, RejectCode::Forbidden);
    assert_eq!(message, "a coordinator cannot act on its own session");
    // Rule 3: the target holds an active binding with a mutating class.
    let bindings = st.manager.coordinator_bindings().clone();
    bindings.insert(
        "digest-a".to_owned(),
        coordinator_binding(&s, &[ToolClass::Mail], BindingState::Active),
    );
    let d = st.coord(follow_up(&s, "x")).await;
    let (code, message) = st.refused(&d).await;
    assert_eq!(code, RejectCode::Forbidden);
    assert_eq!(message, "the target session is a coordinator");
    // Both rules at once: rule 3 comes first (the ADR order).
    let d = st
        .send_as(
            CommandOrigin::Coordinator,
            Some(authority(s.as_str())),
            follow_up(&s, "x"),
        )
        .await;
    assert_eq!(
        st.refused(&d).await.1,
        "the target session is a coordinator"
    );
    // A binding with only read or inspect classes, or a pending one, is not
    // a coordinator that mutates.
    for (classes, state) in [
        (
            vec![ToolClass::Read, ToolClass::Inspect],
            BindingState::Active,
        ),
        (vec![ToolClass::Mail], BindingState::Pending),
        (vec![ToolClass::Mail], BindingState::Revoked),
    ] {
        bindings.insert(
            "digest-a".to_owned(),
            coordinator_binding(&s, &classes, state),
        );
        let d = st.coord(follow_up(&s, "y")).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    }
    assert!(st.effects(&s).await.len() > before);
    st.manager.shutdown(StopMode::Kill).await;
}

// Test 11: the recheck at the dispatch, with the round trip.
#[tokio::test]
async fn an_effect_with_authority_runs_only_when_the_deck_confirms_it() {
    let mut st = Station::new(4).await;
    let s = idle_session(&mut st).await;
    let deck = FakeDeck::new(Reply::Valid);
    st.link_deck(&deck);

    // `valid`: the follow-up reaches the provider.
    let d = st.coord(follow_up(&s, "ok")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert_eq!(d.effects.len(), 1);
    assert_eq!(
        d.effects[0].payload.authority(),
        Some(&authority("coord-1")),
        "the effect carries the authority of its command"
    );
    st.wait_turns(&s, 2).await;
    let asked = deck.asked();
    assert_eq!(asked.len(), 1, "one outstanding check per effect");
    assert_eq!(
        asked[0],
        AuthorityCheck {
            grant_id: "g1".to_owned(),
            generation: 1,
            caller_station: "alpha".into(),
            caller_session: "coord-1".into(),
            target_session: s.clone(),
            command_id: d.receipt.command_id.clone(),
            launch: None,
        }
    );

    // Everything else refuses and writes the `error` event, once each, and
    // the provider gets nothing.
    let cases = [
        (Reply::Invalid, "authority_lost"),
        (Reply::Forbidden, "authority_lost"),
        (Reply::OtherError, "authority_unknown"),
        (Reply::LinkGone, "authority_unknown"),
    ];
    for (n, (reply, code)) in cases.into_iter().enumerate() {
        deck.set(reply);
        let d = st.coord(follow_up(&s, "no")).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let effects = st.effects(&s).await;
            let mine = effects
                .iter()
                .find(|e| e.command_id.as_ref() == Some(&d.receipt.command_id));
            if let Some(e) = mine.filter(|e| e.state == EffectState::RetiredNotDispatched) {
                assert_eq!(e.reason.as_deref(), Some(code));
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "{effects:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(deck.asked().len(), 2 + n);
    }
    let errors = st.errors(&s).await;
    assert_eq!(errors.iter().filter(|c| *c == "authority_lost").count(), 2);
    assert_eq!(
        errors.iter().filter(|c| *c == "authority_unknown").count(),
        2
    );
    let message = st
        .wait_for("the lost message", |e| match e {
            Event::Error { code, message, .. } if code == "authority_lost" => Some(message.clone()),
            _ => None,
        })
        .await;
    assert_eq!(
        message,
        "the coordinator's authority ended before this action ran"
    );
    assert_eq!(
        st.turns_done(&s).await,
        2,
        "no refused effect reached the provider"
    );

    // No deck link at all: `authority_unknown`.
    st.manager.coordinator_bindings().clear_deck(1);
    let d = st.coord(follow_up(&s, "alone")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    st.wait_for("the unknown message", |e| match e {
        Event::Error { code, message, .. } if code == "authority_unknown" => {
            (message == "the deck did not confirm the coordinator's authority").then_some(())
        }
        _ => None,
    })
    .await;
    st.manager.shutdown(StopMode::Kill).await;
}

/// Holds the effect worker in the round trip of a first effect, then
/// queues a second one behind it. The second passed the decision with the
/// switch on.
async fn held_pair(st: &mut Station) -> (SessionId, Arc<FakeDeck>, Arc<Notify>, Decided) {
    let s = idle_session(st).await;
    let (deck, gate) = FakeDeck::held(Reply::Valid);
    st.link_deck(&deck);
    let first = st.coord(follow_up(&s, "first")).await;
    assert_eq!(first.receipt.state, ReceiptState::Accepted);
    st.wait_asked(&deck, 1).await;
    let second = st.coord(follow_up(&s, "second")).await;
    assert_eq!(
        second.receipt.state,
        ReceiptState::Accepted,
        "{:?}",
        second.receipt
    );
    (s, deck, gate, second)
}

async fn wait_retired(st: &Station, s: &SessionId, command: &CommandId) -> Effect {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let effects = st.effects(s).await;
        if let Some(e) = effects
            .iter()
            .find(|e| e.command_id.as_ref() == Some(command))
            .filter(|e| e.state == EffectState::RetiredNotDispatched)
        {
            return e.clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "{effects:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// Tests 8e and 8d (station part): the target station's switch turned off
// between the decision and the dispatch. The effect that waits in its round
// trip is refused after the answer (the station reads the switch again,
// PRE-PR fix 1); the effect behind it is refused before the deck is asked.
#[tokio::test]
async fn the_switch_turned_off_between_decision_and_dispatch_stops_the_effect_without_a_deck_question()
 {
    let mut st = Station::new(4).await;
    let (s, deck, gate, second) = held_pair(&mut st).await;
    st.switch(false);
    gate.notify_one();
    let retired = wait_retired(&st, &s, &second.receipt.command_id).await;
    assert_eq!(retired.reason.as_deref(), Some("authority_lost"));
    assert_eq!(
        deck.asked().len(),
        1,
        "the live setting stops the second before the deck is asked"
    );
    assert_eq!(st.turns_done(&s).await, 1, "neither follow-up ran");
    assert_eq!(st.errors(&s).await, ["authority_lost", "authority_lost"]);
    st.manager.shutdown(StopMode::Kill).await;
}

// PRE-PR fix 1: the switch turned off while the deck answers `valid` is read
// again after the round trip: the provider gets no write.
#[tokio::test]
async fn the_switch_turned_off_between_the_round_trip_and_the_write_stops_the_effect() {
    let mut st = Station::new(4).await;
    let s = idle_session(&mut st).await;
    let (deck, gate) = FakeDeck::held(Reply::Valid);
    st.link_deck(&deck);
    let d = st.coord(follow_up(&s, "late")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    st.wait_asked(&deck, 1).await;
    st.switch(false);
    gate.notify_one();
    let retired = wait_retired(&st, &s, &d.receipt.command_id).await;
    assert_eq!(retired.reason.as_deref(), Some("authority_lost"));
    assert_eq!(st.turns_done(&s).await, 1, "the provider got no write");
    assert_eq!(st.errors(&s).await, ["authority_lost"]);
    st.manager.shutdown(StopMode::Kill).await;
}

// Test 11: a target that became a coordinator is refused at the dispatch.
#[tokio::test]
async fn a_target_that_became_a_coordinator_is_refused_at_the_dispatch() {
    let mut st = Station::new(4).await;
    let (s, deck, gate, second) = held_pair(&mut st).await;
    st.manager.coordinator_bindings().insert(
        "digest-b".to_owned(),
        coordinator_binding(&s, &[ToolClass::Drive], BindingState::Active),
    );
    gate.notify_one();
    let retired = wait_retired(&st, &s, &second.receipt.command_id).await;
    assert_eq!(retired.reason.as_deref(), Some("authority_lost"));
    assert_eq!(deck.asked().len(), 1);
    st.manager.shutdown(StopMode::Kill).await;
}

// A start with authority (`start_in_project`) is checked right before the
// spawn, and a refused start ends the session.
#[tokio::test]
async fn a_fleet_control_start_is_checked_before_the_spawn() {
    let mut st = Station::new(4).await;
    st.switch(true);
    let deck = FakeDeck::new(Reply::Invalid);
    st.link_deck(&deck);
    let start = |st: &Station| SessionCommand::Start {
        spec: spec(&st.workspace(), "claude"),
        prompt: Some(text("hello")),
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
    };
    let command = start(&st);
    let d = st.coord(command).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let s = d.receipt.result.unwrap().session_id.unwrap();
    assert!(matches!(
        d.effects[0].payload,
        EffectPayload::Start {
            authority: Some(_),
            ..
        }
    ));
    st.wait_for("the failed start", |e| match e {
        Event::SessionEnded { reason, .. } if reason == "start_failed" => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(st.errors(&s).await[0], "authority_lost");
    assert_eq!(st.state(&s).await, SessionState::Exited);
    assert_eq!(deck.asked().len(), 1);
    // With `valid` the session starts.
    deck.set(Reply::Valid);
    let command = start(&st);
    let d = st.coord(command).await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    st.wait_turns(&s, 1).await;
    st.manager.shutdown(StopMode::Kill).await;
}

// MCP P4a §4 (rule 3, W1), test 16: a binding of a worker grant is not a
// coordinator that mutates, so fleet control may target its session; the
// same binding of an owner grant stays refused.
#[tokio::test]
async fn fleet_control_may_target_a_worker_binding() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_turns(&s, 1).await;
    st.switch(true);
    let bindings = st.manager.coordinator_bindings().clone();
    let mut binding = coordinator_binding(&s, &[ToolClass::Mail], BindingState::Active);
    bindings.insert("digest-a".to_owned(), binding.clone());
    let d = st.coord(follow_up(&s, "owner")).await;
    assert_eq!(
        st.refused(&d).await.1,
        "the target session is a coordinator"
    );

    binding.kind = fleetdeck_proto::command::GrantKind::Worker;
    bindings.insert("digest-a".to_owned(), binding);
    let d = st.coord(follow_up(&s, "worker")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    // The binding is still bound: `policy_now` reports it (holds_grant).
    assert!(bindings.holds_grant(&s));
    st.manager.shutdown(StopMode::Kill).await;
}

#[path = "fleet_control_gate_tests.rs"]
mod gate;
#[path = "fleet_control_question_tests.rs"]
mod question;
#[path = "fleet_control_rules_tests.rs"]
mod rules;
