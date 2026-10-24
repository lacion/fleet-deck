//! The admission, the attempted step and the caps of the input tools (R13 P7
//! §3.1, §7.6, §9, task K1): the charges of one acceptance, the two caps with
//! their stored refusals, the input rate, no refund, the sign-out in the
//! `attempted` transaction, `finished_at`, and the input caps of the cap
//! change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_text::{code, limits};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;

use super::admit_tests::{grant, new_op, rejected, rows, store, t0};
use super::*;
use crate::store::auth::LoginMethod;
use crate::store::grants::GrantClass;
use crate::store::{DeckStore, call};

const ALL: [GrantClass; 3] = [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];

/// A grant of the three classes with the allowance and the given caps.
async fn input_grant(s: &DeckStore, cap: u32, characters: u32) -> (String, CoordinatorContext) {
    grant(s, "coord", &ALL, |g| {
        g.allow_input = true;
        g.input_cap = cap;
        g.input_character_cap = characters;
    })
    .await
}

fn input_op(gid: &str, ctx: &CoordinatorContext, name: &str, tool: OpTool, length: u32) -> NewOp {
    let mut op = new_op(gid, ctx, name, tool);
    op.class = OpClass::Input;
    op.input_length = Some(length);
    op
}

fn key(gid: &str, ctx: &CoordinatorContext, name: &str) -> NewOp {
    input_op(gid, ctx, name, OpTool::KeyPress, 1)
}

fn text(gid: &str, ctx: &CoordinatorContext, name: &str, length: u32) -> NewOp {
    input_op(gid, ctx, name, OpTool::TypeText, length)
}

/// `(desktop, input, input characters, attempts)` used by the grant.
async fn counters(s: &DeckStore, gid: &str) -> (u32, u32, u32, u32) {
    let g = s.grant(gid.to_owned()).await.unwrap().unwrap();
    (
        g.desktop_used,
        g.input_used,
        g.input_character_used,
        g.desktop_attempts,
    )
}

async fn charged_text(s: &DeckStore, id: &str) -> String {
    let id = id.to_owned();
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(
            "SELECT charged FROM coordinator_desktop_ops WHERE operation_id = ?1",
            [id],
            |r| r.get::<_, String>(0),
        )?)
    })
    .await
    .unwrap()
}

/// `(attempted_at, finished_at, exposure_addrs)` of an operation.
async fn times(s: &DeckStore, id: &str) -> (Option<i64>, Option<i64>, Option<String>) {
    let id = id.to_owned();
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(
            "SELECT attempted_at, finished_at, exposure_addrs
             FROM coordinator_desktop_ops WHERE operation_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn an_accepted_input_charges_the_desktop_action_the_input_action_and_the_typed_characters() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 10, 1000).await;
    let now = t0(&det);
    let Admission::Accepted(row) = s
        .desktop_admit(text(&gid, &ctx, "t1", 42), now)
        .await
        .unwrap()
    else {
        panic!("expected Accepted");
    };
    assert_eq!((row.class, row.input_length), (OpClass::Input, Some(42)));
    assert_eq!(
        row.charged,
        Charges {
            desktop: 1,
            capture: 0
        }
    );
    assert_eq!(counters(&s, &gid).await, (1, 1, 42, 1));
    let stored: serde_json::Value =
        serde_json::from_str(&charged_text(&s, &row.operation_id).await).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({"desktop": 1, "capture": 0, "input": 1, "input_characters": 42})
    );
    // A chord and a movement take the input count, never a character.
    s.desktop_admit(key(&gid, &ctx, "k1"), now + 1)
        .await
        .unwrap();
    let mv = input_op(&gid, &ctx, "m1", OpTool::PointerMove, 0);
    s.desktop_admit(mv, now + 2).await.unwrap();
    assert_eq!(counters(&s, &gid).await, (3, 3, 42, 3));
    let stored: serde_json::Value = serde_json::from_str(&charged_text(&s, "op-k1").await).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({"desktop": 1, "capture": 0, "input": 1, "input_characters": 0})
    );
    // The other tools keep their JSON.
    let (gid2, ctx2) = grant(
        &s,
        "other",
        &[GrantClass::Desktop, GrantClass::Capture],
        |_| {},
    )
    .await;
    let n = new_op(&gid2, &ctx2, "n1", OpTool::Notify);
    s.desktop_admit(n, now + 3).await.unwrap();
    assert_eq!(
        charged_text(&s, "op-n1").await,
        r#"{"desktop":1,"capture":0}"#
    );
}

