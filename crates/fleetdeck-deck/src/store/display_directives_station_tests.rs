//! Tests of the station sender of a directive (R13 P4 task E, design
//! `omarchy-p4.md` §3.6 and the exposure proof of §4).

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::display_directive::{DirectiveBody, SessionRef};
use fleetdeck_proto::event::{RequestKind, ResponseChannel};
use fleetdeck_proto::session::OpenRequest;
use fleetdeck_proto::{IncarnationId, SessionSummary, StationId};
use fleetdeck_testkit::Det;

use super::StationDirectiveError;
use crate::link::display_directive::DisplayIntent;
use crate::store::display_directives::DirectiveSender;
use crate::store::displays::NewDisplay;
use crate::store::stations::NewStation;
use crate::store::{DeckStore, call};

fn card(id: &str, requests: &[(&str, i64)]) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.open_requests = requests
        .iter()
        .map(|(request_id, at)| OpenRequest {
            request_id: (*request_id).into(),
            node_id: None,
            request_kind: RequestKind::Command,
            title: "a title".to_owned(),
            details: "details".to_owned(),
            input: None,
            options: vec![],
            response: ResponseChannel::RpcReply,
            requested_at: *at,
        })
        .collect();
    s
}

struct Rig {
    s: DeckStore,
    a: StationId,
    b: StationId,
}

async fn station(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([fleetdeck_proto::link::Scope::Admin]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

/// Two stations without any session.
async fn rig() -> Rig {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let a = station(&s, "alpha").await;
    let b = station(&s, "beta").await;
    Rig { s, a, b }
}

impl Rig {
    async fn sessions(&self, station: &StationId, cards: Vec<SessionSummary>) {
        self.s
            .apply_snapshot(station.clone(), IncarnationId::from("i"), 0, cards)
            .await
            .unwrap();
    }

    /// A display with this ceiling, bound to `bound`.
    async fn display(&self, ceiling: &[&StationId], bound: &StationId) -> String {
        let id = self
            .s
            .create_display(NewDisplay {
                name: "Wall".to_owned(),
                ceiling: ceiling.iter().map(|s| (*s).clone()).collect(),
            })
            .await
            .unwrap()
            .display_id;
        self.bind(&id, bound).await;
        id
    }

    async fn bind(&self, display: &str, station: &StationId) {
        self.s
            .display_set_binding(
                display.to_owned(),
                Some(DisplayBinding {
                    station_id: station.clone(),
                    monitor: "DP-1".to_owned(),
                }),
            )
            .await
            .unwrap();
    }

    async fn owner(&self, display: &str, body: DirectiveBody) {
        self.s
            .display_directive_set(display.to_owned(), DirectiveSender::Owner, body)
            .await
            .unwrap();
    }

    async fn send(
        &self,
        station: &StationId,
        display: &str,
        intent: DisplayIntent,
    ) -> Result<DirectiveBody, StationDirectiveError> {
        self.s
            .display_directive_station(station.clone(), display.to_owned(), intent)
            .await
            .map(|v| v.directive)
    }

    async fn read(&self, display: &str) -> DirectiveBody {
        self.s
            .display_directive(display.to_owned())
            .await
            .unwrap()
            .unwrap()
            .directive
    }

    async fn sender(&self, display: &str) -> (String, String) {
        let display = display.to_owned();
        call(&self.s.inner.db, move |conn| {
            Ok(conn.query_row(
                "SELECT sender_kind, sender_id FROM display_directives WHERE display_id = ?1",
                [display],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
    }
}

fn focus(station: &StationId, session: &str) -> DirectiveBody {
    DirectiveBody::Focus {
        session: SessionRef {
            station_id: station.clone(),
            session_id: session.into(),
        },
        pin: false,
        filter: None,
    }
}

fn ambient() -> DirectiveBody {
    DirectiveBody::Ambient {
        pin: false,
        filter: None,
    }
}

fn mission() -> DirectiveBody {
    DirectiveBody::mission()
}

const NEXT: DisplayIntent = DisplayIntent::NextNeedsYou;
const MODE: DisplayIntent = DisplayIntent::NextMode;

#[tokio::test]
async fn a_bound_station_sends_both_intents_and_the_row_names_the_station() {
    let r = rig().await;
    r.sessions(&r.a, vec![card("s1", &[("q1", 100)])]).await;
    let d = r.display(&[&r.a], &r.a).await;
    assert_eq!(r.send(&r.a, &d, NEXT).await.unwrap(), focus(&r.a, "s1"));
    // The wall reads what the station sent (not `mission`).
    assert_eq!(r.read(&d).await, focus(&r.a, "s1"));
    assert_eq!(r.sender(&d).await, ("station".to_owned(), r.a.to_string()));
    // `focus` goes to `ambient`, then `mission`, then `focus` again.
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), ambient());
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), mission());
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), focus(&r.a, "s1"));
}

