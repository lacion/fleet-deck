#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::time::Duration;

use fleetdeck_testkit::Det;
use rusqlite::params;

use super::*;
use crate::store::LoginMethod;
use crate::store::board_fence::sign_out_in_tx;

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// Inserts an input operation row with the given timing columns.
async fn op(
    st: &DeckStore,
    id: &str,
    tool: &str,
    attempted_at: Option<i64>,
    finished_at: Option<i64>,
    addrs: Option<&str>,
) {
    let (id, tool, addrs) = (id.to_owned(), tool.to_owned(), addrs.map(str::to_owned));
    st.db()
        .call(move |c| {
            c.execute(
                "INSERT INTO coordinator_desktop_ops
                 (operation_id, grant_id, request_key, payload_hash, tool, class, binding_id,
                  generation, caller_station, caller_session, target_station, resource, state,
                  charged, created_at, updated_at, attempted_at, finished_at, exposure_addrs)
                 VALUES (?1, 'g', ?1, 'h', ?2, 'input', 'b', 1, 'cs', 'cn', 'ts', 'r',
                         'attempted', '{}', 0, 0, ?3, ?4, ?5)",
                params![id, tool, attempted_at, finished_at, addrs],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

const T0: i64 = 1_000_000;

#[tokio::test]
async fn fence_an_unfinished_operation_exposes_for_45_s_and_60_s_more() {
    let det = Det::new();
    let st = store(&det).await;
    op(
        &st,
        "o1",
        "key_press",
        Some(T0),
        None,
        Some(r#"["10.0.0.1"]"#),
    )
    .await;
    for (now, exposed) in [
        (T0, true),
        (T0 + 45_000, true),
        (T0 + 104_999, true),
        (T0 + 105_000, false),
    ] {
        st.invalidate_exposure();
        let got = st.exposed_addresses(now).await.unwrap();
        assert_eq!(got.contains("10.0.0.1"), exposed, "{now}");
    }
}

#[tokio::test]
async fn fence_a_finished_operation_exposes_for_5_s_and_60_s_more() {
    let det = Det::new();
    let st = store(&det).await;
    op(
        &st,
        "o1",
        "type_text",
        Some(T0),
        Some(T0 + 2_000),
        Some(r#"["10.0.0.1","fd7a::1"]"#),
    )
    .await;
    for (now, exposed) in [
        (T0 + 2_000, true),
        (T0 + 66_999, true),
        (T0 + 67_000, false),
    ] {
        st.invalidate_exposure();
        let got = st.exposed_addresses(now).await.unwrap();
        let want = if exposed {
            set(&["10.0.0.1", "fd7a::1"])
        } else {
            HashSet::new()
        };
        assert_eq!(got, want, "{now}");
    }
}

#[tokio::test]
async fn fence_only_attempted_input_operations_expose() {
    let det = Det::new();
    let st = store(&det).await;
    // Not attempted; another tool; no snapshot.
    op(&st, "o1", "key_press", None, None, Some(r#"["10.0.0.1"]"#)).await;
    op(
        &st,
        "o2",
        "screenshot",
        Some(T0),
        None,
        Some(r#"["10.0.0.2"]"#),
    )
    .await;
    op(&st, "o3", "pointer_move", Some(T0), None, None).await;
    op(
        &st,
        "o4",
        "pointer_move",
        Some(T0),
        None,
        Some(r#"["10.0.0.4"]"#),
    )
    .await;
    let got = st.exposed_addresses(T0 + 1).await.unwrap();
    assert_eq!(got, set(&["10.0.0.4"]));
}

#[tokio::test]
async fn fence_the_union_covers_every_exposing_operation() {
    let det = Det::new();
    let st = store(&det).await;
    op(
        &st,
        "o1",
        "key_press",
        Some(T0),
        None,
        Some(r#"["10.0.0.1"]"#),
    )
    .await;
    op(
        &st,
        "o2",
        "type_text",
        Some(T0 + 10),
        None,
        Some(r#"["10.0.0.2","10.0.0.1"]"#),
    )
    .await;
    op(
        &st,
        "o3",
        "type_text",
        Some(T0 - 500_000),
        None,
        Some(r#"["10.0.0.3"]"#),
    )
    .await;
    let got = st.exposed_addresses(T0 + 20).await.unwrap();
    assert_eq!(got, set(&["10.0.0.1", "10.0.0.2"]));
}

#[tokio::test]
async fn fence_the_exposed_set_is_cached_for_a_second_and_invalidate_clears_it() {
    let det = Det::new();
    let st = store(&det).await;
    assert!(st.exposed_addresses(T0).await.unwrap().is_empty());
    op(
        &st,
        "o1",
        "key_press",
        Some(T0),
        None,
        Some(r#"["10.0.0.1"]"#),
    )
    .await;
    // The cache still answers.
    assert!(st.exposed_addresses(T0 + 500).await.unwrap().is_empty());
    assert!(st.exposed_addresses(T0 + 999).await.unwrap().is_empty());
    // After a second, or after an invalidation, the row shows.
    assert_eq!(
        st.exposed_addresses(T0 + 1_000).await.unwrap(),
        set(&["10.0.0.1"])
    );
    let st2 = store(&det).await;
    assert!(st2.exposed_addresses(T0).await.unwrap().is_empty());
    op(
        &st2,
        "o1",
        "key_press",
        Some(T0),
        None,
        Some(r#"["10.0.0.1"]"#),
    )
    .await;
    st2.invalidate_exposure();
    assert_eq!(
        st2.exposed_addresses(T0 + 1).await.unwrap(),
        set(&["10.0.0.1"])
    );
}

#[tokio::test]
async fn fence_a_malformed_snapshot_counts_as_no_address() {
    let det = Det::new();
    let st = store(&det).await;
    op(&st, "o1", "key_press", Some(T0), None, Some("not json")).await;
    op(&st, "o2", "key_press", Some(T0), None, Some(r#"[1, 2]"#)).await;
    op(
        &st,
        "o3",
        "key_press",
        Some(T0),
        None,
        Some(r#"["10.0.0.3"]"#),
    )
    .await;
    let got = st.exposed_addresses(T0 + 1).await.unwrap();
    assert_eq!(got, set(&["10.0.0.3"]));
}

async fn member_session(st: &DeckStore, hash: &str, addr: Option<&str>, seen: Option<i64>) {
    let (hash, addr) = (hash.to_owned(), addr.map(str::to_owned));
    st.db()
        .call(move |c| {
            c.execute(
                "INSERT OR IGNORE INTO shares
                 (share_id, kind, target, role, steering, expires_at, created_at)
                 VALUES ('s', 'board', '', 'approver', 'open', 9999999999999, 0)",
                [],
            )?;
            c.execute(
                "INSERT INTO members (member_id, share_id, display_name, created_at)
                 VALUES ('m' || ?1, 's', 'n' || ?1, 0)",
                params![hash],
            )?;
            c.execute(
                "INSERT INTO member_sessions
                 (session_hash, member_id, share_id, csrf_token, created_at, expires_at,
                  last_addr, last_addr_seen_at)
                 VALUES (?1, 'm' || ?1, 's', 'c', 0, 9999999999999, ?2, ?3)",
                params![hash, addr, seen],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn sign_out(st: &DeckStore, addrs: &[&str]) -> Vec<SignedOut> {
    sign_out_with(st, addrs, &[]).await
}

async fn sign_out_with(
    st: &DeckStore,
    addrs: &[&str],
    sessions: &[(SessionKind, String)],
) -> Vec<SignedOut> {
    let addrs: Vec<String> = addrs.iter().map(|s| (*s).to_owned()).collect();
    let sessions = sessions.to_vec();
    st.db()
        .call(move |c| {
            let tx = c.transaction()?;
            let out = sign_out_in_tx(&tx, &addrs, &sessions, 0).map_err(|e| {
                fleetdeck_core::db::DbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(
                    e.to_string().into(),
                ))
            })?;
            tx.commit()?;
            Ok(out)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn fence_sign_out_in_tx_deletes_owner_and_member_sessions_of_the_addresses_at_any_age() {
    let det = Det::new();
    let st = store(&det).await;
    let owner = st
        .create_login_at(LoginMethod::Link, Some("10.0.0.1"))
        .await
        .unwrap();
    let other = st
        .create_login_at(LoginMethod::Link, Some("10.0.0.9"))
        .await
        .unwrap();
    let nowhere = st.create_login(LoginMethod::Link).await.unwrap();
    // A session that was last seen a year ago is still deleted.
    det.advance(Duration::from_secs(1));
    member_session(&st, "mem-old", Some("fd7a::1"), Some(1)).await;
    member_session(&st, "mem-safe", Some("10.0.0.9"), Some(5)).await;
    member_session(&st, "mem-none", None, None).await;

    let out = sign_out(&st, &["10.0.0.1", "fd7a::1"]).await;
    let kinds: Vec<(SessionKind, bool)> = out
        .iter()
        .map(|s| (s.kind, s.session_hash == "mem-old"))
        .collect();
    assert_eq!(out.len(), 2, "{kinds:?}");
    assert!(out.iter().any(|s| s.kind == SessionKind::Owner));
    assert!(
        out.iter()
            .any(|s| s.kind == SessionKind::Member && s.session_hash == "mem-old")
    );

    assert!(st.login_session(&owner.cookie).await.unwrap().is_none());
    assert!(st.login_session(&other.cookie).await.unwrap().is_some());
    assert!(st.login_session(&nowhere.cookie).await.unwrap().is_some());
    assert!(
        !st.session_row_exists(SessionKind::Member, "mem-old")
            .await
            .unwrap()
    );
    assert!(
        st.session_row_exists(SessionKind::Member, "mem-safe")
            .await
            .unwrap()
    );
    assert!(
        st.session_row_exists(SessionKind::Member, "mem-none")
            .await
            .unwrap()
    );
    // No address, nothing to delete.
    assert!(sign_out(&st, &[]).await.is_empty());
}

#[tokio::test]
async fn fence_sign_out_in_tx_deletes_the_given_sessions_whatever_their_last_address() {
    let det = Det::new();
    let st = store(&det).await;
    // A session whose last address is elsewhere but that has a socket at a
    // snapshot address: the registry names it.
    let moved = st
        .create_login_at(LoginMethod::Link, Some("10.0.0.9"))
        .await
        .unwrap();
    let moved_hash = st
        .login_session(&moved.cookie)
        .await
        .unwrap()
        .unwrap()
        .session_hash;
    let bystander = st
        .create_login_at(LoginMethod::Link, Some("10.0.0.8"))
        .await
        .unwrap();
    member_session(&st, "mem-moved", Some("10.0.0.9"), Some(5)).await;
    member_session(&st, "mem-safe", Some("10.0.0.9"), Some(5)).await;
    let named = [
        (SessionKind::Owner, moved_hash.clone()),
        (SessionKind::Member, "mem-moved".to_owned()),
        // A hash of the other kind does not delete a row of this kind.
        (SessionKind::Owner, "mem-safe".to_owned()),
    ];
    let out = sign_out_with(&st, &["10.0.0.1"], &named).await;
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(out.contains(&SignedOut {
        kind: SessionKind::Owner,
        session_hash: moved_hash
    }));
    assert!(out.contains(&SignedOut {
        kind: SessionKind::Member,
        session_hash: "mem-moved".to_owned()
    }));
    assert!(st.login_session(&moved.cookie).await.unwrap().is_none());
    assert!(st.login_session(&bystander.cookie).await.unwrap().is_some());
    assert!(
        st.session_row_exists(SessionKind::Member, "mem-safe")
            .await
            .unwrap()
    );
    // A session that is both at an address and named is returned once.
    let both = st
        .create_login_at(LoginMethod::Link, Some("10.0.0.1"))
        .await
        .unwrap();
    let both_hash = st
        .login_session(&both.cookie)
        .await
        .unwrap()
        .unwrap()
        .session_hash;
    let out = sign_out_with(&st, &["10.0.0.1"], &[(SessionKind::Owner, both_hash)]).await;
    assert_eq!(out.len(), 1, "{out:?}");
}

#[tokio::test]
async fn fence_a_session_records_its_address_and_a_touch_writes_it_again_only_when_due() {
    let det = Det::new();
    let st = store(&det).await;
    let login = st
        .create_login_at(LoginMethod::Password, Some("10.0.0.1"))
        .await
        .unwrap();
    let hash = st
        .login_session(&login.cookie)
        .await
        .unwrap()
        .unwrap()
        .session_hash;
    let read = |st: DeckStore, hash: String| async move {
        st.db()
            .call(move |c| {
                Ok(c.query_row(
                    "SELECT last_addr, last_addr_seen_at FROM login_sessions WHERE session_hash = ?1",
                    [hash],
                    |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?)),
                )?)
            })
            .await
            .unwrap()
    };
    let created = det.now_ms().try_into().unwrap();
    assert_eq!(
        read(st.clone(), hash.clone()).await,
        (Some("10.0.0.1".to_owned()), Some(created))
    );
    // The same address within 10 s: no write.
    st.touch_session_addr(SessionKind::Owner, &hash, "10.0.0.1", created + 10_000)
        .await
        .unwrap();
    assert_eq!(read(st.clone(), hash.clone()).await.1, Some(created));
    // A new address writes at once.
    st.touch_session_addr(SessionKind::Owner, &hash, "10.0.0.2", created + 1_000)
        .await
        .unwrap();
    assert_eq!(
        read(st.clone(), hash.clone()).await,
        (Some("10.0.0.2".to_owned()), Some(created + 1_000))
    );
    // The same address after more than 10 s writes the time.
    st.touch_session_addr(SessionKind::Owner, &hash, "10.0.0.2", created + 11_001)
        .await
        .unwrap();
    assert_eq!(
        read(st.clone(), hash.clone()).await.1,
        Some(created + 11_001)
    );
    // A session with no address (the old call) gets one at its first touch.
    let bare = st.create_login(LoginMethod::Link).await.unwrap();
    let bare_hash = st
        .login_session(&bare.cookie)
        .await
        .unwrap()
        .unwrap()
        .session_hash;
    assert_eq!(read(st.clone(), bare_hash.clone()).await, (None, None));
    st.touch_session_addr(SessionKind::Owner, &bare_hash, "10.0.0.3", created)
        .await
        .unwrap();
    assert_eq!(
        read(st, bare_hash).await,
        (Some("10.0.0.3".to_owned()), Some(created))
    );
}