#[tokio::test]
async fn a_missing_class_is_not_granted_and_a_missing_allowance_is_input_not_allowed() {
    let (det, s) = store().await;
    let now = t0(&det);
    // The input class without the capture class, the desktop class and the
    // input class without `capture`.
    for (i, classes) in [
        vec![GrantClass::Desktop, GrantClass::Input],
        vec![GrantClass::Capture, GrantClass::Input],
        vec![GrantClass::Desktop, GrantClass::Capture],
    ]
    .into_iter()
    .enumerate()
    {
        let (gid, ctx) = grant(&s, &format!("c{i}"), &classes, |_| {}).await;
        let got = s
            .desktop_admit(key(&gid, &ctx, &format!("k{i}")), now)
            .await
            .unwrap();
        assert_eq!(rejected(got), (code::NOT_GRANTED, false), "{classes:?}");
        assert_eq!(counters(&s, &gid).await, (0, 0, 0, 0));
    }
    // The three classes without the allowance.
    let (gid, ctx) = grant(&s, "noallow", &ALL, |_| {}).await;
    let got = s.desktop_admit(key(&gid, &ctx, "k"), now).await.unwrap();
    assert_eq!(rejected(got), (code::INPUT_NOT_ALLOWED, false));
    assert_eq!(counters(&s, &gid).await, (0, 0, 0, 0));
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 0);
    assert_eq!(rows(&s, "coordinator_desktop_rejects").await, 0);
    // An input tool of another class is a defect of the caller.
    let (gid, ctx) = input_grant(&s, 5, 50).await;
    let mut wrong = key(&gid, &ctx, "w");
    wrong.class = OpClass::Desktop;
    assert!(s.desktop_admit(wrong, now).await.is_err());
    let mut none = key(&gid, &ctx, "x");
    none.input_length = None;
    assert!(s.desktop_admit(none, now).await.is_err());
}

#[tokio::test]
async fn the_input_count_cap_refuses_with_a_stored_code_and_a_replay_returns_it() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 2, 1000).await;
    let now = t0(&det);
    for (i, name) in ["k1", "k2"].into_iter().enumerate() {
        let got = s
            .desktop_admit(key(&gid, &ctx, name), now + i64::try_from(i).unwrap())
            .await
            .unwrap();
        assert!(matches!(got, Admission::Accepted(_)));
    }
    let third = key(&gid, &ctx, "k3");
    let refused = s.desktop_admit(third.clone(), now + 5).await.unwrap();
    assert_eq!(rejected(refused), (code::INPUT_CAP_REACHED, true));
    // The refusal consumed one attempt and no action.
    assert_eq!(counters(&s, &gid).await, (2, 2, 0, 3));
    assert_eq!(rows(&s, "coordinator_desktop_rejects").await, 1);
    // The stored refusal answers the same key again, also after the cap rose.
    s.desktop_caps_update(
        &gid,
        CapsChange {
            input_cap: Some(10),
            ..CapsChange::default()
        },
        true,
    )
    .await
    .unwrap()
    .unwrap();
    let again = s.desktop_admit(third, now + 6).await.unwrap();
    assert_eq!(rejected(again), (code::INPUT_CAP_REACHED, true));
    // A cap of zero accepts nothing.
    let (gid, ctx) = grant(&s, "zero", &ALL, |g| g.allow_input = true).await;
    let got = s.desktop_admit(key(&gid, &ctx, "z"), now).await.unwrap();
    assert_eq!(rejected(got), (code::INPUT_CAP_REACHED, true));
}

