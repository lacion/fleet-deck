//! The change signal of a credential and the admission of its streams
//! (MCP P4b §8.1, §11 test 1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn binding(id: &str, session: &str) -> Binding {
    Binding {
        binding_id: id.to_owned(),
        grant_id: format!("grant-{id}"),
        session_id: session.into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("native-1".to_owned()),
        generation: 1,
        classes: [ToolClass::Read, ToolClass::Mail].into_iter().collect(),
        state: BindingState::Active,
        control_skill: "SKILL v1".to_owned(),
        kind: GrantKind::Coordinator,
        scope: GrantScope::Lineage,
    }
}

/// A new binding with its token and its digest.
fn add(bindings: &Bindings, id: &str, session: &str) -> (String, String) {
    let (token, digest) = mint().unwrap();
    bindings.insert(digest.clone(), binding(id, session));
    (token.into_exposed(), digest)
}

fn stream(bindings: &Bindings, token: &str, kind: StreamKind) -> StreamAuth {
    bindings.authenticate_stream(Some(&format!("Bearer {token}")), kind)
}

fn open(bindings: &Bindings, token: &str) -> (BindingHandle, StreamPermits) {
    match stream(bindings, token, StreamKind::Listen) {
        StreamAuth::Ok {
            handle, permits, ..
        } => (handle, permits),
        _ => panic!("the stream was not admitted"),
    }
}

/// True while the sender of the watch is alive.
fn is_open(handle: &BindingHandle) -> bool {
    handle.changes.has_changed().is_ok()
}

#[test]
fn a_change_bumps_the_watch_and_hard_invalidation_closes_it() {
    let bindings = Bindings::default();

    // An in-place change bumps the version and keeps the watch open.
    let (token, digest) = add(&bindings, "b-1", "s-1");
    let (mut handle, _permits) = open(&bindings, &token);
    assert_eq!(handle.digest, digest);
    assert!(!handle.changes.has_changed().unwrap());
    assert!(bindings.rewrite(&digest, |b| {
        b.classes.insert(ToolClass::Own);
    }));
    assert!(handle.changes.has_changed().unwrap());
    assert_eq!(*handle.changes.borrow_and_update(), 1);
    assert!(bindings.rewrite(&digest, |b| b.generation = 2));
    assert!(bindings.rewrite(&digest, |b| b.generation = 3));
    assert_eq!(*handle.changes.borrow_and_update(), 3, "changes coalesce");
    assert!(is_open(&handle));
    assert!(!bindings.rewrite("unknown", |_| {}));
    assert_eq!(bindings.get("b-1").unwrap().generation, 3);

    // A plain `update` that keeps the binding alive does not signal.
    assert!(bindings.update("b-1", |b| b.generation = 4));
    assert!(!handle.changes.has_changed().unwrap());

    // The handle is registered under the lock of the authentication: a
    // rewrite right after it is seen.
    let (mut fresh, _fresh_permits) = open(&bindings, &token);
    assert!(bindings.rewrite(&digest, |b| b.generation = 5));
    assert!(fresh.changes.has_changed().unwrap());
    assert_eq!(*fresh.changes.borrow_and_update(), 4);

    // `update` to Revoked and to Expired close the watch.
    assert!(bindings.update("b-1", |b| b.state = BindingState::Revoked));
    assert!(!is_open(&handle));
    assert!(!is_open(&fresh));
    let (token, digest) = add(&bindings, "b-2", "s-2");
    let (handle, _permits) = open(&bindings, &token);
    assert!(bindings.update_digest(&digest, |b| b.state = BindingState::Expired));
    assert!(!is_open(&handle));

    // `rewrite` to an ended state closes it too.
    let (token, digest) = add(&bindings, "b-3", "s-3");
    let (handle, _permits) = open(&bindings, &token);
    assert!(bindings.rewrite(&digest, |b| b.state = BindingState::Revoked));
    assert!(!is_open(&handle));
    let (token, digest) = add(&bindings, "b-4", "s-4");
    let (handle, _permits) = open(&bindings, &token);
    assert!(bindings.rewrite(&digest, |b| b.state = BindingState::Expired));
    assert!(!is_open(&handle));

    // A new credential under the same binding id closes the old watch and
    // gets a new incarnation.
    let (token, digest) = add(&bindings, "b-5", "s-5");
    let (old, _old_permits) = open(&bindings, &token);
    let (new_token, new_digest) = add(&bindings, "b-5", "s-5");
    assert!(!is_open(&old));
    assert!(!bindings.has_digest(&digest));
    let (new, _new_permits) = open(&bindings, &new_token);
    assert_eq!(new.digest, new_digest);
    assert!(new.incarnation > old.incarnation);
    assert!(is_open(&new));

    // The identity change of a root session revokes: the watch closes.
    let (token, _) = add(&bindings, "b-6", "s-6");
    let (handle, _permits) = open(&bindings, &token);
    assert_eq!(
        bindings.native_identity("b-6", "native-other"),
        Some(BindingState::Revoked)
    );
    assert!(!is_open(&handle));

    // A process end and a session revoke end every stream of the session.
    let (token_a, _) = add(&bindings, "b-7", "s-7");
    let (token_b, _) = add(&bindings, "b-8", "s-7");
    let (a, _a_permits) = open(&bindings, &token_a);
    let (b, _b_permits) = open(&bindings, &token_b);
    assert_eq!(bindings.end_session(&"s-7".into()).len(), 2);
    assert!(!is_open(&a));
    assert!(!is_open(&b));
    let (token, _) = add(&bindings, "b-9", "s-9");
    let (handle, _permits) = open(&bindings, &token);
    bindings.revoke_session(&"s-9".into());
    assert!(!is_open(&handle));

    // The drop of the entry closes the watch.
    let (token, _) = add(&bindings, "b-10", "s-10");
    let (handle, _permits) = open(&bindings, &token);
    assert!(is_open(&handle));
    drop(bindings);
    assert!(!is_open(&handle));
}