#[tokio::test]
async fn the_mode_cycle_skips_focus_without_a_request() {
    let r = rig().await;
    r.sessions(&r.a, vec![card("s1", &[])]).await;
    let d = r.display(&[&r.a], &r.a).await;
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), ambient());
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), mission());
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), ambient());
}

#[tokio::test]
async fn a_mode_outside_the_cycle_goes_to_mission() {
    let r = rig().await;
    r.sessions(&r.a, vec![card("s1", &[("q1", 100)])]).await;
    let d = r.display(&[&r.a], &r.a).await;
    let review = DirectiveBody::Review {
        session: SessionRef {
            station_id: r.a.clone(),
            session_id: "s1".into(),
        },
        pin: true,
        filter: None,
    };
    r.owner(&d, review).await;
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), mission());
}

#[tokio::test]
async fn next_needs_you_walks_the_requests_oldest_first_and_wraps_around() {
    let r = rig().await;
    r.sessions(
        &r.a,
        vec![
            card("s3", &[("q3", 300)]),
            card("s1", &[("q1", 100)]),
            card("s2", &[("q2", 200)]),
        ],
    )
    .await;
    let d = r.display(&[&r.a], &r.a).await;
    for expected in ["s1", "s2", "s3", "s1", "s2"] {
        assert_eq!(r.send(&r.a, &d, NEXT).await.unwrap(), focus(&r.a, expected));
    }
}

#[tokio::test]
async fn equal_times_are_ordered_by_session_id() {
    let r = rig().await;
    r.sessions(
        &r.a,
        vec![card("sb", &[("q2", 100)]), card("sa", &[("q1", 100)])],
    )
    .await;
    let d = r.display(&[&r.a], &r.a).await;
    for expected in ["sa", "sb", "sa"] {
        assert_eq!(r.send(&r.a, &d, NEXT).await.unwrap(), focus(&r.a, expected));
    }
}

#[tokio::test]
async fn with_a_focus_that_has_no_request_the_target_is_the_oldest() {
    let r = rig().await;
    r.sessions(
        &r.a,
        vec![
            card("s1", &[("q1", 100)]),
            card("s2", &[("q2", 200)]),
            card("idle", &[]),
        ],
    )
    .await;
    let d = r.display(&[&r.a], &r.a).await;
    r.owner(&d, focus(&r.a, "idle")).await;
    assert_eq!(r.send(&r.a, &d, NEXT).await.unwrap(), focus(&r.a, "s1"));
}

#[tokio::test]
async fn nothing_to_show_without_an_open_request() {
    let r = rig().await;
    r.sessions(&r.a, vec![card("s1", &[])]).await;
    let d = r.display(&[&r.a], &r.a).await;
    let out = r.send(&r.a, &d, NEXT).await;
    assert!(
        matches!(out, Err(StationDirectiveError::NothingToShow)),
        "{out:?}"
    );
    // Nothing was stored.
    assert_eq!(r.read(&d).await, mission());
}

#[tokio::test]
async fn an_unbound_station_and_an_unknown_display_get_the_same_answer() {
    let r = rig().await;
    r.sessions(&r.a, vec![card("s1", &[("q1", 100)])]).await;
    r.sessions(&r.b, vec![card("t1", &[("q9", 50)])]).await;
    let d = r.display(&[&r.a, &r.b], &r.a).await;
    let unbound = r.send(&r.b, &d, NEXT).await;
    let unknown = r.send(&r.a, "no-such-display", NEXT).await;
    for out in [unbound, unknown] {
        assert!(
            matches!(out, Err(StationDirectiveError::NotBound)),
            "{out:?}"
        );
    }
    // A display with no binding at all.
    let free =
        r.s.create_display(NewDisplay {
            name: "Free".to_owned(),
            ceiling: vec![r.a.clone()],
        })
        .await
        .unwrap()
        .display_id;
    let out = r.send(&r.a, &free, MODE).await;
    assert!(
        matches!(out, Err(StationDirectiveError::NotBound)),
        "{out:?}"
    );
    // The refused calls stored nothing.
    assert_eq!(r.read(&d).await, mission());
}

