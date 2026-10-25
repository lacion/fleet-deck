//! ADR-006 §8.3a 26c–26e (DL-212): `session.coordinator_renew` and
//! `session.coordinator_revoke` at the station.

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::link::Scope;

use super::*;
use crate::coordinator::{Binding, BindingState, ToolClass};
use crate::store::Caller;

impl Station {
    /// A paired deck with `drive` (G5: the grant is read in the decision).
    async fn deck(&self) -> Caller {
        let key = Identity::generate().unwrap().public_key();
        self.manager
            .store()
            .pin_peer(crate::store::peers::PairedKey {
                key,
                role: fleetdeck_link::tls::Role::Deck,
                scopes: [Scope::Read, Scope::Drive].into_iter().collect(),
                dial_url: None,
            })
            .await
            .unwrap();
        Caller::peer(key.fingerprint(), Scope::Drive)
    }

    async fn lifecycle(
        &mut self,
        id: &str,
        origin: CommandOrigin,
        caller: Caller,
        command: SessionCommand,
    ) -> Decided {
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(id),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin,
            command,
        };
        self.manager.submit(envelope, caller).await.unwrap()
    }

    /// The native identity of `session.started` of a session: a binding
    /// with another identity is revoked at once (DL-217, §5.6).
    async fn native(&self, session: &SessionId) -> String {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for e in self.events().await {
                if &e.session_id == session
                    && let EventBody::Known(Event::SessionStarted { provider_refs, .. }) = &e.event
                    && let Some(id) = provider_refs.get("session_id").and_then(|v| v.as_str())
                {
                    return id.to_owned();
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "no session.started");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    async fn bind(&self, session: &SessionId, grant: &str, state: BindingState, generation: u32) {
        let native = self.native(session).await;
        self.manager.coordinator_bindings().insert(
            format!("digest-{grant}"),
            Binding {
                binding_id: format!("b-{grant}"),
                grant_id: grant.to_owned(),
                session_id: session.clone(),
                provider_instance: "claude".to_owned(),
                native_id: Some(native),
                generation,
                classes: [ToolClass::Read].into_iter().collect(),
                state,
                control_skill: String::new(),
                kind: fleetdeck_proto::command::GrantKind::Coordinator,
                scope: fleetdeck_proto::command::GrantScope::Lineage,
            },
        );
    }

    fn binding(&self, grant: &str) -> Binding {
        self.manager
            .coordinator_bindings()
            .get(&format!("b-{grant}"))
            .unwrap()
    }

    async fn lifecycle_events(&self) -> Vec<Event> {
        self.events()
            .await
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(
                    e @ (Event::CoordinatorBound { .. } | Event::CoordinatorEnded { .. }),
                ) => Some(e),
                _ => None,
            })
            .collect()
    }
}

fn revoke(session: &SessionId, grant: &str) -> SessionCommand {
    SessionCommand::CoordinatorRevoke {
        session_id: session.clone(),
        grant_id: grant.to_owned(),
    }
}

fn renew(session: &SessionId, grant: &str, generation: u32) -> SessionCommand {
    SessionCommand::CoordinatorRenew {
        session_id: session.clone(),
        grant_id: grant.to_owned(),
        generation,
    }
}

#[tokio::test]
async fn a_duplicate_renew_or_revoke_command_returns_the_stored_receipt_and_applies_once() {
    let mut st = Station::new(4).await;
    let s = st.start(&st.workspace(), Some("hello")).await;
    let deck = st.deck().await;
    st.bind(&s, "g-1", BindingState::Expired, 1).await;
    let first = st
        .lifecycle(
            "renew-1",
            CommandOrigin::Board,
            deck.clone(),
            renew(&s, "g-1", 2),
        )
        .await;
    assert_eq!(
        first.receipt.state,
        ReceiptState::Accepted,
        "{:?}",
        first.receipt
    );
    let again = st
        .lifecycle(
            "renew-1",
            CommandOrigin::Board,
            deck.clone(),
            renew(&s, "g-1", 2),
        )
        .await;
    assert_eq!(again.receipt, first.receipt);
    assert_eq!(st.binding("g-1").state, BindingState::Active);
    assert_eq!(st.binding("g-1").generation, 2);
    // A renewal of an active binding (or at a wrong generation) is refused.
    let r = st
        .lifecycle(
            "renew-2",
            CommandOrigin::Board,
            deck.clone(),
            renew(&s, "g-1", 3),
        )
        .await;
    assert_eq!(r.receipt.reject.unwrap().code, RejectCode::InvalidState);

    let first = st
        .lifecycle(
            "revoke-1",
            CommandOrigin::Board,
            deck.clone(),
            revoke(&s, "g-1"),
        )
        .await;
    assert_eq!(first.receipt.state, ReceiptState::Accepted);
    let again = st
        .lifecycle(
            "revoke-1",
            CommandOrigin::Board,
            deck.clone(),
            revoke(&s, "g-1"),
        )
        .await;
    assert_eq!(again.receipt, first.receipt);
    assert_eq!(st.binding("g-1").state, BindingState::Revoked);
    // A revoked binding never renews.
    let r = st
        .lifecycle(
            "renew-3",
            CommandOrigin::Board,
            deck.clone(),
            renew(&s, "g-1", 3),
        )
        .await;
    assert_eq!(r.receipt.reject.unwrap().code, RejectCode::InvalidState);
    // Exactly one bound (generation 2) and one ended (revoked) event.
    let events = st.lifecycle_events().await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(
        &events[0],
        Event::CoordinatorBound { generation: 2, .. }
    ));
    assert!(
        matches!(&events[1], Event::CoordinatorEnded { reason, generation: 2, .. } if reason == "revoked")
    );
}

