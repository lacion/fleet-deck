//! MCP P4a PRE-PR R1 fixes at the store level: `session.ended` revokes the
//! worker grants of its session, and a renewal cannot pass the worker cap.
//! They share the world of `worker_grant_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::command::CommandResult;

use super::*;

/// The worker grant of start `id`.
async fn of_start(w: &World, id: &str) -> Grant {
    grants_by(w)
        .await
        .into_iter()
        .find(|(c, _)| c == id)
        .unwrap()
        .1
}

/// Sends start `id` and settles it with an accepted receipt that names `session`.
async fn accept_start(w: &World, id: &str, session: &str) {
    w.s.mark_sent(CommandId::from(id)).await.unwrap().unwrap();
    let receipt = Receipt::accepted(
        CommandId::from(id),
        1,
        CommandResult {
            session_id: Some(session.into()),
            input_id: None,
        },
        w.s.now_ms(),
    );
    w.s.settle_command(CommandId::from(id), Settle::Receipt(receipt))
        .await
        .unwrap();
}

fn ended() -> Event {
    Event::SessionEnded {
        reason: "start_failed".into(),
        exit_code: None,
    }
}

fn bound(grant: &Grant, binding: &str, generation: u32, native: &str) -> Event {
    Event::CoordinatorBound {
        binding_id: binding.to_owned(),
        grant_id: grant.grant_id.clone(),
        generation,
        provider_instance: "claude".to_owned(),
        native_id: native.to_owned(),
    }
}

const ACTIVE: (GrantState, Option<String>) = (GrantState::Active, None);

fn revoked(reason: &str) -> (GrantState, Option<String>) {
    (GrantState::Revoked, Some(reason.to_owned()))
}

#[tokio::test]
async fn a_session_end_before_the_handoff_revokes_its_worker_grant_at_once() {
    let w = world().await;
    // The start failed before the credential handoff: the receipt names the session.
    submit(&w, "s1", &on()).await;
    accept_start(&w, "s1", "w1").await;
    let g1 = of_start(&w, "s1").await;
    assert_eq!(state_of(&w, &g1).await, ACTIVE);
    apply(&w, &w.a, 1, "w1", ended()).await;
    assert_eq!(state_of(&w, &g1).await, revoked("session_ended"));
    // The same event again (a replay) changes nothing.
    apply(&w, &w.a, 1, "w1", ended()).await;
    assert_eq!(state_of(&w, &g1).await, revoked("session_ended"));

    // Another session, or the same session of another station: the grant stays.
    submit(&w, "s2", &on()).await;
    accept_start(&w, "s2", "worker-1").await;
    let g2 = of_start(&w, "s2").await;
    apply(&w, &w.a, 2, "owner-1", ended()).await;
    apply(&w, &w.b, 1, "worker-1", ended()).await;
    assert_eq!(state_of(&w, &g2).await, ACTIVE);
    apply(&w, &w.a, 3, "worker-1", ended()).await;
    assert_eq!(state_of(&w, &g2).await, revoked("session_ended"));

    // A bound worker grant names its session by `session_id`.
    submit(&w, "s3", &on()).await;
    let g3 = of_start(&w, "s3").await;
    apply(&w, &w.a, 4, "owner-1", bound(&g3, "b3", 1, "n3")).await;
    assert_eq!(state_of(&w, &g3).await, ACTIVE);
    apply(&w, &w.a, 5, "owner-1", ended()).await;
    assert_eq!(state_of(&w, &g3).await, revoked("session_ended"));

    // An owner grant is never touched.
    let owner =
        w.s.create_grant(NewGrant::new(w.a.clone(), "coord-1".into()))
            .await
            .unwrap();
    apply(&w, &w.a, 6, "coord-1", ended()).await;
    assert_eq!(state_of(&w, &owner).await, ACTIVE);
}

#[tokio::test]
async fn a_worker_renewal_cannot_pass_the_cap() {
    let w = world().await;
    // g0 binds and goes idle: it frees its slot.
    submit(&w, "r0", &on()).await;
    let g0 = of_start(&w, "r0").await;
    apply(&w, &w.a, 1, "w1", bound(&g0, "b0", 1, "n0")).await;
    let idle = Event::CoordinatorEnded {
        binding_id: "b0".to_owned(),
        generation: 1,
        reason: "idle".to_owned(),
    };
    apply(&w, &w.a, 2, "w1", idle).await;
    assert_eq!(
        state_of(&w, &g0).await,
        (GrantState::Expired, Some("idle".to_owned()))
    );

    // 200 other starts fill the cap.
    for n in 0..WORKER_GRANT_CAP {
        let out = submit(&w, &format!("c{n}"), &on()).await;
        assert!(sent_spec(&out.command).is_some(), "start {n}");
    }
    let in_effect = "SELECT count(*) FROM tokens WHERE kind = 'worker' AND state = 'active'";
    assert_eq!(rows(&w, in_effect).await, 200);
    assert_eq!(w.s.renew_grant(g0.grant_id.clone()).await.unwrap(), None);

    // One slot frees: the renewal is allowed, and the slot is taken again.
    let c0 = of_start(&w, "c0").await;
    assert!(
        w.s.revoke_grant(c0.grant_id, "owner".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(rows(&w, in_effect).await, 199);
    assert_eq!(w.s.renew_grant(g0.grant_id.clone()).await.unwrap(), Some(2));
    let out = submit(&w, "c200", &on()).await;
    assert!(sent_spec(&out.command).is_some());
    assert_eq!(rows(&w, in_effect).await, 200);

    // The station acknowledges the renewal: it passes the cap, so it is revoked
    // and the station's binding is revoked too.
    apply(&w, &w.a, 3, "w1", bound(&g0, "b0b", 2, "n0")).await;
    assert_eq!(state_of(&w, &g0).await, revoked("worker_cap"));
    assert_eq!(rows(&w, in_effect).await, 200);
    let queued = w.s.outbox().await.unwrap();
    let revokes: Vec<_> = queued
        .iter()
        .filter_map(|i| match serde_json::from_value(i.payload.clone()) {
            Ok(CommandKind::Session {
                command:
                    SessionCommand::CoordinatorRevoke {
                        session_id,
                        grant_id,
                    },
            }) => Some((i.origin.clone(), i.station_id.clone(), session_id, grant_id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        revokes,
        [(
            "board".to_owned(),
            w.a.clone(),
            "w1".into(),
            g0.grant_id.clone()
        )]
    );
}
