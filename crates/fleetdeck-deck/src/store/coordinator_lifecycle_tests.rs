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
