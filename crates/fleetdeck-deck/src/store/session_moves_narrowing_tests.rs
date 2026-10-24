#![allow(clippy::unwrap_used, clippy::expect_used)]

//! MCP P4b §7.1 and §7.2 (test 10, the land and move part): the land and
//! move fallbacks of the station's pre-execution check answer
//! `class_removed` when the grant lost a class of the item, and a narrowing
//! or a revoke fails the lands and moves that lost one. The outbox part is in
//! `fleet_control_narrowing_tests.rs`.

use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::fleet_git::LandState;
use fleetdeck_proto::mutation::{AuthorityCheck, CoordinatorBinding};
use rusqlite::params;

use super::*;
use crate::store::coordinator::grant_narrowing::{Removal, fail_removed_tx};
use crate::store::grants::{GrantClass, NewGrant};
use crate::store::land_queue::{LandRequest, LandRequested, LandRow};
use crate::store::land_world::session;

struct Rig {
    m: Moves,
    authority: CommandAuthority,
}

/// A fleet grant over both stations with `manage` and `inspect`, bound at
/// generation 1, in the move world; the sessions `s1` and `s6` can move, the
/// sessions `s2`, `s4` and `s5` can land.
async fn rig() -> Rig {
    let m = moves().await;
    m.w.s.set_fleet_control(true);
    m.show(vec![
        pushed("s1"),
        pushed("s6"),
        session("s2", "/work/app", SessionState::Exited),
        session("s4", "/work/app", SessionState::Exited),
        session("s5", "/work/app", SessionState::Exited),
    ])
    .await;
    let mut grant = NewGrant::new(m.w.st.clone(), "coord".into());
    grant.classes = BTreeSet::from([GrantClass::Manage, GrantClass::Inspect]);
    grant.stations = BTreeSet::from([m.w.st.clone(), m.bravo.clone()]);
    grant.scope = GrantScope::Fleet;
    let stored = m.w.s.create_grant(grant).await.unwrap();
    let binding = CoordinatorBinding {
        binding_id: "b".to_owned(),
        grant_id: stored.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "n-coord".to_owned(),
    };
    assert!(m.w.s.bind_grant(binding).await.unwrap());
    let authority = CommandAuthority {
        grant_id: stored.grant_id,
        caller_station: m.w.st.clone(),
        caller_session: "coord".into(),
        generation: 1,
    };
    Rig { m, authority }
}

impl Rig {
    fn grant_id(&self) -> String {
        self.authority.grant_id.clone()
    }

    /// A coordinator land of `session` that needs `required`.
    async fn land(&self, session: &str, required: &[GrantClass]) -> LandRow {
        let asked = self
            .m
            .w
            .s
            .land_request(LandRequest {
                requested_by: format!("coordinator:{}", self.authority.grant_id),
                request_command_id: format!("k-{session}"),
                station_id: self.m.w.st.0.clone(),
                session_id: session.to_owned(),
                authority: Some(self.authority.clone()),
                required_classes: Some(required.iter().copied().collect()),
            })
            .await
            .unwrap();
        let LandRequested::Queued { land_id, .. } = asked else {
            panic!("{asked:?}");
        };
        self.m.w.row(&land_id).await
    }

    /// A coordinator move of `session` that needs `required`.
    async fn mv(&self, session: &str, required: &[GrantClass]) -> MoveRow {
        let mut req = self.m.request(&format!("k-{session}"), session);
        req.requested_by = format!("coordinator:{}", self.authority.grant_id);
        req.authority = Some(self.authority.clone());
        req.required_classes = Some(required.iter().copied().collect());
        let started = self
            .m
            .w
            .s
            .move_request(req, env(&["claude"]))
            .await
            .unwrap();
        let MoveRequested::Started { move_id } = started else {
            panic!("{started:?}");
        };
        self.m.row(&move_id).await
    }

