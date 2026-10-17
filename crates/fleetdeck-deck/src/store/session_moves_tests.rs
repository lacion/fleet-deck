#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The session moves on the deck store (design §12.2, §12.3, §12.4A; FG-9,
//! T4.4b): the migration, the request and its conditions, the transitions,
//! the event hook, the restart, the undo request and the authority lookup.

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::event::{Event, EventBody, ProviderKind, SessionState};
use fleetdeck_proto::fleet_git::{MoveInInfo, MoveInState, PushInfo, PushState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{EventEnvelope, SessionSummary, StationId};

use super::super::land_world::{World, session, world};
use super::super::schema::migrations;
use super::super::stations::NewStation;
use super::*;
use crate::store::LinkFeature;
use crate::store::session_moves_steps::{MovePatch, backoff_ms};

const OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A pushed Claude session of the project in `/work/app`.
fn pushed(id: &str) -> SessionSummary {
    let mut s = session(id, "/work/app", SessionState::Exited);
    s.provider = Some(ProviderKind::Claude);
    s.provider_instance = Some("claude".into());
    s.push = Some(PushInfo {
        state: PushState::Pushed,
        ref_name: format!("fd/alpha/{id}"),
        oid: Some(OID.to_owned()),
        desired_oid: None,
        stop_code: None,
        attempts: 0,
        next_attempt_at: None,
        code: None,
        text: None,
    });
    s
}

struct Moves {
    w: World,
    bravo: StationId,
}

/// The land world with the move feature on `alpha`, and a station `bravo`
/// with a ready home of the project and the move feature.
async fn moves() -> Moves {
    let w = world(&["/work/app"]).await;
    let bravo =
        w.s.pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "bravo".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    w.s.clear_link_features(&w.st, 1);
    for st in [&w.st, &bravo] {
        w.s.set_link_features(
            st,
            1,
            vec![
                feature::FLEET_GIT_MOVE.to_owned(),
                feature::FLEET_GIT_LAND.to_owned(),
                feature::COORDINATOR_V2.to_owned(),
            ],
        );
    }
    let (project, b) = (w.projects[0].0.clone(), bravo.0.clone());
    w.s.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO home_generations (project_id, station_id, last_generation)
                 VALUES (?1, ?2, 1)",
                [&project, &b],
            )?;
            conn.execute(
                "INSERT INTO project_homes (project_id, station_id, generation, position, state,
                     path, canonical_path, sync_owner, created_at, updated_at)
                 VALUES (?1, ?2, 1, 1, 'ready', '/srv/app', '/srv/app', 1, 1, 1)",
                [&project, &b],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    w.s.apply_snapshot(bravo.clone(), "inc-b".into(), 0, vec![])
        .await
        .unwrap();
    Moves { w, bravo }
}

fn env(instances: &[&str]) -> MoveEnv {
    MoveEnv {
        src: LinkFeature::Negotiated,
        dst: LinkFeature::Negotiated,
        dst_instances: Some(instances.iter().map(|s| (*s).to_owned()).collect()),
    }
}

impl Moves {
    fn request(&self, command: &str, session: &str) -> MoveRequest {
        MoveRequest {
            requested_by: "owner".to_owned(),
            request_command_id: command.to_owned(),
            src_station: self.w.st.0.clone(),
            src_session: session.to_owned(),
            dst_station: self.bravo.0.clone(),
            authority: None,
        }
    }

    async fn ask(&self, command: &str, session: &str) -> MoveRequested {
        self.w
            .s
            .move_request(self.request(command, session), env(&["claude"]))
            .await
            .unwrap()
    }

    async fn started(&self, command: &str, session: &str) -> String {
        match self.ask(command, session).await {
            MoveRequested::Started { move_id } => move_id,
            MoveRequested::Refused(r) => panic!("refused: {r:?}"),
        }
    }

    async fn row(&self, id: &str) -> MoveRow {
        self.w.s.move_row(id.to_owned()).await.unwrap().unwrap()
    }

    async fn show(&self, sessions: Vec<SessionSummary>) {
        self.w.show(sessions).await;
    }
}

