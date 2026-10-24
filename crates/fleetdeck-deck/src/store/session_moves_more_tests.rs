#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The restart, the undo request, the reads and the authority of the session
//! moves on the deck store (FG-9, T4.4b); the setup is in the parent.

use super::*;
use crate::store::session_moves_steps::{MoveGate, MovePatch};

// ----- the restart -----

#[tokio::test]
async fn a_restart_aborts_the_steps_that_ran_in_memory_and_returns_the_starting_rows() {
    let m = moves().await;
    m.show(vec![pushed("s1"), pushed("s2"), pushed("s3")]).await;
    let ids = [
        m.started("c1", "s1").await,
        m.started("c2", "s2").await,
        m.started("c3", "s3").await,
    ];
    m.w.s
        .move_advance(
            ids[1].clone(),
            &[MoveState::Holding],
            MoveState::Importing,
            MovePatch::default(),
        )
        .await
        .unwrap();
    m.w.s
        .move_advance(
            ids[2].clone(),
            &[MoveState::Holding],
            MoveState::Starting,
            MovePatch::default(),
        )
        .await
        .unwrap();
    m.w.s.move_sent(ids[2].clone(), 1000).await.unwrap();
    let starting = m.w.s.move_recover().await.unwrap();
    assert_eq!(starting, vec![ids[2].clone()]);
    for id in &ids[..2] {
        let row = m.row(id).await;
        assert_eq!(row.state, MoveState::Aborting);
        assert_eq!(row.code.as_deref(), Some("move_lost"));
        assert_eq!(
            row.message.as_deref(),
            Some("The move stopped when the deck restarted. The session stays on alpha.")
        );
    }
    let row = m.row(&ids[2]).await;
    assert_eq!((row.state, row.next_step_at), (MoveState::Starting, None));
}

// ----- the undo request -----