    /// The station sends the land: the row is `running` and its outbox row
    /// has `attempts` sends.
    async fn run(&self, land: &LandRow, attempts: i64) {
        let id = land.land_id.clone();
        self.m
            .w
            .s
            .db()
            .call(move |conn| {
                conn.execute(
                    "UPDATE land_queue SET state = 'running', started_at = 1 WHERE land_id = ?1",
                    [&id],
                )?;
                conn.execute(
                    "INSERT INTO land_outbox (command_id, land_id, station_id, payload, state,
                         attempts, next_at, updated_at)
                     SELECT command_id, land_id, station_id, '{}', 'pending', ?2, 0, 1
                     FROM land_queue WHERE land_id = ?1",
                    params![id, attempts],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn set_attempts(&self, land: &LandRow, attempts: i64) {
        let id = land.land_id.clone();
        self.m
            .w
            .s
            .db()
            .call(move |conn| {
                conn.execute(
                    "UPDATE land_outbox SET attempts = ?2 WHERE land_id = ?1",
                    params![id, attempts],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Writes `tokens.classes` of the grant (the change route is P4b-3b).
    async fn set_classes(&self, classes: &[GrantClass]) {
        let text =
            serde_json::to_string(&classes.iter().copied().collect::<BTreeSet<_>>()).unwrap();
        let id = self.grant_id();
        self.m
            .w
            .s
            .db()
            .call(move |conn| {
                conn.execute(
                    "UPDATE tokens SET classes = ?2 WHERE grant_id = ?1",
                    params![id, text],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn fail_removed(&self, classes: &[GrantClass]) {
        let id = self.grant_id();
        let classes: BTreeSet<GrantClass> = classes.iter().copied().collect();
        let now = self.m.w.s.now_ms();
        crate::store::call(&self.m.w.s.db(), move |conn| {
            let tx = conn.transaction()?;
            fail_removed_tx(&tx, &id, &classes, Removal::Narrowed, now)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    }

    /// The deck's answer to the station's check of `command` at `station`.
    async fn verdict(
        &self,
        station: &StationId,
        command: &str,
        target: &str,
    ) -> std::result::Result<(), &'static str> {
        let check = AuthorityCheck {
            grant_id: self.grant_id(),
            generation: 1,
            caller_station: self.authority.caller_station.clone(),
            caller_session: "coord".into(),
            target_session: target.into(),
            command_id: command.into(),
            launch: None,
        };
        let local = self
            .m
            .w
            .s
            .authority_local(station.clone(), check)
            .await
            .unwrap()?;
        assert!(!local.target_excluded);
        Ok(())
    }

    async fn land_row(&self, land: &LandRow) -> LandRow {
        self.m.w.row(&land.land_id).await
    }

    async fn move_row(&self, mv: &MoveRow) -> MoveRow {
        self.m.row(&mv.move_id).await
    }
}

fn ended(row: &LandRow, state: LandState, code: &str) -> bool {
    row.state == crate::store::land_queue::state_name(state) && row.code.as_deref() == Some(code)
}

/// MCP P4b test 10, the land and move part.
#[tokio::test]
async fn a_narrowing_or_a_revoke_stops_fleet_commands_before_execution() {
    let r = rig().await;
    let (st, bravo) = (r.m.w.st.clone(), r.m.bravo.clone());
    // A move that needs `manage` and one that needs `inspect`, a land that
    // needs `manage` (sent once), one that needs `inspect`, and an owner land.
    let move_a = r.mv("s1", &[GrantClass::Manage]).await;
    let move_b = r.mv("s6", &[GrantClass::Inspect]).await;
    assert_eq!(move_a.required_classes.as_deref(), Some(r#"["manage"]"#));
    r.m.w
        .s
        .move_advance(
            move_a.move_id.clone(),
            &[MoveState::Holding],
            MoveState::Starting,
            MovePatch::default(),
        )
        .await
        .unwrap();
    let land_a = r.land("s2", &[GrantClass::Manage]).await;
    let land_b = r.land("s4", &[GrantClass::Inspect]).await;
    let owner = r.m.w.queued("owner-1", "s5").await;
    assert_eq!(land_a.required_classes.as_deref(), Some(r#"["manage"]"#));
    assert_eq!(r.m.w.row(&owner).await.required_classes, None);
    r.run(&land_a, 1).await;

    // Before a narrowing the three fallbacks answer: the land, the stop of
    // the source (`MoveOut`) and the start on the target (`MoveIn`).
    let ask_all = || async {
        (
            r.verdict(&st, &land_a.command_id, "s2").await,
            r.verdict(&st, &move_a.out_command_id, "s1").await,
            r.verdict(&bravo, &move_a.in_command_id, "n1").await,
        )
    };
    assert_eq!(ask_all().await, (Ok(()), Ok(()), Ok(())));
    // `manage` goes while `inspect` stays.
    r.set_classes(&[GrantClass::Inspect]).await;
    let gone = Err("class_removed");
    assert_eq!(ask_all().await, (gone, gone, gone));
    // Back with `manage`: the same commands pass again.
    r.set_classes(&[GrantClass::Manage, GrantClass::Inspect])
        .await;
    assert_eq!(ask_all().await, (Ok(()), Ok(()), Ok(())));
    // An item admitted before V46 (no set) is checked as today.
    r.set_classes(&[GrantClass::Inspect]).await;
    let id = move_a.move_id.clone();
    r.m.w
        .s
        .db()
        .call(move |conn| {
            conn.execute(
                "UPDATE session_moves SET required_classes = NULL WHERE move_id = ?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        r.verdict(&st, &move_a.out_command_id, "s1").await,
        Ok(()),
        "a move without a set"
    );
    let id = move_a.move_id.clone();
    r.m.w
        .s
        .db()
        .call(move |conn| {
            conn.execute(
                "UPDATE session_moves SET required_classes = '[\"manage\"]' WHERE move_id = ?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // `fail_removed_tx` with `manage` gone: the move before its commit point
    // aborts. The land was sent already and keeps running; the items whose
    // classes are intact, and the owner land, are untouched.
    r.fail_removed(&[GrantClass::Inspect]).await;
    let aborted = r.move_row(&move_a).await;
    assert_eq!(aborted.state, MoveState::Aborting);
    assert_eq!(aborted.code.as_deref(), Some("authority_removed"));
    assert_eq!(r.land_row(&land_a).await.state, "running");
    assert_eq!(r.move_row(&move_b).await.state, MoveState::Holding);
    assert_eq!(r.land_row(&land_b).await.state, "queued");
    assert_eq!(r.m.w.row(&owner).await.state, "queued");

    // A land that no send reached yet fails too; the running row's outbox
    // row is rejected with it.
    r.set_attempts(&land_a, 0).await;
    r.fail_removed(&[GrantClass::Inspect]).await;
    assert!(ended(
        &r.land_row(&land_a).await,
        LandState::Failed,
        "authority_removed"
    ));
    assert_eq!(r.land_row(&land_b).await.state, "queued");
    assert_eq!(r.m.w.row(&owner).await.state, "queued");

    // A revoke allows nothing: the queued land and the move that still hold
    // their classes fail now; the owner land stays.
    assert!(
        r.m.w
            .s
            .revoke_grant(r.grant_id(), "owner".to_owned())
            .await
            .unwrap()
    );
    let row = r.land_row(&land_b).await;
    assert!(ended(&row, LandState::Failed, "authority_lost"));
    assert_eq!(
        row.message.as_deref(),
        Some(
            "The coordinator lost its permission during the land (grant_revoked). Nothing was pushed."
        )
    );
    let aborted = r.move_row(&move_b).await;
    assert_eq!(aborted.state, MoveState::Aborting);
    assert_eq!(aborted.code.as_deref(), Some("authority_lost"));
    assert_eq!(
        aborted.message.as_deref(),
        Some("The coordinator lost its permission during the move (grant_revoked).")
    );
    assert_eq!(r.m.w.row(&owner).await.state, "queued");
}
