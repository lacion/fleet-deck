//! The conflict mail (DL-145): when two sessions change the same file, the
//! deck mails each of them once, with a stable mail id; an ended session
//! gets none; `conflict_mail = false` turns it off.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_deck::DeckStore;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::event::{Event, EventBody, FileChange, SessionState};
use fleetdeck_proto::orchestration::{MailDelivery, MailView};
use fleetdeck_proto::session::RepoInfo;
use fleetdeck_proto::{EventEnvelope, IncarnationId, SessionSummary, StationId};
use fleetdeck_testkit::Det;
use ring::digest::{SHA256, digest};

const GROUP: &str = "origin:github.com/o/app";
const FILE: &str = "src/lib.rs";

fn session(id: &str, title: &str, state: SessionState) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.title = Some(title.to_owned());
    s.state = state;
    s.cwd = Some("/home/x/app".to_owned());
    s.repo = Some(RepoInfo {
        root: "/home/x/app".to_owned(),
        origin: Some("github.com/o/app".to_owned()),
    });
    s
}

fn changed(seq: u64, session: &str, at: i64) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::FileChanged {
            path: "/home/x/app/src/lib.rs".to_owned(),
            change: FileChange::Modified,
            node_id: None,
        }),
    }
}

async fn pin(store: &DeckStore) -> StationId {
    store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "alpha".to_owned(),
            scopes: BTreeSet::new(),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id
}

/// A store with the station `alpha` and the sessions `s1` ("Alpha fix") and
/// `s2` ("Beta fix"); `s2` has the state `s2_state`.
async fn setup(det: &Det, s2_state: SessionState) -> (DeckStore, StationId, IncarnationId) {
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let station = pin(&store).await;
    let inc = IncarnationId::from("alpha-db");
    store
        .apply_snapshot(
            station.clone(),
            inc.clone(),
            0,
            vec![
                session("s1", "Alpha fix", SessionState::Running),
                session("s2", "Beta fix", s2_state),
            ],
        )
        .await
        .unwrap();
    (store, station, inc)
}

fn expected_id(station: &StationId, recipient: &str) -> String {
    let (a, b) = (format!("{station}/s1"), format!("{station}/s2"));
    let recipient = format!("{station}/{recipient}");
    let text = format!("{GROUP}|{FILE}|{a}|{b}|{recipient}");
    let hex: String = digest(&SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("conflict-{}", &hex[..24])
}

fn mail_to<'a>(mails: &'a [MailView], session: &str) -> &'a MailView {
    mails
        .iter()
        .find(|m| m.session_id.as_str() == session)
        .unwrap()
}

fn now(det: &Det) -> i64 {
    i64::try_from(det.now_ms()).unwrap()
}