fn refused(r: MoveRequested) -> (&'static str, String) {
    match r {
        MoveRequested::Refused(r) => (r.code, r.message),
        MoveRequested::Started { move_id } => panic!("started {move_id}"),
    }
}

// ----- the migration -----

#[test]
fn the_open_index_allows_one_open_move_for_each_source_session() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    migrations().to_latest(&mut conn).unwrap();
    let insert = |id: &str, session: &str, state: &str| {
        conn.execute(
            "INSERT INTO session_moves (move_id, request_command_id, src_station, src_session,
                 dst_station, kind, state, out_command_id, in_command_id, finish_command_id,
                 requested_by, created_at)
             VALUES (?1, ?1, 'a', ?2, 'b', 'native', ?3, 'o-'||?1, 'i-'||?1, 'f-'||?1, 'owner', 1)",
            rusqlite::params![id, session, state],
        )
    };
    insert("m1", "s1", "holding").unwrap();
    assert!(insert("m2", "s1", "starting").is_err());
    // An ended move, and another session, do not count.
    insert("m3", "s1", "failed").unwrap();
    insert("m4", "s1", "undone").unwrap();
    insert("m5", "s2", "resume_failed").unwrap();
    // A command id of a station is unique.
    assert!(
        conn.execute(
            "INSERT INTO session_moves (move_id, request_command_id, src_station, src_session,
                 dst_station, kind, state, out_command_id, in_command_id, finish_command_id,
                 requested_by, created_at)
             VALUES ('m6', 'x', 'a', 's9', 'b', 'native', 'done', 'o-m1', 'i-m6', 'f-m6',
                     'owner', 1)",
            [],
        )
        .is_err()
    );
}

// ----- the request -----

#[tokio::test]
async fn a_request_starts_a_holding_move_and_the_same_command_id_gives_the_first_answer() {
    let m = moves().await;
    m.show(vec![pushed("s1"), pushed("s2")]).await;
    let first = m.started("c1", "s1").await;
    assert_eq!(m.started("c1", "s1").await, first);
    let row = m.row(&first).await;
    assert_eq!(row.state, MoveState::Holding);
    assert_eq!(row.kind, "native");
    assert_eq!(row.dst_station, m.bravo.0);
    assert!(row.out_command_id.starts_with("moveout-"));
    assert!(row.in_command_id.starts_with("movein-"));
    assert!(row.finish_command_id.starts_with("movefinish-"));
    assert_eq!(row.authority, None);
    // The same command id for another session is not the same request.
    assert_eq!(refused(m.ask("c1", "s2").await).0, "command_id_reused");
    // The session has an open move.
    let (code, text) = refused(m.ask("c2", "s1").await);
    assert_eq!(
        (code, text.as_str()),
        ("move_not_ready", "The session is already moving.")
    );
}

#[tokio::test]
async fn a_non_claude_session_moves_as_branch_only_and_a_coordinator_move_keeps_its_authority() {
    let m = moves().await;
    let mut other = pushed("s1");
    other.provider = Some(ProviderKind::Unknown);
    m.show(vec![other]).await;
    let mut req = m.request("c1", "s1");
    req.requested_by = "coordinator:g1".to_owned();
    req.authority = Some(CommandAuthority {
        grant_id: "g1".to_owned(),
        caller_station: m.w.st.clone(),
        caller_session: "coord".into(),
        generation: 1,
    });
    let MoveRequested::Started { move_id } =
        m.w.s.move_request(req, env(&["claude"])).await.unwrap()
    else {
        panic!("refused");
    };
    let row = m.row(&move_id).await;
    assert_eq!(row.kind, "branch_only");
    assert_eq!(row.authority.as_ref().map(|a| a.generation), Some(1));
    assert_eq!(
        row.authority_expires_at,
        Some(m.w.s.now_ms() + AUTHORITY_TTL_MS)
    );
}

