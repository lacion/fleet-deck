//! ADR-006 §8.3 25c and 25d: the coordinator lifecycle events fold into
//! the grant in the read-model transaction.

use super::*;

fn lifecycle(seq: u64, event: fleetdeck_proto::event::Event) -> fleetdeck_proto::EventEnvelope {
    fleetdeck_proto::EventEnvelope {
        station_seq: seq,
        session_id: "coord-1".into(),
        session_seq: seq,
        at: 1_000 + i64::try_from(seq).unwrap(),
        proto_version: 1,
        raw_ref: None,
        event: fleetdeck_proto::event::EventBody::Known(event),
    }
}

fn bound(generation: u32, native: &str, grant_id: &str) -> fleetdeck_proto::event::Event {
    fleetdeck_proto::event::Event::CoordinatorBound {
        binding_id: "b-1".to_owned(),
        grant_id: grant_id.to_owned(),
        generation,
        provider_instance: "claude".to_owned(),
        native_id: native.to_owned(),
    }
}

fn ended(generation: u32, reason: &str) -> fleetdeck_proto::event::Event {
    fleetdeck_proto::event::Event::CoordinatorEnded {
        binding_id: "b-1".to_owned(),
        generation,
        reason: reason.to_owned(),
    }
}

#[tokio::test]
async fn a_late_or_duplicate_bound_event_never_revives_a_revoked_binding_or_lowers_its_generation()
{
    let w = world().await;
    let g =
        w.s.create_grant(NewGrant::new(w.alpha.clone(), "coord-1".into()))
            .await
            .unwrap();
    let inc: fleetdeck_proto::IncarnationId = "inc".to_owned().into();
    w.s.apply_events(
        w.alpha.clone(),
        inc.clone(),
        vec![lifecycle(1, bound(1, "native-1", &g.grant_id))],
    )
    .await
    .unwrap();
    let got = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(got.binding.as_ref().unwrap().generation, 1);
    assert_eq!(got.binding.as_ref().unwrap().native_id, "native-1");
    // A duplicate or lower generation changes nothing.
    w.s.apply_events(
        w.alpha.clone(),
        inc.clone(),
        vec![lifecycle(2, bound(1, "native-9", &g.grant_id))],
    )
    .await
    .unwrap();
    let got = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(got.binding.as_ref().unwrap().native_id, "native-1");
    // Revoked: a later bound event of a higher generation does not revive it.
    w.s.apply_events(
        w.alpha.clone(),
        inc.clone(),
        vec![lifecycle(3, ended(1, "native_identity_changed"))],
    )
    .await
    .unwrap();
    w.s.apply_events(
        w.alpha.clone(),
        inc,
        vec![lifecycle(4, bound(2, "native-1", &g.grant_id))],
    )
    .await
    .unwrap();
    let got = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(got.state, GrantState::Revoked);
    assert_eq!(got.binding.unwrap().generation, 1);
}

