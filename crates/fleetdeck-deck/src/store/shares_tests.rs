#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::share::{ShareKind, ShareRole, Steering};
use fleetdeck_testkit::Det;

use super::NewShareRow;
use crate::store::DeckStore;

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn share(kind: ShareKind, target: &str, steering: Steering) -> NewShareRow {
    NewShareRow {
        kind,
        target: target.to_owned(),
        role: ShareRole::Collaborator,
        steering,
        expires_in_days: None,
        uses: None,
        history: false,
    }
}

#[tokio::test]
async fn a_session_is_in_mob_mode_when_an_active_share_that_covers_it_is_mob() {
    let det = Det::new();
    let st = store(&det).await;
    let is_mob = |s: &str| {
        let st = st.clone();
        let s = s.to_owned();
        async move { st.session_is_mob("st".to_owned(), s).await.unwrap() }
    };
    assert!(!is_mob("s1").await);
    st.create_share(share(ShareKind::Session, "st/s1", Steering::Open))
        .await
        .unwrap();
    assert!(!is_mob("s1").await);
    let mob = st
        .create_share(share(ShareKind::Session, "st/s1", Steering::Mob))
        .await
        .unwrap();
    assert!(is_mob("s1").await);
    assert!(!is_mob("s2").await);
    // A space share is mob for the sessions of its space.
    let space = st.create_space("pair").await.unwrap();
    st.create_share(share(ShareKind::Space, &space.space_id, Steering::Mob))
        .await
        .unwrap();
    assert!(!is_mob("s2").await);
    st.add_space_session(space.space_id, "st".into(), "s2".into())
        .await
        .unwrap();
    assert!(is_mob("s2").await);
    // A board share covers every session.
    let board = st
        .create_share(share(ShareKind::Board, "", Steering::Mob))
        .await
        .unwrap();
    assert!(is_mob("s9").await);
    // A revoked share is no more.
    st.revoke_share(board.share_id).await.unwrap();
    assert!(!is_mob("s9").await);
    st.revoke_share(mob.share_id).await.unwrap();
    // The first open share stays; a revoked mob share does not count.
    assert!(!is_mob("s1").await);
}

#[tokio::test]
async fn the_first_request_drives_and_the_others_wait_in_order() {
    let det = Det::new();
    let st = store(&det).await;
    let ask = |who: &str| {
        let st = st.clone();
        let who = who.to_owned();
        async move {
            st.drive_request("st".to_owned(), "s1".to_owned(), who)
                .await
                .unwrap()
        }
    };
    let a = ask("member:Ann").await;
    assert_eq!(a.driver.as_deref(), Some("member:Ann"));
    assert!(a.requests.is_empty());
    let b = ask("member:Bob").await;
    assert_eq!(b.driver.as_deref(), Some("member:Ann"));
    assert_eq!(b.requests, ["member:Bob"]);
    // A repeat changes nothing; the driver asking is a no-op.
    det.advance(std::time::Duration::from_millis(5));
    let again = ask("member:Bob").await;
    assert_eq!(again.requests, ["member:Bob"]);
    assert_eq!(ask("member:Ann").await.requests, ["member:Bob"]);
    ask("member:Cy").await;
    let state = st
        .driver_state("st".to_owned(), "s1".to_owned())
        .await
        .unwrap();
    assert_eq!(state.requests, ["member:Bob", "member:Cy"]);
    // The grant hands the composer on and ends the request of the new driver.
    let state = st
        .drive_grant("st".to_owned(), "s1".to_owned(), "member:Bob".to_owned())
        .await
        .unwrap();
    assert_eq!(state.driver.as_deref(), Some("member:Bob"));
    assert_eq!(state.requests, ["member:Cy"]);
    // A release leaves nobody driving; the next request drives at once.
    let state = st
        .drive_release("st".to_owned(), "s1".to_owned())
        .await
        .unwrap();
    assert_eq!(state.driver, None);
    assert_eq!(ask("member:Cy").await.driver.as_deref(), Some("member:Cy"));
    // Another session has its own driver.
    let other = st
        .driver_state("st".to_owned(), "s2".to_owned())
        .await
        .unwrap();
    assert_eq!(other.driver, None);
}