#[tokio::test]
async fn the_conditions_refuse_in_the_order_of_the_design_and_a_refusal_writes_nothing() {
    let m = moves().await;
    async fn ask(m: &Moves, session: &str) -> (&'static str, String) {
        refused(m.ask("cx", session).await)
    }
    // Unknown session and station.
    assert_eq!(ask(&m, "nope").await.0, "unknown_session");
    // 1. A turn is running, an approval is open.
    let mut running = pushed("s1");
    running.state = SessionState::Running;
    running.current_run = Some("r1".into());
    let mut approval = pushed("s2");
    approval.open_requests = vec![fleetdeck_proto::session::OpenRequest {
        request_id: "q1".into(),
        node_id: None,
        request_kind: fleetdeck_proto::event::RequestKind::Tool,
        title: "t".to_owned(),
        details: "d".to_owned(),
        input: None,
        options: Vec::new(),
        response: fleetdeck_proto::event::ResponseChannel::RpcReply,
        requested_at: 1,
    }];
    // 2. Not pushed, and not in a synced project.
    let mut unpushed = pushed("s3");
    unpushed.push.as_mut().unwrap().state = PushState::Due;
    let mut newer = pushed("s4");
    newer.push.as_mut().unwrap().desired_oid = Some("b".repeat(40));
    let mut nopush = pushed("s5");
    nopush.push = None;
    // 5. Arrived by a move and not confirmed.
    let mut arrived = pushed("s6");
    arrived.move_in = Some(MoveInInfo {
        move_id: "m0".to_owned(),
        state: MoveInState::Released,
        code: None,
        text: None,
    });
    // 4. Already moved, a land holds it.
    let mut moved = pushed("s7");
    moved.moved_to = Some(fleetdeck_proto::fleet_git::MovedTo {
        move_id: "m0".to_owned(),
        station_id: m.bravo.clone(),
        session_id: "x".into(),
    });
    m.show(vec![
        running,
        approval,
        unpushed,
        newer,
        nopush,
        arrived,
        moved,
        pushed("s8"),
    ])
    .await;
    let text = |t: &str| ("move_not_ready", t.to_owned());
    assert_eq!(ask(&m, "s1").await, text("A turn is running."));
    assert_eq!(ask(&m, "s2").await, text("An approval is open."));
    assert_eq!(ask(&m, "s3").await, text("The branch is not pushed."));
    assert_eq!(ask(&m, "s4").await, text("The branch is not pushed."));
    assert_eq!(
        ask(&m, "s5").await,
        text("This session is not in a synced project.")
    );
    assert_eq!(
        ask(&m, "s6").await,
        text("The session has not confirmed its conversation yet. Send it one prompt first.")
    );
    assert_eq!(ask(&m, "s7").await, text("The session is already moving."));
    // A land holds the session.
    let land = m.w.queued("l1", "s8").await;
    assert!(!land.is_empty());
    assert_eq!(ask(&m, "s8").await, text("The session is being landed."));
    let n = m.w.s.move_open().await.unwrap().len();
    assert_eq!(n, 0, "a refusal writes nothing");
}