#[tokio::test]
async fn two_sessions_that_change_one_file_get_one_mail_each() {
    let det = Det::new();
    let (s, station, inc) = setup(&det, SessionState::Running).await;
    let at = now(&det);
    s.apply_events(
        station.clone(),
        inc.clone(),
        vec![changed(1, "s1", at - 2_000)],
    )
    .await
    .unwrap();
    assert!(
        s.mails(10).await.unwrap().is_empty(),
        "one session: no mail"
    );
    s.apply_events(
        station.clone(),
        inc.clone(),
        vec![changed(2, "s2", at - 1_000)],
    )
    .await
    .unwrap();
    let mails = s.mails(10).await.unwrap();
    assert_eq!(mails.len(), 2, "{mails:?}");
    let to_s1 = mail_to(&mails, "s1");
    let to_s2 = mail_to(&mails, "s2");
    assert_eq!(to_s1.mail_id, expected_id(&station, "s1"));
    assert_eq!(to_s2.mail_id, expected_id(&station, "s2"));
    assert_ne!(to_s1.mail_id, to_s2.mail_id);
    assert_eq!(
        to_s1.text,
        "Fleet Deck: the session \"Beta fix\" on alpha also changed src/lib.rs in the last 30 \
         minutes. Coordinate before you change it further."
    );
    assert_eq!(
        to_s2.text,
        "Fleet Deck: the session \"Alpha fix\" on alpha also changed src/lib.rs in the last 30 \
         minutes. Coordinate before you change it further."
    );
    for m in &mails {
        assert_eq!(m.delivery, MailDelivery::FollowUp);
        assert_eq!(m.sender, "fleetdeck");
        assert_eq!(m.station_id, station);
    }
    // The outbox item delivers the framed text as a follow-up.
    let items = s.due_outbox(station.clone()).await.unwrap();
    assert_eq!(items.len(), 2);
    for item in &items {
        assert_eq!(item.origin, "mail");
        let kind: fleetdeck_deck::link::CommandKind =
            serde_json::from_value(item.payload.clone()).unwrap();
        let fleetdeck_deck::link::CommandKind::Session {
            command: SessionCommand::FollowUp { input, .. },
        } = kind
        else {
            panic!("a follow-up is expected: {item:?}");
        };
        assert!(
            input
                .text
                .starts_with("[FLEETDECK MAIL from fleetdeck] Fleet Deck: ")
        );
    }

    // A third touch by either session mails nobody again.
    s.apply_events(station.clone(), inc.clone(), vec![changed(3, "s1", at)])
        .await
        .unwrap();
    s.apply_events(station.clone(), inc, vec![changed(4, "s2", at + 1_000)])
        .await
        .unwrap();
    assert_eq!(s.mails(10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn an_ended_session_gets_no_mail_and_the_live_one_still_does() {
    let det = Det::new();
    let (s, station, inc) = setup(&det, SessionState::Exited).await;
    let at = now(&det);
    s.apply_events(
        station.clone(),
        inc,
        vec![changed(1, "s1", at - 2_000), changed(2, "s2", at - 1_000)],
    )
    .await
    .unwrap();
    let mails = s.mails(10).await.unwrap();
    assert_eq!(mails.len(), 1, "{mails:?}");
    assert_eq!(mails[0].session_id.as_str(), "s1");
    assert_eq!(mails[0].mail_id, expected_id(&station, "s1"));
}

#[tokio::test]
async fn conflict_mail_false_mails_nobody() {
    let det = Det::new();
    let (s, station, inc) = setup(&det, SessionState::Running).await;
    s.set_conflict_mail(false);
    let at = now(&det);
    s.apply_events(
        station,
        inc,
        vec![changed(1, "s1", at - 2_000), changed(2, "s2", at - 1_000)],
    )
    .await
    .unwrap();
    assert!(s.mails(10).await.unwrap().is_empty());
    assert_eq!(
        s.conflicts().await.unwrap().len(),
        1,
        "the radar still sees it"
    );
}

#[tokio::test]
async fn a_restart_does_not_warn_again() {
    let det = Det::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let inc = IncarnationId::from("alpha-db");
    let at = now(&det);
    let station = {
        let s = DeckStore::open(&path, det.clock(), det.ids())
            .await
            .unwrap();
        let station = pin(&s).await;
        s.apply_snapshot(
            station.clone(),
            inc.clone(),
            0,
            vec![
                session("s1", "Alpha fix", SessionState::Running),
                session("s2", "Beta fix", SessionState::Running),
            ],
        )
        .await
        .unwrap();
        s.apply_events(
            station.clone(),
            inc.clone(),
            vec![changed(1, "s1", at - 2_000), changed(2, "s2", at - 1_000)],
        )
        .await
        .unwrap();
        assert_eq!(s.mails(10).await.unwrap().len(), 2);
        station
    };
    let s = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    s.apply_events(station, inc, vec![changed(3, "s1", at)])
        .await
        .unwrap();
    assert_eq!(s.mails(10).await.unwrap().len(), 2);
}