#[tokio::test]
async fn a_closed_watch_ends_changed_and_a_bump_wakes_it() {
    let bindings = Bindings::default();
    let (token, digest) = add(&bindings, "b-1", "s-1");
    let (mut handle, _permits) = open(&bindings, &token);
    assert!(bindings.rewrite(&digest, |b| b.generation = 2));
    handle.changes.changed().await.unwrap();
    assert!(bindings.update("b-1", |b| b.state = BindingState::Revoked));
    assert!(handle.changes.changed().await.is_err());
}

#[test]
fn streams_have_their_own_limits_and_the_same_refusals_as_a_request() {
    let bindings = Bindings::default();
    let (token, digest) = add(&bindings, "b-1", "s-1");

    // Two listens and two watches per binding; the third of a kind is
    // limited and holds nothing.
    let listens = [open(&bindings, &token), open(&bindings, &token)];
    assert!(matches!(
        stream(&bindings, &token, StreamKind::Listen),
        StreamAuth::Limited
    ));
    let watches = [
        stream(&bindings, &token, StreamKind::Watch),
        stream(&bindings, &token, StreamKind::Watch),
    ];
    assert!(watches.iter().all(|w| matches!(w, StreamAuth::Ok { .. })));
    assert!(matches!(
        stream(&bindings, &token, StreamKind::Watch),
        StreamAuth::Limited
    ));
    // The request permits are not touched: all eight are free.
    let requests: Vec<_> = (0..BINDING_CONCURRENCY)
        .map(|_| bindings.authenticate(Some(&format!("Bearer {token}"))))
        .collect();
    drop(requests);
    drop(listens);
    drop(watches);
    // The permits came back.
    assert!(matches!(
        stream(&bindings, &token, StreamKind::Listen),
        StreamAuth::Ok { .. }
    ));

    // The refusals of `authenticate`: missing, malformed, unknown, held, ended.
    assert!(matches!(
        bindings.authenticate_stream(None, StreamKind::Listen),
        StreamAuth::Unauthorized
    ));
    assert!(matches!(
        bindings.authenticate_stream(Some("Bearer fdc1_short"), StreamKind::Listen),
        StreamAuth::Unauthorized
    ));
    let (unknown, _) = mint().unwrap();
    assert!(matches!(
        stream(&bindings, unknown.expose(), StreamKind::Watch),
        StreamAuth::Unauthorized
    ));
    bindings.hold([digest.clone()]);
    assert!(matches!(
        stream(&bindings, &token, StreamKind::Watch),
        StreamAuth::Starting
    ));
    bindings.release([&digest]);
    assert!(bindings.update("b-1", |b| b.state = BindingState::Revoked));
    bindings.hold([digest.clone()]);
    assert!(
        matches!(
            stream(&bindings, &token, StreamKind::Watch),
            StreamAuth::Unauthorized
        ),
        "an ended binding is refused before the hold"
    );
}

#[test]
fn the_station_limit_refuses_a_stream_and_holds_nothing() {
    let bindings = Bindings::default();
    // 128 bindings of 2 watches fill the station limit of the watches.
    let mut held = Vec::new();
    for i in 0..WATCHES_PER_STATION / WATCHES_PER_BINDING {
        let (token, _) = add(&bindings, &format!("w-{i}"), &format!("ws-{i}"));
        for _ in 0..WATCHES_PER_BINDING {
            held.push(stream(&bindings, &token, StreamKind::Watch));
        }
    }
    assert!(held.iter().all(|w| matches!(w, StreamAuth::Ok { .. })));
    let (token, _) = add(&bindings, "b-1", "s-1");
    assert!(matches!(
        stream(&bindings, &token, StreamKind::Watch),
        StreamAuth::Limited
    ));
    // The station limit of the listens is its own, and the binding kept its
    // own permits: nothing leaked from the refused watch.
    assert!(matches!(
        stream(&bindings, &token, StreamKind::Listen),
        StreamAuth::Ok { .. }
    ));
    held.pop();
    assert!(matches!(
        stream(&bindings, &token, StreamKind::Watch),
        StreamAuth::Ok { .. }
    ));
}

#[test]
fn the_rate_window_counts_a_stream() {
    let bindings = Bindings::default();
    let (token, _) = add(&bindings, "b-1", "s-1");
    for _ in 0..BINDING_RATE {
        // A permit that drops at once keeps the stream limits free.
        assert!(matches!(
            stream(&bindings, &token, StreamKind::Listen),
            StreamAuth::Ok { .. }
        ));
    }
    assert!(matches!(
        stream(&bindings, &token, StreamKind::Listen),
        StreamAuth::Limited
    ));
}