#[tokio::test]
async fn the_character_cap_counts_the_text_and_the_exact_remainder_fits() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 50, 100).await;
    let now = t0(&det);
    let a = s.desktop_admit(text(&gid, &ctx, "t1", 60), now).await;
    assert!(matches!(a.unwrap(), Admission::Accepted(_)));
    let over = text(&gid, &ctx, "t2", 41);
    let refused = s.desktop_admit(over.clone(), now + 1).await.unwrap();
    assert_eq!(rejected(refused), (code::INPUT_CHARACTERS_REACHED, true));
    assert_eq!(counters(&s, &gid).await, (1, 1, 60, 2));
    // The replay of the stored refusal.
    let again = s.desktop_admit(over, now + 2).await.unwrap();
    assert_eq!(rejected(again), (code::INPUT_CHARACTERS_REACHED, true));
    // A chord needs no character: it fits when the characters are all used.
    let exact = s.desktop_admit(text(&gid, &ctx, "t3", 40), now + 3).await;
    assert!(matches!(exact.unwrap(), Admission::Accepted(_)));
    let got = s.desktop_admit(key(&gid, &ctx, "k1"), now + 4).await;
    assert!(matches!(got.unwrap(), Admission::Accepted(_)));
    assert_eq!(counters(&s, &gid).await, (3, 3, 100, 4));
}

#[tokio::test]
async fn the_desktop_cap_comes_before_the_input_caps() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &ALL, |g| {
        g.allow_input = true;
        g.desktop_cap = Some(1);
        g.input_cap = 0;
    })
    .await;
    let now = t0(&det);
    // One desktop action is left, the input cap is 0: the input cap refuses.
    let got = s.desktop_admit(key(&gid, &ctx, "k1"), now).await.unwrap();
    assert_eq!(rejected(got), (code::INPUT_CAP_REACHED, true));
    s.desktop_caps_update(
        &gid,
        CapsChange {
            desktop_cap: Some(0),
            ..CapsChange::default()
        },
        true,
    )
    .await
    .unwrap()
    .unwrap();
    let got = s.desktop_admit(key(&gid, &ctx, "k2"), now + 1).await;
    assert_eq!(
        rejected(got.unwrap()),
        (code::DESKTOP_CAP_REACHED, true),
        "the desktop cap is checked first"
    );
}