#[tokio::test]
async fn a_request_kind_is_read_from_the_stored_timeline() {
    use fleetdeck_proto::event::{ApprovalOption, Event, EventBody, RequestKind, ResponseChannel};
    use fleetdeck_proto::{EventEnvelope, SessionSummary};

    let det = Det::new();
    let st = store(&det).await;
    // The timeline needs a station row; pin one the way the other tests do.
    let key = fleetdeck_core::identity::Identity::generate()
        .unwrap()
        .public_key();
    let rec = st
        .pin_station(crate::store::stations::NewStation {
            key,
            name: "fake".to_owned(),
            scopes: std::collections::BTreeSet::new(),
            dial_url: None,
        })
        .await
        .unwrap();
    let inc = fleetdeck_proto::IncarnationId::from("inc");
    st.apply_snapshot(
        rec.station_id.clone(),
        inc.clone(),
        0,
        vec![SessionSummary::new("s1".into())],
    )
    .await
    .unwrap();
    let ask = |seq: u64, id: &str, kind: RequestKind| EventEnvelope {
        station_seq: seq,
        session_id: "s1".into(),
        session_seq: seq,
        at: 1,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::ApprovalRequested {
            request_id: id.into(),
            node_id: None,
            request_kind: kind,
            title: "t".to_owned(),
            details: String::new(),
            input: None,
            options: vec![ApprovalOption {
                option_id: "allow".to_owned(),
                label: "Allow".to_owned(),
                kind: None,
            }],
            response: ResponseChannel::RpcReply,
            provider_refs: serde_json::Map::new(),
        }),
    };
    st.apply_events(
        rec.station_id.clone(),
        inc,
        vec![
            ask(1, "q1", RequestKind::Question),
            ask(2, "t1", RequestKind::Tool),
        ],
    )
    .await
    .unwrap();
    let kind = |id: &str| {
        let (st, s, id) = (st.clone(), rec.station_id.0.clone(), id.to_owned());
        async move { st.request_kind(s, "s1".to_owned(), id).await.unwrap() }
    };
    assert_eq!(kind("q1").await, Some(RequestKind::Question));
    assert_eq!(kind("t1").await, Some(RequestKind::Tool));
    assert_eq!(kind("nope").await, None);
}

#[tokio::test]
async fn set_share_history_changes_the_view_and_the_grant() {
    let det = Det::new();
    let st = store(&det).await;
    let created = st
        .create_share(share(ShareKind::Board, "", Steering::Open))
        .await
        .unwrap();
    let login = st
        .join(
            &fleetdeck_core::Secret::new(created.token.expose().clone()),
            "Ann",
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!st.shares().await.unwrap()[0].history);
    assert!(!login.grant.history);

    let view = st
        .set_share_history(created.share_id.clone(), true)
        .await
        .unwrap()
        .unwrap();
    assert!(view.history);
    assert!(st.shares().await.unwrap()[0].history);
    let grant = st.member_login(&login.cookie).await.unwrap().unwrap();
    assert!(grant.history);

    let view = st
        .set_share_history(created.share_id.clone(), false)
        .await
        .unwrap()
        .unwrap();
    assert!(!view.history);
    let grant = st.member_login(&login.cookie).await.unwrap().unwrap();
    assert!(!grant.history);

    assert!(
        st.set_share_history("nope".to_owned(), true)
            .await
            .unwrap()
            .is_none()
    );
    // A share made with the option reads it.
    let mut on = share(ShareKind::Board, "", Steering::Open);
    on.history = true;
    let on = st.create_share(on).await.unwrap();
    let shares = st.shares().await.unwrap();
    let found = shares.iter().find(|s| s.share_id == on.share_id).unwrap();
    assert!(found.history);
}
