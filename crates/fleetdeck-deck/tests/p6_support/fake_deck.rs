//! A hand-driven deck for the proof of the real `LinkCheck` (proof 9): a
//! listener with the key of the deck that the target station pins, which sends
//! a coordinator `desktop.act` to the real target and holds every
//! `desktop.authority_check` of the station without an answer (or answers it
//! `valid`, on request).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::{Control, Inbound};
use fleetdeck_link::desktop_authority::issue_coordinator;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::desktop::{DesktopAction, Urgency};
use fleetdeck_proto::desktop_authority::with_authority;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::{IncarnationId, SessionId, StationId};
use serde_json::{Value, json};

use super::StationB;

pub struct FakeDeck {
    pub control: Control,
    node_fp: String,
    /// The checks that the station asked, and whether to answer `valid`.
    pub checks: Arc<Mutex<Vec<Value>>>,
    answer: Arc<Mutex<bool>>,
}

impl FakeDeck {
    /// Links `b` to the fake deck of `identity`.
    pub async fn start(b: &StationB, identity: &Arc<Identity>) -> Self {
        // The scopes that the deck gives the station: the ones of a station
        // that a grant names (`coordinate` is the scope of the check).
        let grants = Grants::new();
        grants.grant(
            b.identity.fingerprint(),
            BTreeSet::from([
                Scope::Read,
                Scope::Drive,
                Scope::Approve,
                Scope::Admin,
                Scope::Coordinate,
            ]),
        );
        let node = NodeConfig::new(
            Arc::clone(identity),
            LocalHello::new(Role::Deck, IncarnationId::from("fake-deck-db")),
            Arc::new(Epochs::in_memory()),
        )
        .with_store(grants);
        node.pins.pin(b.identity.public_key(), Role::Station);
        let mut listener = node::listen(node.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        b.link(listener.local_addr, identity.public_key(), &[])
            .await;
        let open = tokio::time::timeout(Duration::from_secs(10), listener.next_link())
            .await
            .expect("a link")
            .expect("a listener");
        let mut live = fleetdeck_link::start(&node, open);
        let control = live.control.clone();
        let checks: Arc<Mutex<Vec<Value>>> = Arc::default();
        let answer = Arc::new(Mutex::new(false));
        let (seen, valid, reply) = (Arc::clone(&checks), Arc::clone(&answer), control.clone());
        tokio::spawn(async move {
            while let Some(message) = live.inbound.recv().await {
                let Inbound::Request(req) = message else {
                    continue;
                };
                if req.method != "desktop.authority_check" {
                    continue;
                }
                seen.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(req.params.clone());
                if *valid.lock().unwrap_or_else(PoisonError::into_inner) {
                    let ok = Outcome::Ok(json!({"valid": true}));
                    let _ = reply.respond(req.id, ok).await;
                }
            }
        });
        Self {
            control,
            node_fp: b.identity.fingerprint().to_string(),
            checks,
            answer,
        }
    }

    /// From now on the fake deck answers every check `valid`.
    pub fn answer_valid(&self, on: bool) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = on;
    }

    /// The `desktop.act` of a notification with the coordinator authority
    /// `action_id`; the answer of the station.
    pub async fn notify(&self, action_id: &str) -> Outcome {
        let action = DesktopAction::Notify {
            title: "swift-fox: hello".to_owned(),
            body: "body".to_owned(),
            urgency: Urgency::Normal,
        };
        let ctx = CoordinatorContext {
            grant_id: "grant-1".to_owned(),
            binding_id: "binding-1".to_owned(),
            generation: 1,
            caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZA"),
            caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZB"),
            caller_label: "swift-fox".to_owned(),
        };
        let epoch = self
            .control
            .params()
            .peer_desktop_epoch
            .clone()
            .expect("the station sent a desktop epoch");
        let now = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let authority = issue_coordinator(
            &action,
            &self.node_fp,
            action_id.to_owned(),
            epoch,
            now,
            ctx,
        );
        let params = with_authority(&action, &authority);
        tokio::time::timeout(
            Duration::from_secs(20),
            self.control.request("desktop.act", None, params),
        )
        .await
        .expect("an answer")
        .expect("the link")
    }
}