#[tokio::test]
async fn the_target_conditions_name_the_station_and_the_link() {
    let m = moves().await;
    m.show(vec![pushed("s1")]).await;
    let ask = |env: MoveEnv, dst: String| {
        let mut req = m.request("c9", "s1");
        req.dst_station = dst;
        let store = m.w.s.clone();
        async move { refused(store.move_request(req, env).await.unwrap()) }
    };
    // The target is the source.
    let (code, text) = ask(env(&["claude"]), m.w.st.0.clone()).await;
    assert_eq!(
        (code, text.as_str()),
        ("move_not_ready", "The session is already on this station.")
    );
    // A target that has no provider of the instance.
    let (code, text) = ask(env(&["codex"]), m.bravo.0.clone()).await;
    assert_eq!(
        (code, text.as_str()),
        ("move_not_ready", "bravo has no provider named claude.")
    );
    // A link that is down or without the feature.
    let mut down = env(&["claude"]);
    down.dst = LinkFeature::Down;
    assert_eq!(ask(down, m.bravo.0.clone()).await.1, "bravo is offline.");
    let mut without = env(&["claude"]);
    without.src = LinkFeature::Without;
    assert_eq!(
        ask(without, m.bravo.0.clone()).await.0,
        "fleet_git_unsupported"
    );
    // An unknown station.
    assert_eq!(
        ask(env(&["claude"]), "ghost".to_owned()).await.0,
        "unknown_station"
    );
    // A target without a home of the project.
    let carol =
        m.w.s
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: "carol".to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap()
            .station_id;
    let (code, text) = ask(env(&["claude"]), carol.0.clone()).await;
    assert_eq!(
        (code, text.as_str()),
        ("move_not_ready", "carol has no home of this project.")
    );
    // A target whose home is not verified.
    let b = m.bravo.0.clone();
    m.w.s
        .db()
        .call(move |conn| {
            conn.execute(
                "UPDATE project_homes SET state = 'unverified' WHERE station_id = ?1",
                [b],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        ask(env(&["claude"]), m.bravo.0.clone()).await.0,
        "home_unverified"
    );
}

#[tokio::test]
async fn the_dialog_lists_the_stations_with_a_home_and_the_first_failed_condition() {
    let m = moves().await;
    let mut running = pushed("s1");
    running.state = SessionState::Running;
    running.current_turn = Some("t1".into());
    m.show(vec![running, pushed("s2")]).await;
    let envs = vec![
        (m.w.st.0.clone(), env(&["claude"])),
        (m.bravo.0.clone(), env(&["claude"])),
    ];
    let targets =
        m.w.s
            .move_targets(m.w.st.0.clone(), "s1".to_owned(), envs.clone())
            .await
            .unwrap();
    assert_eq!(targets.len(), 1, "the source is not a target");
    assert!(!targets[0].ready);
    assert_eq!(
        targets[0].failed.as_ref().unwrap().text,
        "A turn is running."
    );
    let targets =
        m.w.s
            .move_targets(m.w.st.0.clone(), "s2".to_owned(), envs)
            .await
            .unwrap();
    assert!(targets[0].ready && targets[0].failed.is_none());
    assert_eq!(
        (targets[0].name.as_str(), targets[0].kind.as_str()),
        ("bravo", "native")
    );
}

// ----- the transitions -----

#[tokio::test]
async fn a_transition_needs_the_expected_state_and_resets_the_step() {
    let m = moves().await;
    m.show(vec![pushed("s1")]).await;
    let id = m.started("c1", "s1").await;
    let s = &m.w.s;
    s.move_sent(id.clone(), 1000).await.unwrap();
    let row = m.row(&id).await;
    assert_eq!((row.step_attempts, row.next_step_at.is_some()), (1, true));
    // The wrong expected state changes nothing.
    assert!(
        !s.move_advance(
            id.clone(),
            &[MoveState::Starting],
            MoveState::Committed,
            MovePatch::default()
        )
        .await
        .unwrap()
    );
    let patch = MovePatch {
        kind: Some("branch_only".to_owned()),
        bytes: Some(7),
        ..MovePatch::note("c", "t")
    };
    assert!(
        s.move_advance(
            id.clone(),
            &[MoveState::Holding],
            MoveState::Exporting,
            patch
        )
        .await
        .unwrap()
    );
    let row = m.row(&id).await;
    assert_eq!(row.state, MoveState::Exporting);
    assert_eq!((row.step_attempts, row.next_step_at), (0, None));
    assert_eq!((row.kind.as_str(), row.bytes), ("branch_only", Some(7)));
    assert_eq!(row.code.as_deref(), Some("c"));
    // The commit and the end set their times.
    s.move_advance(
        id.clone(),
        &[MoveState::Exporting],
        MoveState::Starting,
        MovePatch::default(),
    )
    .await
    .unwrap();
    s.move_advance(
        id.clone(),
        &[MoveState::Starting],
        MoveState::Committed,
        MovePatch::clear(),
    )
    .await
    .unwrap();
    let row = m.row(&id).await;
    assert!(row.committed_at.is_some() && row.code.is_none());
    s.move_advance(
        id.clone(),
        &[MoveState::Committed],
        MoveState::Done,
        MovePatch::default(),
    )
    .await
    .unwrap();
    assert!(m.row(&id).await.ended_at.is_some());
    // A done move frees the source session for another move.
    m.started("c2", "s1").await;
}

#[tokio::test]
async fn the_backoff_doubles_up_to_sixty_units() {
    assert_eq!(
        (1..=8).map(|n| backoff_ms(n, 1000)).collect::<Vec<_>>(),
        [2000, 4000, 8000, 16000, 32000, 60000, 60000, 60000]
    );
}

// ----- the event hook -----

fn move_in(
    seq: u64,
    session: &str,
    move_id: &str,
    state: MoveInState,
    code: Option<&str>,
) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::SessionMoveIn(MoveInInfo {
            move_id: move_id.to_owned(),
            state,
            code: code.map(str::to_owned),
            text: code.map(|_| "the text".to_owned()),
        })),
    }
}

