#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::OnceLock;

use fleetdeck_testkit::Det;

use super::*;

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

#[tokio::test]
async fn fence_audit_rows_name_the_kind_and_a_hash_prefix_and_never_an_address() {
    let det = Det::new();
    let st = store(&det).await;
    let rows = vec![
        SignedOut {
            kind: SessionKind::Owner,
            session_hash: "0123456789abcdef".to_owned(),
        },
        SignedOut {
            kind: SessionKind::Member,
            session_hash: "fedcba9876543210".to_owned(),
        },
    ];
    audit_signed_out(&st, &rows, "op-7").await;
    let log = st.audit_log(10).await.unwrap();
    let mut got: Vec<_> = log
        .iter()
        .filter(|e| e.action == "board.signed_out_for_input")
        .map(|e| {
            (
                e.actor.clone(),
                e.target.clone().unwrap(),
                e.detail.clone().unwrap(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            (
                "deck".to_owned(),
                "member:fedcba98".to_owned(),
                "rule=1 op=op-7".to_owned()
            ),
            (
                "deck".to_owned(),
                "owner:01234567".to_owned(),
                "rule=1 op=op-7".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn fence_revoke_before_dispatch_waits_for_a_live_relay() {
    let det = Det::new();
    let st = store(&det).await;
    let reg = st.registry();
    let t = reg.register("h1", SessionKind::Owner, SocketKind::Shell, "10.0.0.1");
    assert!(reg.go_live(&t, || true));
    let rows = vec![SignedOut {
        kind: SessionKind::Owner,
        session_hash: "h1".to_owned(),
    }];
    // The relay never ends: false after the wait.
    let stuck = revoke_before_dispatch(&st, &rows, &[]).await;
    assert!(!stuck);
    assert!(t.is_revoked());
    // A socket from a snapshot address is revoked too.
    let u = reg.register("h2", SessionKind::Member, SocketKind::Board, "10.0.0.5");
    assert!(reg.go_live(&u, || true));
    let relay = {
        let (reg, u) = (std::sync::Arc::clone(reg), u.clone());
        tokio::spawn(async move {
            u.revoked().await;
            reg.remove(&u);
        })
    };
    reg.remove(&t);
    assert!(revoke_before_dispatch(&st, &[], &["10.0.0.5".to_owned()]).await);
    relay.await.unwrap();
}

#[tokio::test]
async fn fence_the_local_station_is_shared_with_the_cell_and_the_first_value_stays() {
    let det = Det::new();
    let st = store(&det).await;
    assert_eq!(st.local_station(), None);
    st.set_local_station(StationId::from("st1"));
    st.set_local_station(StationId::from("st2"));
    assert_eq!(st.local_station(), Some(StationId::from("st1")));

    let st = store(&det).await;
    let cell = std::sync::Arc::new(OnceLock::new());
    st.share_local_station(std::sync::Arc::clone(&cell));
    assert_eq!(st.local_station(), None);
    // `fleetdeck up` sets the cell after the pairing; the store sees it.
    cell.set(StationId::from("st3")).unwrap();
    assert_eq!(st.local_station(), Some(StationId::from("st3")));
}

#[test]
fn fence_the_startup_order_moves_the_recovery_before_the_link_node_and_the_board() {
    let src = include_str!("../serve.rs");
    let at = |needle: &str| src.find(needle).unwrap_or_else(|| panic!("{needle}"));
    let open = at("DeckStore::open(");
    let captures = at("captures_prepared_to_unknown");
    let recover = at("desktop_ops_recover");
    let node = at("DeckNode::start_with(");
    let board = at("BoardServer::bind_with(");
    assert!(open < captures && captures < recover);
    assert!(recover < node && recover < board);
    // Each call stays once.
    assert_eq!(src.matches("captures_prepared_to_unknown").count(), 1);
    assert_eq!(src.matches("desktop_ops_recover").count(), 1);
}
