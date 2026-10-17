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
                .starts_with("[FLEETDECK MAIL from fleetdeck]\n<untrusted source=\"conflict:"),
            "{}",
            input.text
        );
        assert!(input.text.contains("\nFleet Deck: "), "{}", input.text);
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

#[tokio::test]
async fn a_replayed_old_touch_is_no_conflict_with_a_recent_one() {
    let det = Det::new();
    let (s, station, inc) = setup(&det, SessionState::Running).await;
    let at = now(&det);
    s.apply_events(
        station.clone(),
        inc.clone(),
        vec![changed(1, "s1", at - 1_000)],
    )
    .await
    .unwrap();
    // A replay of a touch two hours old: outside the window on the other side.
    s.apply_events(
        station.clone(),
        inc.clone(),
        vec![changed(2, "s2", at - 2 * 60 * 60 * 1000)],
    )
    .await
    .unwrap();
    assert!(s.mails(10).await.unwrap().is_empty());
}

fn info_with_model_help(assist: bool) -> fleetdeck_proto::link::StationInfo {
    fleetdeck_proto::link::StationInfo {
        name: "alpha".to_owned(),
        incarnation: "alpha-db".to_owned().into(),
        head: 0,
        live_sessions: 2,
        version: None,
        max_sessions: Some(8),
        allow_shell: None,
        allow_setup_cmd: None,
        providers: Vec::new(),
        assist_tokens_today: assist.then_some(0),
        assist_cap: assist.then_some(200_000),
        resources: None,
        desktop: None,
        traits: None,
        config: None,
        taildrive: None,
        fleet_git: None,
        fleet_summary: None,
    }
}

fn outbox_text(item: &fleetdeck_deck::store::OutboxItem) -> String {
    let kind: fleetdeck_deck::link::CommandKind =
        serde_json::from_value(item.payload.clone()).unwrap();
    let fleetdeck_deck::link::CommandKind::Session {
        command: SessionCommand::FollowUp { input, .. },
    } = kind
    else {
        panic!("a follow-up is expected: {item:?}");
    };
    input.text
}

#[tokio::test]
async fn a_station_without_model_help_gets_the_plain_mail_at_once_and_no_job() {
    let det = Det::new();
    let (s, station, inc) = setup(&det, SessionState::Running).await;
    s.set_station_info(&station, info_with_model_help(false));
    let at = now(&det);
    s.apply_events(
        station.clone(),
        inc,
        vec![changed(1, "s1", at - 2_000), changed(2, "s2", at - 1_000)],
    )
    .await
    .unwrap();
    assert_eq!(s.due_outbox(station).await.unwrap().len(), 2);
    assert!(s.take_conflict_jobs().is_empty());
}

#[tokio::test]
async fn a_station_with_model_help_holds_the_mail_until_its_sentence_is_stored() {
    let det = Det::new();
    let (s, station, inc) = setup(&det, SessionState::Running).await;
    s.set_station_info(&station, info_with_model_help(true));
    let at = now(&det);
    s.apply_events(
        station.clone(),
        inc,
        vec![changed(1, "s1", at - 2_000), changed(2, "s2", at - 1_000)],
    )
    .await
    .unwrap();
    // Both mails exist with the plain text, but neither is due yet.
    assert_eq!(s.mails(10).await.unwrap().len(), 2);
    assert!(s.due_outbox(station.clone()).await.unwrap().is_empty());
    let jobs = s.take_conflict_jobs();
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    assert!(s.take_conflict_jobs().is_empty(), "each job comes out once");
    let first = &jobs[0];
    assert_eq!(first.station, station);
    assert!(
        first.input.starts_with("File: src/lib.rs\n"),
        "{}",
        first.input
    );
    assert!(first.input.contains("<<<UNTRUSTED TEXT"));
    assert!(first.input.contains("Alpha fix") && first.input.contains("Beta fix"));

    // A stored sentence goes after the plain text, in the mail and in the
    // delivery, and the delivery is due.
    s.finish_conflict_sentence(
        first.mail_id.clone(),
        Some("Alpha edited the parser and Beta the printer.".to_owned()),
    )
    .await
    .unwrap();
    let mails = s.mails(10).await.unwrap();
    let with = mails.iter().find(|m| m.mail_id == first.mail_id).unwrap();
    assert!(
        with.text.ends_with(
            "Coordinate before you change it further. Alpha edited the parser and Beta the printer."
        ),
        "{}",
        with.text
    );
    let due = s.due_outbox(station.clone()).await.unwrap();
    assert_eq!(due.len(), 1);
    assert!(is_framed(&outbox_text(&due[0]), &with.text));

    // A failed call releases the plain mail unchanged.
    let second = &jobs[1];
    s.finish_conflict_sentence(second.mail_id.clone(), None)
        .await
        .unwrap();
    let due = s.due_outbox(station).await.unwrap();
    assert_eq!(due.len(), 2);
    let plain = mails.iter().find(|m| m.mail_id == second.mail_id).unwrap();
    assert!(
        plain
            .text
            .ends_with("Coordinate before you change it further.")
    );
    assert!(due.iter().any(|i| is_framed(&outbox_text(i), &plain.text)));
}

/// True when `framed` is the conflict mail of `text`: the header, one
/// untrusted envelope with the source `conflict:<station>`, and the text
/// inside it (ADR-008 section 4.3).
fn is_framed(framed: &str, text: &str) -> bool {
    framed.starts_with("[FLEETDECK MAIL from fleetdeck]\n<untrusted source=\"conflict:")
        && framed.ends_with(&format!("\n{text}\n</untrusted>"))
        && framed.matches("<untrusted").count() == 1
}

#[tokio::test]
async fn a_sentence_for_a_mail_that_was_sent_already_changes_nothing() {
    let det = Det::new();
    let (s, station, inc) = setup(&det, SessionState::Running).await;
    s.set_station_info(&station, info_with_model_help(true));
    let at = now(&det);
    s.apply_events(
        station.clone(),
        inc,
        vec![changed(1, "s1", at - 2_000), changed(2, "s2", at - 1_000)],
    )
    .await
    .unwrap();
    let jobs = s.take_conflict_jobs();
    // The hold ends: the plain mails are due and one goes out.
    det.advance(std::time::Duration::from_millis(
        u64::try_from(fleetdeck_deck::store::conflicts::SENTENCE_HOLD_MS).unwrap() + 1,
    ));
    let due = s.due_outbox(station.clone()).await.unwrap();
    assert_eq!(due.len(), 2);
    let item = &due[0];
    let before = outbox_text(item);
    let mails = s.mails(10).await.unwrap();
    let sent = mails.iter().find(|m| is_framed(&before, &m.text)).unwrap();
    assert!(jobs.iter().any(|j| j.mail_id == sent.mail_id));
    s.start_attempt(
        item.outbox_id.clone(),
        fleetdeck_deck::store::NewCommand {
            command_id: "late-cmd".into(),
            station_id: station,
            method: "session.follow_up".to_owned(),
            origin: "mail".to_owned(),
            envelope: serde_json::json!({ "command_id": "late-cmd" }),
            retry_until: item.expires_at,
            author: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    s.finish_conflict_sentence(sent.mail_id.clone(), Some("Too late.".to_owned()))
        .await
        .unwrap();
    let mails = s.mails(10).await.unwrap();
    assert!(mails.iter().all(|m| !m.text.contains("Too late.")));
    let after = s
        .outbox_item(item.outbox_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outbox_text(&after), before);
}