#[tokio::test]
async fn a_renew_or_revoke_whose_grant_belongs_to_another_session_is_a_stored_rejection() {
    let mut st = Station::new(4).await;
    let a = st.start(&st.workspace(), Some("hello")).await;
    let b = st.start(&st.workspace(), Some("hello")).await;
    let deck = st.deck().await;
    st.bind(&a, "g-a", BindingState::Expired, 1).await;
    for (n, command) in [revoke(&b, "g-a"), renew(&b, "g-a", 2)]
        .into_iter()
        .enumerate()
    {
        let id = format!("other-{n}");
        let d = st
            .lifecycle(&id, CommandOrigin::Board, deck.clone(), command.clone())
            .await;
        assert_eq!(
            d.receipt.reject.as_ref().unwrap().code,
            RejectCode::InvalidCommand
        );
        // Stored: the same id returns the same rejection.
        let again = st
            .lifecycle(&id, CommandOrigin::Board, deck.clone(), command)
            .await;
        assert_eq!(again.receipt, d.receipt);
    }
    assert_eq!(st.binding("g-a").state, BindingState::Expired);
    assert!(st.lifecycle_events().await.is_empty());
}

#[tokio::test]
async fn a_station_local_request_or_a_coordinator_operation_cannot_create_renew_or_revoke() {
    let mut st = Station::new(4).await;
    let s = st.start(&st.workspace(), Some("hello")).await;
    let deck = st.deck().await;
    st.bind(&s, "g-1", BindingState::Expired, 1).await;
    let mut n = 0;
    // The local socket, whatever origin it names.
    for origin in [
        CommandOrigin::Board,
        CommandOrigin::Local,
        CommandOrigin::Coordinator,
    ] {
        for command in [revoke(&s, "g-1"), renew(&s, "g-1", 2)] {
            n += 1;
            let d = st
                .lifecycle(&format!("local-{n}"), origin, Caller::local(), command)
                .await;
            assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
        }
    }
    // A paired deck, but a coordinator or mail origin.
    for origin in [CommandOrigin::Coordinator, CommandOrigin::Mail] {
        for command in [revoke(&s, "g-1"), renew(&s, "g-1", 2)] {
            n += 1;
            let d = st
                .lifecycle(&format!("peer-{n}"), origin, deck.clone(), command)
                .await;
            assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
        }
    }
    assert_eq!(st.binding("g-1").state, BindingState::Expired);
    assert!(st.lifecycle_events().await.is_empty());
}

/// MCP P4b-2b: a deck link without `coordinator_live.v1` refuses the set.
#[tokio::test]
async fn a_coordinator_set_from_a_deck_without_the_feature_is_refused_and_a_local_one_as_forbidden()
{
    let mut st = Station::new(4).await;
    let s = st.start(&st.workspace(), Some("hello")).await;
    let deck = st.deck().await;
    let spec: fleetdeck_proto::command::CoordinatorSpec =
        serde_json::from_value(serde_json::json!({
            "grant_id": "g-1", "classes": ["observe"], "idle_ttl_ms": 60_000,
            "expires_at": 4_102_444_800_000_i64,
            "control_skill": {"version": "1", "text": "You coordinate the fleet."}
        }))
        .unwrap();
    for (n, spec) in [Some(Box::new(spec)), None].into_iter().enumerate() {
        let command = SessionCommand::CoordinatorSet {
            session_id: s.clone(),
            seq: 1,
            grant_id: "g-1".to_owned(),
            spec,
        };
        let d = st
            .lifecycle(
                &format!("set-{n}"),
                CommandOrigin::Board,
                deck.clone(),
                command,
            )
            .await;
        let reject = d.receipt.reject.unwrap();
        assert_eq!(reject.code, RejectCode::InvalidState);
        assert_eq!(reject.message, "the deck link has no coordinator_live.v1");
    }
    // The local socket never sets a grant.
    let local = SessionCommand::CoordinatorSet {
        session_id: s.clone(),
        seq: 1,
        grant_id: "g-1".to_owned(),
        spec: None,
    };
    let d = st
        .lifecycle("set-local", CommandOrigin::Board, Caller::local(), local)
        .await;
    assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
}