#[tokio::test]
async fn a_station_never_reaches_a_session_of_another_station() {
    let r = rig().await;
    // Both stations have open requests; the one of b is the oldest.
    r.sessions(&r.a, vec![card("sa", &[("qa", 200)])]).await;
    r.sessions(&r.b, vec![card("sb", &[("qb", 100)])]).await;
    let d = r.display(&[&r.a, &r.b], &r.a).await;
    assert_eq!(r.send(&r.a, &d, NEXT).await.unwrap(), focus(&r.a, "sa"));
    assert_eq!(r.send(&r.a, &d, NEXT).await.unwrap(), focus(&r.a, "sa"));
    // With no request of its own, a gets nothing, though b has one.
    r.sessions(&r.a, vec![card("sa", &[])]).await;
    let out = r.send(&r.a, &d, NEXT).await;
    assert!(
        matches!(out, Err(StationDirectiveError::NothingToShow)),
        "{out:?}"
    );
    // The mode cycle only focuses a session of a.
    r.owner(&d, mission()).await;
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), ambient());
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), mission());
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), ambient());
}

#[tokio::test]
async fn a_focus_on_a_session_of_another_station_counts_as_no_focus() {
    let r = rig().await;
    r.sessions(
        &r.a,
        vec![card("s1", &[("q1", 100)]), card("s2", &[("q2", 200)])],
    )
    .await;
    r.sessions(&r.b, vec![card("s2", &[("q9", 50)])]).await;
    let d = r.display(&[&r.a, &r.b], &r.a).await;
    r.owner(&d, focus(&r.b, "s2")).await;
    // The session id `s2` exists on both stations; only the station of the
    // caller counts, so the target is the oldest request of a.
    assert_eq!(r.send(&r.a, &d, NEXT).await.unwrap(), focus(&r.a, "s1"));
}

#[tokio::test]
async fn a_station_outside_the_ceiling_of_its_display_has_nothing_to_show() {
    let r = rig().await;
    r.sessions(&r.a, vec![card("s1", &[("q1", 100)])]).await;
    r.sessions(&r.b, vec![card("t1", &[("q2", 50)])]).await;
    // The display is bound to a, but its ceiling holds only b.
    let d = r.display(&[&r.b], &r.a).await;
    let out = r.send(&r.a, &d, NEXT).await;
    assert!(
        matches!(out, Err(StationDirectiveError::NothingToShow)),
        "{out:?}"
    );
    // The mode cycle never reaches `focus` either.
    assert_eq!(r.send(&r.a, &d, MODE).await.unwrap(), ambient());
}

#[tokio::test]
async fn a_revoked_display_is_not_bound() {
    let r = rig().await;
    r.sessions(&r.a, vec![card("s1", &[("q1", 100)])]).await;
    let d = r.display(&[&r.a], &r.a).await;
    assert!(r.s.display_revoke(d.clone()).await.unwrap());
    let out = r.send(&r.a, &d, NEXT).await;
    assert!(
        matches!(out, Err(StationDirectiveError::NotBound)),
        "{out:?}"
    );
}

#[tokio::test]
async fn a_rebinding_ends_the_authority_of_the_old_station() {
    let r = rig().await;
    r.sessions(&r.a, vec![card("s1", &[("q1", 100)])]).await;
    r.sessions(&r.b, vec![card("t1", &[("q2", 50)])]).await;
    let d = r.display(&[&r.a, &r.b], &r.a).await;
    r.bind(&d, &r.b).await;
    let out = r.send(&r.a, &d, NEXT).await;
    assert!(
        matches!(out, Err(StationDirectiveError::NotBound)),
        "{out:?}"
    );
    assert_eq!(r.send(&r.b, &d, NEXT).await.unwrap(), focus(&r.b, "t1"));
}