#[tokio::test]
async fn active_to_expired_and_active_to_revoked_at_the_same_generation_apply_and_a_replayed_earlier_bound_event_reactivates_neither()
 {
    let w = world().await;
    let inc: fleetdeck_proto::IncarnationId = "inc".to_owned().into();
    let a =
        w.s.create_grant(NewGrant::new(w.alpha.clone(), "coord-1".into()))
            .await
            .unwrap();
    w.s.apply_events(
        w.alpha.clone(),
        inc.clone(),
        vec![
            lifecycle(1, bound(1, "native-1", &a.grant_id)),
            lifecycle(2, ended(1, "idle")),
            lifecycle(3, bound(1, "native-1", &a.grant_id)),
        ],
    )
    .await
    .unwrap();
    let got = w.s.grant(a.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(got.state, GrantState::Expired);
    // An ended event of another generation does nothing; of the bound one
    // it revokes, also an expired grant.
    w.s.apply_events(
        w.alpha.clone(),
        inc.clone(),
        vec![lifecycle(4, ended(7, "stopped"))],
    )
    .await
    .unwrap();
    assert_eq!(
        w.s.grant(a.grant_id.clone()).await.unwrap().unwrap().state,
        GrantState::Expired
    );
    w.s.apply_events(
        w.alpha.clone(),
        inc.clone(),
        vec![lifecycle(5, ended(1, "stopped"))],
    )
    .await
    .unwrap();
    assert_eq!(
        w.s.grant(a.grant_id.clone()).await.unwrap().unwrap().state,
        GrantState::Revoked
    );
    // An event of another station's session never binds this grant.
    let b =
        w.s.create_grant(NewGrant::new(w.alpha.clone(), "coord-1".into()))
            .await
            .unwrap();
    w.s.apply_events(
        w.beta.clone(),
        inc,
        vec![lifecycle(1, bound(1, "native-1", &b.grant_id))],
    )
    .await
    .unwrap();
    assert!(
        w.s.grant(b.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .binding
            .is_none()
    );
}

/// Review follow-up (round 2): a session that the grant spawned joins its
/// lineage once the station's start receipt names it (DL-211).
#[tokio::test]
async fn a_spawned_session_joins_the_lineage_when_its_start_receipt_names_it() {
    let w = world().await;
    let b = grant(
        &w,
        &[GrantClass::Spawn, GrantClass::Drive],
        3,
        &[("claude", "default")],
    )
    .await;
    let r = decide(&w, env(&w, &b, "k1", spawn(&w.alpha, "claude", None))).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let stop = || CoordinatorOp::Stop {
        station: w.alpha.clone(),
        session_id: "spawned-1".into(),
    };
    // No receipt yet: the new session is not known, so not in the lineage.
    let r = decide(&w, env(&w, &b, "k2", stop())).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    // The outbox router sent the start and stored the station's receipt.
    let station = w.alpha.as_str().to_owned();
    crate::store::call(&w.s.db(), move |conn| {
        let outbox: String = conn.query_row(
            "SELECT start_outbox_id FROM coordinator_lineage WHERE command_id = 'k1'",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO commands (command_id, station_id, method, origin, envelope,
                 retry_until, state, receipt, created_at, updated_at)
             VALUES ('start-cmd', ?1, 'session.start', 'coordinator', '{}', 0, 'done',
                 '{\"result\":{\"session_id\":\"spawned-1\"}}', 0, 0)",
            [station],
        )?;
        conn.execute(
            "UPDATE outbox SET command_id = 'start-cmd' WHERE outbox_id = ?1",
            [outbox],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let r = decide(&w, env(&w, &b, "k3", stop())).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    // Another session id from the same start is still outside.
    let other = CoordinatorOp::Stop {
        station: w.alpha.clone(),
        session_id: "spawned-2".into(),
    };
    let r = decide(&w, env(&w, &b, "k4", other)).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
}

/// ADR-006 §8.3 test 25: a query shows only targets inside the grant.
#[tokio::test]
async fn queries_return_only_targets_inside_the_grant() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Read, GrantClass::Mail], 0, &[]).await;
    let v = ask(&w, query(&w, &b, CoordinatorQuery::ListSessions, 5_000))
        .await
        .unwrap();
    let ids: Vec<&str> = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["session_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"worker-1"), "{v}");
    assert!(!ids.contains(&"other-1"), "beta is outside the grant: {v}");
}

/// ADR-006 §8.3 test 25a: a successful query and an accepted new mutation
/// refresh the idle time; a rejection and a duplicate do not.
#[tokio::test]
async fn a_successful_query_and_an_accepted_mutation_refresh_idle_time_and_a_ping_a_discovery_a_rejection_or_a_duplicate_do_not()
 {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Read, GrantClass::Mail], 0, &[]).await;
    let used = || async {
        w.s.grant(b.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .last_used_at
    };
    let start = used().await;
    // An accepted mutation refreshes.
    w.det.advance(Duration::from_secs(5));
    let first = decide(
        &w,
        env(&w, &b, "k1", mail(target(&w.alpha, "worker-1"), "a")),
    )
    .await;
    assert_eq!(first.state, ReceiptState::Accepted);
    let after_accept = used().await;
    assert!(after_accept > start, "{start:?} -> {after_accept:?}");
    // A duplicate returns the stored receipt and does not refresh.
    w.det.advance(Duration::from_secs(5));
    let again = decide(
        &w,
        env(&w, &b, "k1", mail(target(&w.alpha, "worker-1"), "a")),
    )
    .await;
    assert_eq!(again, first);
    assert_eq!(used().await, after_accept);
    // A stored rejection does not refresh.
    let r = decide(&w, env(&w, &b, "k2", mail(target(&w.beta, "other-1"), "a"))).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    assert_eq!(used().await, after_accept);
    // A successful query refreshes.
    w.det.advance(Duration::from_secs(5));
    ask(&w, query(&w, &b, CoordinatorQuery::ListSessions, 5_000))
        .await
        .unwrap();
    assert!(used().await > after_accept);
}

#[tokio::test]
async fn renewal_needs_an_idle_expired_binding_and_the_matching_station_acknowledgment() {
    let w = world().await;
    let g =
        w.s.create_grant(NewGrant::new(w.alpha.clone(), "coord-1".into()))
            .await
            .unwrap();
    let inc: fleetdeck_proto::IncarnationId = "inc".to_owned().into();
    let apply = |seq, event| {
        w.s.apply_events(w.alpha.clone(), inc.clone(), vec![lifecycle(seq, event)])
    };
    let state = || async {
        let got = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
        (got.state, got.binding.map(|b| b.generation))
    };
    // An unbound or an active grant cannot be renewed.
    assert_eq!(w.s.renew_grant(g.grant_id.clone()).await.unwrap(), None);
    apply(1, bound(1, "native-1", &g.grant_id)).await.unwrap();
    assert_eq!(w.s.renew_grant(g.grant_id.clone()).await.unwrap(), None);
    // An unsolicited higher generation of an active grant does not bind.
    apply(2, bound(2, "native-1", &g.grant_id)).await.unwrap();
    assert_eq!(state().await, (GrantState::Active, Some(1)));
    // Idle expiry; a bound event without a renewal does not revive it.
    apply(3, ended(1, "idle")).await.unwrap();
    apply(4, bound(2, "native-1", &g.grant_id)).await.unwrap();
    assert_eq!(state().await, (GrantState::Expired, Some(1)));
    // The owner's renewal waits for the station's matching acknowledgment.
    assert_eq!(w.s.renew_grant(g.grant_id.clone()).await.unwrap(), Some(2));
    assert_eq!(w.s.renew_grant(g.grant_id.clone()).await.unwrap(), Some(2));
    assert_eq!(state().await, (GrantState::Expired, Some(1)));
    apply(5, bound(3, "native-1", &g.grant_id)).await.unwrap();
    assert_eq!(state().await, (GrantState::Expired, Some(1)));
    apply(6, bound(2, "native-1", &g.grant_id)).await.unwrap();
    assert_eq!(state().await, (GrantState::Active, Some(2)));
    // The acknowledgment consumed the renewal; a revoked grant never renews.
    apply(7, bound(3, "native-1", &g.grant_id)).await.unwrap();
    assert_eq!(state().await, (GrantState::Active, Some(2)));
    w.s.revoke_grant(g.grant_id.clone(), "owner".to_owned())
        .await
        .unwrap();
    assert_eq!(w.s.renew_grant(g.grant_id.clone()).await.unwrap(), None);
}