#[tokio::test]
async fn the_input_rate_is_20_for_the_grant_and_20_for_the_caller_session() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 500, 100_000).await;
    let now = t0(&det);
    for i in 0..limits::INPUT_PER_GRANT_WINDOW {
        let got = s
            .desktop_admit(key(&gid, &ctx, &format!("k{i}")), now)
            .await
            .unwrap();
        assert!(matches!(got, Admission::Accepted(_)), "{i}");
    }
    let limited = s.desktop_admit(key(&gid, &ctx, "k-over"), now).await;
    assert_eq!(rejected(limited.unwrap()), (code::RATE_LIMITED, false));
    // A refusal charges nothing and stores nothing.
    assert_eq!(counters(&s, &gid).await, (20, 20, 0, 20));
    assert_eq!(rows(&s, "coordinator_desktop_rejects").await, 0);
    // The window moves on: a minute later the rate has room again.
    let later = now + 61_000;
    let got = s.desktop_admit(key(&gid, &ctx, "k-later"), later).await;
    assert!(matches!(got.unwrap(), Admission::Accepted(_)));
    // The caller session shares one bucket across grants: 20 rows of another
    // grant of the same session fill it.
    let (gid, ctx) = grant(&s, "busy", &ALL, |g| {
        g.allow_input = true;
        g.input_cap = 100;
        g.input_character_cap = 100;
    })
    .await;
    let scope = format!("session:{}/{}", ctx.caller_station, ctx.caller_session);
    call(&s.inner.db, move |conn| {
        let tx = conn.transaction()?;
        for _ in 0..limits::INPUT_PER_SESSION_WINDOW {
            super::rate::charge(&tx, &scope, super::rate::INPUT, later)?;
        }
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    let limited = s.desktop_admit(key(&gid, &ctx, "k-s"), later).await;
    assert_eq!(rejected(limited.unwrap()), (code::RATE_LIMITED, false));
    assert_eq!(counters(&s, &gid).await, (0, 0, 0, 0));
}

#[tokio::test]
async fn the_input_shares_the_mutation_bucket_and_a_duplicate_charges_once() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 100, 1000).await;
    let now = t0(&det);
    let first = key(&gid, &ctx, "dup");
    let Admission::Accepted(row) = s.desktop_admit(first.clone(), now).await.unwrap() else {
        panic!("expected Accepted");
    };
    // The same key and payload replays the row and charges nothing.
    let mut again = first;
    again.operation_id = "op-other".to_owned();
    let got = s.desktop_admit(again, now + 1).await.unwrap();
    assert_eq!(got, Admission::Existing(row));
    assert_eq!(counters(&s, &gid).await, (1, 1, 0, 1));
    // Another payload under the same key conflicts.
    let mut changed = key(&gid, &ctx, "dup");
    changed.payload_hash = "other".to_owned();
    let got = s.desktop_admit(changed, now + 2).await.unwrap();
    assert_eq!(got, Admission::Conflict);
    // The 30 mutations of the shared bucket stop an input too: one input and
    // 29 closes fill it, with the input bucket far from its 20.
    for i in 0..limits::MUTATIONS_PER_WINDOW - 1 {
        let close = new_op(&gid, &ctx, &format!("c{i}"), OpTool::WindowClose);
        let got = s.desktop_admit(close, now + 3).await.unwrap();
        assert!(matches!(got, Admission::Accepted(_)), "{i}");
    }
    let limited = s.desktop_admit(key(&gid, &ctx, "over"), now + 5).await;
    assert_eq!(rejected(limited.unwrap()), (code::RATE_LIMITED, false));
}

#[tokio::test]
async fn the_attempted_input_step_writes_the_snapshot_and_signs_the_addresses_out() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 10, 100).await;
    let now = t0(&det);
    let Admission::Accepted(row) = s.desktop_admit(key(&gid, &ctx, "k1"), now).await.unwrap()
    else {
        panic!("expected Accepted");
    };
    let id = row.operation_id;
    let on_target = s
        .create_login_at(LoginMethod::Password, Some("10.0.0.5"))
        .await
        .unwrap();
    let elsewhere = s
        .create_login_at(LoginMethod::Password, Some("10.0.0.9"))
        .await
        .unwrap();
    let addrs = vec!["10.0.0.5".to_owned(), "fd7a::1".to_owned()];
    let signed = s
        .desktop_op_attempted_input(&id, &addrs, now + 10)
        .await
        .unwrap()
        .expect("the row was prepared");
    assert_eq!(signed.len(), 1);
    assert!(
        s.login_session(&on_target.cookie).await.unwrap().is_none(),
        "the session of the target address is deleted"
    );
    assert!(
        s.login_session(&elsewhere.cookie).await.unwrap().is_some(),
        "a session of another address is untouched"
    );
    let row = s.desktop_op(&id).await.unwrap().unwrap();
    assert_eq!((row.state, row.updated_at), (OpState::Attempted, now + 10));
    assert_eq!(
        times(&s, &id).await,
        (
            Some(now + 10),
            None,
            Some(r#"["10.0.0.5","fd7a::1"]"#.to_owned())
        )
    );
    // The same step again changes nothing: the row is not prepared.
    let again = s
        .desktop_op_attempted_input(&id, &addrs, now + 20)
        .await
        .unwrap();
    assert!(again.is_none());
    assert_eq!(times(&s, &id).await.0, Some(now + 10));
    // Another tool is never an input step, and a failed step signs nobody out.
    let other = new_op(&gid, &ctx, "n1", OpTool::Notify);
    s.desktop_admit(other, now).await.unwrap();
    // (An address that no operation exposes: the sign-in is not refused.)
    let victim = s
        .create_login_at(LoginMethod::Password, Some("10.0.0.7"))
        .await
        .unwrap();
    let got = s
        .desktop_op_attempted_input("op-n1", &["10.0.0.7".to_owned()], now + 30)
        .await
        .unwrap();
    assert!(got.is_none());
    assert!(s.login_session(&victim.cookie).await.unwrap().is_some());
    assert_eq!(times(&s, "op-n1").await, (None, None, None));
}