#[tokio::test]
async fn the_undo_request_needs_resume_failed_and_the_same_command_id_gives_the_first_answer() {
    let m = moves().await;
    m.show(vec![pushed("s1")]).await;
    let id = m.started("c1", "s1").await;
    let s = &m.w.s;
    let undo = |c: &str| s.move_undo_request(id.clone(), c.to_owned());
    // Refused in every state but `resume_failed`.
    let steps: [(MoveState, &'static [MoveState]); 3] = [
        (MoveState::Holding, &[]),
        (MoveState::Starting, &[MoveState::Holding]),
        (MoveState::Done, &[MoveState::Starting]),
    ];
    for (state, from) in steps {
        if !from.is_empty() {
            s.move_advance(id.clone(), from, state, MovePatch::default())
                .await
                .unwrap();
        }
        let r = undo("u1").await.unwrap().unwrap_err();
        assert_eq!(r.code, "move_not_undoable", "{state:?}");
        assert_eq!(
            r.message,
            "Only a move whose new session could not resume can be undone."
        );
    }
    assert_eq!(
        s.move_undo_request("nope".to_owned(), "u1".to_owned())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "unknown_move"
    );
    s.move_advance(
        id.clone(),
        &[MoveState::Done],
        MoveState::ResumeFailed,
        MovePatch::default(),
    )
    .await
    .unwrap();
    let row = undo("u1").await.unwrap().unwrap();
    assert_eq!(row.state, MoveState::Undoing);
    let first = row.undo_command_id.clone().unwrap();
    assert!(first.starts_with("moveundo-"));
    // The same command id again: the first answer, no second command id.
    let again = undo("u1").await.unwrap().unwrap();
    assert_eq!(again.undo_command_id, Some(first));
    // Another command id on a move that is undoing is refused.
    assert_eq!(
        undo("u2").await.unwrap().unwrap_err().code,
        "move_not_undoable"
    );
    // The end frees the source session.
    s.move_advance(
        id.clone(),
        &[MoveState::Undoing],
        MoveState::Undone,
        MovePatch::clear(),
    )
    .await
    .unwrap();
    m.started("c2", "s1").await;
}

// ----- the reads -----

#[tokio::test]
async fn the_board_reads_the_newest_move_of_a_session_from_either_end() {
    let m = moves().await;
    m.show(vec![pushed("s1")]).await;
    let id = m.started("c1", "s1").await;
    m.w.s
        .move_set_dst(id.clone(), "n1".to_owned())
        .await
        .unwrap();
    let st = m.w.st.0.clone();
    let src =
        m.w.s
            .move_of_session(st, "s1".to_owned())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        (src.move_id.as_str(), src.dst_session.as_deref()),
        (id.as_str(), Some("n1"))
    );
    let dst =
        m.w.s
            .move_of_session(m.bravo.0.clone(), "n1".to_owned())
            .await
            .unwrap();
    assert_eq!(dst.map(|v| v.move_id), Some(id));
    assert!(
        m.w.s
            .move_of_session("x".to_owned(), "y".to_owned())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_summary_is_the_newest_assistant_message_of_the_timeline() {
    let m = moves().await;
    m.show(vec![pushed("s1")]).await;
    let message = |seq: u64, text: &str, parent: Option<&str>| EventEnvelope {
        station_seq: seq,
        session_id: "s1".into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::MessageCompleted {
            turn_id: "t1".into(),
            parent_node_id: parent.map(Into::into),
            text: text.to_owned(),
        }),
    };
    assert_eq!(
        m.w.s
            .move_last_message(m.w.st.0.clone(), "s1".to_owned())
            .await
            .unwrap(),
        None
    );
    fold(
        &m,
        &m.w.st,
        "inc",
        vec![
            message(1, "first", None),
            message(2, "from a subagent", Some("n1")),
            message(3, "  ", None),
        ],
    )
    .await;
    assert_eq!(
        m.w.s
            .move_last_message(m.w.st.0.clone(), "s1".to_owned())
            .await
            .unwrap()
            .as_deref(),
        Some("first")
    );
}

// ----- the authority -----

#[tokio::test]
async fn the_station_check_finds_the_move_in_command_of_a_starting_coordinator_move() {
    use fleetdeck_proto::mutation::AuthorityCheck as Check;
    let m = moves().await;
    m.w.s.set_fleet_control(true);
    m.show(vec![pushed("s1")]).await;
    let grant = crate::store::grants::NewGrant::new(m.w.st.clone(), "coord".into());
    let mut grant = grant;
    grant.classes = BTreeSet::from([crate::store::grants::GrantClass::Manage]);
    grant.stations = BTreeSet::from([m.w.st.clone(), m.bravo.clone()]);
    grant.scope = fleetdeck_proto::command::GrantScope::Fleet;
    let stored = m.w.s.create_grant(grant).await.unwrap();
    let binding = fleetdeck_proto::mutation::CoordinatorBinding {
        binding_id: "b".to_owned(),
        grant_id: stored.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "n-coord".to_owned(),
    };
    assert!(m.w.s.bind_grant(binding).await.unwrap());
    let authority = CommandAuthority {
        grant_id: stored.grant_id.clone(),
        caller_station: m.w.st.clone(),
        caller_session: "coord".into(),
        generation: 1,
    };
    let mut req = m.request("k1", "s1");
    req.requested_by = format!("coordinator:{}", stored.grant_id);
    req.authority = Some(authority.clone());
    let MoveRequested::Started { move_id } =
        m.w.s.move_request(req, env(&["claude"])).await.unwrap()
    else {
        panic!("refused");
    };
    let row = m.row(&move_id).await;
    let check = |station_session: &str| Check {
        grant_id: authority.grant_id.clone(),
        generation: 1,
        caller_station: authority.caller_station.clone(),
        caller_session: "coord".into(),
        target_session: station_session.into(),
        command_id: row.in_command_id.clone().into(),
        launch: None,
    };
    let ask = |c: Check| m.w.s.authority_local(m.bravo.clone(), c);
    // Not `starting` yet: the command is unknown.
    assert_eq!(
        ask(check("new")).await.unwrap().unwrap_err(),
        "unknown_command"
    );
    m.w.s
        .move_advance(
            move_id.clone(),
            &[MoveState::Holding],
            MoveState::Starting,
            MovePatch::default(),
        )
        .await
        .unwrap();
    assert!(!ask(check("new")).await.unwrap().unwrap().target_excluded);
    // Another station, another authority and another session are unknown.
    assert_eq!(
        m.w.s
            .authority_local(m.w.st.clone(), check("new"))
            .await
            .unwrap()
            .unwrap_err(),
        "unknown_command"
    );
    let mut other = check("new");
    other.generation = 2;
    assert_eq!(ask(other).await.unwrap().unwrap_err(), "generation_changed");
    m.w.s
        .move_set_dst(move_id.clone(), "n1".to_owned())
        .await
        .unwrap();
    assert_eq!(
        ask(check("n2")).await.unwrap().unwrap_err(),
        "unknown_command"
    );
    assert!(!ask(check("n1")).await.unwrap().unwrap().target_excluded);
    // The gate: valid, then a revoked grant.
    assert_eq!(
        m.w.s.move_gate(move_id.clone()).await.unwrap(),
        MoveGate::Go
    );
    m.w.s
        .revoke_grant(stored.grant_id.clone(), "owner".to_owned())
        .await
        .unwrap();
    assert!(matches!(
        m.w.s.move_gate(move_id.clone()).await.unwrap(),
        MoveGate::Lost {
            code: "authority_lost",
            ..
        }
    ));
    assert_eq!(
        ask(check("n1")).await.unwrap().unwrap_err(),
        "grant_revoked"
    );
    // The expiry ends the gate.
    m.w.det.advance(std::time::Duration::from_millis(
        u64::try_from(AUTHORITY_TTL_MS).unwrap() + 1,
    ));
    assert!(matches!(
        m.w.s.move_gate(move_id).await.unwrap(),
        MoveGate::Lost {
            code: "authority_expired",
            ..
        }
    ));
}