async fn fold(m: &Moves, station: &StationId, inc: &str, events: Vec<EventEnvelope>) {
    m.w.s
        .apply_events(station.clone(), inc.into(), events)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_move_in_event_commits_or_aborts_a_starting_move_and_a_failed_resume_reopens_a_done_move()
{
    let m = moves().await;
    m.show(vec![pushed("s1"), pushed("s2")]).await;
    let a = m.started("c1", "s1").await;
    let b = m.started("c2", "s2").await;
    for id in [&a, &b] {
        m.w.s
            .move_advance(
                id.clone(),
                &[MoveState::Holding],
                MoveState::Starting,
                MovePatch::default(),
            )
            .await
            .unwrap();
    }
    // Another station's event, and an event of a session in the wrong state.
    fold(
        &m,
        &m.w.st,
        "inc",
        vec![move_in(1, "n1", &a, MoveInState::Resumed, None)],
    )
    .await;
    assert_eq!(m.row(&a).await.state, MoveState::Starting);
    fold(
        &m,
        &m.bravo,
        "inc-b",
        vec![move_in(1, "n1", &a, MoveInState::Launching, None)],
    )
    .await;
    assert_eq!(m.row(&a).await.state, MoveState::Starting);
    // Resumed: the commit point. The row learns the new session.
    fold(
        &m,
        &m.bravo,
        "inc-b",
        vec![move_in(2, "n1", &a, MoveInState::Resumed, None)],
    )
    .await;
    let row = m.row(&a).await;
    assert_eq!(row.state, MoveState::Committed);
    assert_eq!(row.dst_session.as_deref(), Some("n1"));
    // Abandoned before the commit: the move aborts with the code and text.
    fold(
        &m,
        &m.bravo,
        "inc-b",
        vec![move_in(
            3,
            "n2",
            &b,
            MoveInState::Abandoned,
            Some("identity_changed"),
        )],
    )
    .await;
    let row = m.row(&b).await;
    assert_eq!(row.state, MoveState::Aborting);
    assert_eq!(
        (row.code.as_deref(), row.message.as_deref()),
        (Some("identity_changed"), Some("the text"))
    );
    // A failed resume after the release reopens a done move.
    for state in [MoveState::Finishing, MoveState::Releasing, MoveState::Done] {
        m.w.s
            .move_advance(
                a.clone(),
                &[
                    MoveState::Committed,
                    MoveState::Finishing,
                    MoveState::Releasing,
                ],
                state,
                MovePatch::default(),
            )
            .await
            .unwrap();
    }
    assert_eq!(m.row(&a).await.state, MoveState::Done);
    fold(
        &m,
        &m.bravo,
        "inc-b",
        vec![
            move_in(4, "n1", &a, MoveInState::Confirmed, None),
            move_in(
                5,
                "n1",
                &a,
                MoveInState::ResumeFailed,
                Some("move_resume_failed"),
            ),
        ],
    )
    .await;
    let row = m.row(&a).await;
    assert_eq!(row.state, MoveState::ResumeFailed);
    assert_eq!(row.ended_at, None);
    assert_eq!(row.code.as_deref(), Some("move_resume_failed"));
}

#[path = "session_moves_more_tests.rs"]
mod more;