/// The two sessions of a share: the invite token, and the share row.
async fn board_share(s: &DeckStore, uses: u32) -> fleetdeck_core::Secret<String> {
    use fleetdeck_proto::share::{ShareKind, ShareRole, Steering};
    let new = crate::store::shares::NewShareRow {
        kind: ShareKind::Board,
        target: String::new(),
        role: ShareRole::Collaborator,
        steering: Steering::Open,
        expires_in_days: None,
        uses: Some(uses),
        history: false,
    };
    s.create_share(new).await.unwrap().token
}

async fn session_counts(s: &DeckStore) -> (i64, i64) {
    call(&s.inner.db, |conn| {
        let owner = conn.query_row("SELECT COUNT(*) FROM login_sessions", [], |r| r.get(0))?;
        let member = conn.query_row("SELECT COUNT(*) FROM member_sessions", [], |r| r.get(0))?;
        Ok((owner, member))
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_sign_in_that_comes_after_the_attempted_step_is_refused_and_inserts_nothing() {
    use crate::store::DeckError;

    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 10, 100).await;
    let now = t0(&det);
    let Admission::Accepted(row) = s.desktop_admit(key(&gid, &ctx, "k1"), now).await.unwrap()
    else {
        panic!("expected Accepted");
    };
    // The order login, then input: the new session is deleted by rule 1.
    let before = s
        .create_login_at(LoginMethod::Password, Some("10.0.0.5"))
        .await
        .unwrap();
    let invite = board_share(&s, 1).await;
    let addrs = vec!["10.0.0.5".to_owned()];
    let signed = s
        .desktop_op_attempted_input(&row.operation_id, &addrs, now + 10)
        .await
        .unwrap()
        .expect("the row was prepared");
    assert_eq!(signed.len(), 1);
    assert!(s.login_session(&before.cookie).await.unwrap().is_none());
    assert_eq!(session_counts(&s).await, (0, 0));

    // The order input, then login: the exposure is read in the transaction
    // that would insert the session, so nothing is inserted.
    let owner = s
        .create_login_at(LoginMethod::Password, Some("10.0.0.5"))
        .await;
    assert!(matches!(owner, Err(DeckError::SignInBlocked)), "{owner:?}");
    let link = s.create_login_at(LoginMethod::Link, Some("10.0.0.5")).await;
    assert!(matches!(link, Err(DeckError::SignInBlocked)), "{link:?}");
    let member = s.join_at(&invite, "Ann", Some("10.0.0.5")).await;
    assert!(
        matches!(member, Err(DeckError::SignInBlocked)),
        "{member:?}"
    );
    assert_eq!(session_counts(&s).await, (0, 0));

    // A refused join costs no invite use and creates no member: the one use
    // of the invite still serves a join from another address.
    let ok = s.join_at(&invite, "Ann", Some("10.0.0.9")).await.unwrap();
    assert!(ok.is_some(), "the invite keeps its use after a refusal");
    // No address (an old call), and another address, are never refused.
    s.create_login_at(LoginMethod::Password, Some("10.0.0.9"))
        .await
        .unwrap();
    s.create_login_at(LoginMethod::Password, None)
        .await
        .unwrap();
    assert_eq!(session_counts(&s).await, (2, 1));

    // The exposure and the tail end: the address signs in again.
    det.advance(std::time::Duration::from_secs(200));
    s.invalidate_exposure();
    s.create_login_at(LoginMethod::Password, Some("10.0.0.5"))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_attempted_step_deletes_the_sessions_of_the_sockets_at_the_snapshot_addresses() {
    use crate::board_fence::{SessionKind, SocketKind};

    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 10, 100).await;
    let now = t0(&det);
    let Admission::Accepted(row) = s.desktop_admit(key(&gid, &ctx, "k1"), now).await.unwrap()
    else {
        panic!("expected Accepted");
    };
    // Two sessions whose last address is elsewhere; each has a socket at the
    // snapshot address, one live and one pending. A third has a socket
    // elsewhere only.
    let mut cookies = Vec::new();
    for _ in 0..3 {
        let login = s
            .create_login_at(LoginMethod::Password, Some("10.0.0.9"))
            .await
            .unwrap();
        cookies.push(login);
    }
    let mut hashes = Vec::new();
    for login in &cookies {
        hashes.push(
            s.login_session(&login.cookie)
                .await
                .unwrap()
                .unwrap()
                .session_hash,
        );
    }
    let reg = s.registry();
    let live = reg.register(
        &hashes[0],
        SessionKind::Owner,
        SocketKind::Board,
        "10.0.0.5",
    );
    assert!(reg.go_live(&live, || true));
    let _pending = reg.register(
        &hashes[1],
        SessionKind::Owner,
        SocketKind::Shell,
        "10.0.0.5",
    );
    let _away = reg.register(
        &hashes[2],
        SessionKind::Owner,
        SocketKind::Board,
        "10.0.0.8",
    );
    let addrs = vec!["10.0.0.5".to_owned()];
    let signed = s
        .desktop_op_attempted_input(&row.operation_id, &addrs, now + 10)
        .await
        .unwrap()
        .expect("the row was prepared");
    let mut got: Vec<String> = signed.into_iter().map(|x| x.session_hash).collect();
    got.sort();
    let mut want = vec![hashes[0].clone(), hashes[1].clone()];
    want.sort();
    assert_eq!(got, want);
    assert!(s.login_session(&cookies[0].cookie).await.unwrap().is_none());
    assert!(s.login_session(&cookies[1].cookie).await.unwrap().is_none());
    assert!(s.login_session(&cookies[2].cookie).await.unwrap().is_some());
}

#[tokio::test]
async fn the_plain_attempted_step_and_the_end_set_the_times_once() {
    let (det, s) = store().await;
    let (gid, ctx) = input_grant(&s, 10, 100).await;
    let now = t0(&det);
    for name in ["n1", "n2", "n3"] {
        let n = new_op(&gid, &ctx, name, OpTool::Notify);
        s.desktop_admit(n, now).await.unwrap();
    }
    assert!(s.desktop_op_attempted("op-n1", now + 1).await.unwrap());
    assert_eq!(times(&s, "op-n1").await, (Some(now + 1), None, None));
    let done = OpOutcome::Done(serde_json::json!({}));
    assert!(s.desktop_op_finish("op-n1", done, now + 2).await.unwrap());
    assert_eq!(
        times(&s, "op-n1").await,
        (Some(now + 1), Some(now + 2), None)
    );
    // A prepared row that fails has no attempt time.
    let failed = OpOutcome::Failed(code::BUSY);
    assert!(s.desktop_op_finish("op-n2", failed, now + 3).await.unwrap());
    assert_eq!(times(&s, "op-n2").await, (None, Some(now + 3), None));
    // The recovery sets `finished_at` where it is NULL, for both fixes.
    assert!(s.desktop_op_attempted("op-n3", now + 4).await.unwrap());
    let extra = new_op(&gid, &ctx, "n4", OpTool::Notify);
    s.desktop_admit(extra, now + 4).await.unwrap();
    assert_eq!(s.desktop_ops_recover(now + 9).await.unwrap(), (1, 1));
    assert_eq!(
        times(&s, "op-n3").await,
        (Some(now + 4), Some(now + 9), None)
    );
    assert_eq!(times(&s, "op-n4").await, (None, Some(now + 9), None));
    // The rows that ended before keep their time.
    assert_eq!(times(&s, "op-n1").await.1, Some(now + 2));
    assert_eq!(times(&s, "op-n2").await.1, Some(now + 3));
}

#[tokio::test]
async fn the_link_peer_is_the_address_of_the_newest_link() {
    let (_det, s) = store().await;
    let st = StationId::from("alpha");
    assert_eq!(s.link_peer(&st), None);
    s.set_link_peer(&st, 1, "10.0.0.1".parse().unwrap());
    assert_eq!(s.link_peer(&st), Some("10.0.0.1".parse().unwrap()));
    s.set_link_peer(&st, 3, "::ffff:10.0.0.3".parse().unwrap());
    s.set_link_peer(&st, 2, "10.0.0.2".parse().unwrap());
    assert_eq!(s.link_peer(&st), Some("10.0.0.3".parse().unwrap()));
    s.clear_link_features(&st, 3);
    assert_eq!(s.link_peer(&st), Some("10.0.0.2".parse().unwrap()));
    s.clear_link_features(&st, 2);
    s.clear_link_features(&st, 1);
    assert_eq!(s.link_peer(&st), None);
}

#[tokio::test]
async fn the_input_caps_change_with_the_rules_of_the_recording_caps() {
    let (_det, s) = store().await;
    let (gid, _ctx) = input_grant(&s, 100, 20_000).await;
    let change = |cap, characters| CapsChange {
        input_cap: cap,
        input_character_cap: characters,
        ..CapsChange::default()
    };
    // A decrease needs no elevation; the counters are not touched.
    let got = s
        .desktop_caps_update(&gid, change(Some(50), Some(10_000)), false)
        .await
        .unwrap();
    assert_eq!(got, Ok(CapsChanged { increased: false }));
    let g = s.grant(gid.clone()).await.unwrap().unwrap();
    assert_eq!((g.input_cap, g.input_character_cap), (50, 10_000));
    assert_eq!((g.input_used, g.input_character_used), (0, 0));
    // An increase of either needs the elevation, and changes nothing without.
    for c in [change(Some(60), None), change(None, Some(10_001))] {
        let got = s.desktop_caps_update(&gid, c, false).await.unwrap();
        assert_eq!(got, Err(CapsError::ElevationRequired));
    }
    let g = s.grant(gid.clone()).await.unwrap().unwrap();
    assert_eq!((g.input_cap, g.input_character_cap), (50, 10_000));
    let got = s
        .desktop_caps_update(&gid, change(Some(60), Some(10_001)), true)
        .await
        .unwrap();
    assert_eq!(got, Ok(CapsChanged { increased: true }));
    // The ranges, with the fixed texts.
    for (c, text) in [
        (change(Some(5001), None), "input_cap is 0 to 5000"),
        (
            change(None, Some(1_000_001)),
            "input_character_cap is 0 to 1000000",
        ),
    ] {
        let got = s.desktop_caps_update(&gid, c, true).await.unwrap();
        assert_eq!(got, Err(CapsError::Invalid(text)));
    }
    // Zero is allowed: no new accepted call. An empty change keeps all.
    let got = s
        .desktop_caps_update(&gid, change(Some(0), None), false)
        .await
        .unwrap();
    assert_eq!(got, Ok(CapsChanged { increased: false }));
    let got = s
        .desktop_caps_update(&gid, CapsChange::default(), false)
        .await
        .unwrap();
    assert_eq!(got, Ok(CapsChanged { increased: false }));
    // A grant without the allowance refuses every input limit.
    let (gid, _ctx) = grant(&s, "plain", &ALL, |_| {}).await;
    for c in [
        change(Some(0), None),
        change(None, Some(0)),
        change(Some(1), Some(1)),
    ] {
        let got = s.desktop_caps_update(&gid, c, true).await.unwrap();
        assert_eq!(
            got,
            Err(CapsError::Invalid(
                "input limits need a grant that allows input"
            ))
        );
    }
}
