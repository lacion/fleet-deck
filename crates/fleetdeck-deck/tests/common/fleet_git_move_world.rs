//! The world of the session move tests (FG-9, T4.4b): a synced project with a
//! ready home on two stations that answer from [`crate::move_fake`], a pushed
//! session on the first, and a pump for the manual clock of the store.
//! Include it with `#[path = "common/fleet_git_move_world.rs"] mod world;`
//! after the fake.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_deck::fleet_git_move;
use fleetdeck_deck::fleet_git_move::MoveOptions;
use fleetdeck_deck::store::session_moves::{MoveRow, MoveState};
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::fleet_git::{PushInfo, PushState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{SessionSummary, StationId};
use serde_json::json;

use crate::common::T;
use crate::common::board::Http;
use crate::common::fleet_git_fake::{REMOTE, Setup, setup_scopes, station_scopes};
use crate::move_fake::{Log, MoveFake, OID, options};

/// A pushed Claude session of the project in `/work/app`.
pub fn pushed(id: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.state = SessionState::Exited;
    s.cwd = Some("/work/app".to_owned());
    s.worktree = Some(format!("/work/app--fd-{id}"));
    s.branch = Some(format!("fd/alpha/{id}"));
    s.callsign = Some(id.to_owned());
    s.title = Some(format!("Work of {id}"));
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

/// A synced project with a ready home on `fake` (the source) and on `bravo`
/// (the target), the session `s1` on `fake`, and the clock pump.
pub struct World {
    pub s: Setup,
    pub a: Arc<MoveFake>,
    pub b: Arc<MoveFake>,
    pub log: Log,
    pub bravo: StationId,
    stop: tokio::sync::watch::Sender<bool>,
    driving: bool,
}

impl World {
    /// The world; `drive` starts the move task at once.
    pub async fn new(drive: bool) -> Self {
        Self::with(station_scopes(), drive).await
    }

    pub async fn with(scopes: BTreeSet<Scope>, drive: bool) -> Self {
        let mut s = setup_scopes(scopes).await;
        let r = s.save(json!({ "sync": true }), Some(REMOTE)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let log = Log::default();
        let a = MoveFake::connect_first(&mut s.f, "fake", &log).await;
        s.home("ready").await;
        let b = MoveFake::connect_second(&mut s.f, "bravo", &log).await;
        let (project, bravo) = (s.app.project_id.clone(), b.station.0.clone());
        s.f.store
            .db()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO home_generations (project_id, station_id, last_generation)
                     VALUES (?1, ?2, 1)",
                    [&project, &bravo],
                )?;
                conn.execute(
                    "INSERT INTO project_homes (project_id, station_id, generation, position,
                         state, path, canonical_path, sync_owner, created_at, updated_at)
                     VALUES (?1, ?2, 1, 1, 'ready', '/srv/app', '/srv/app', 1, 1, 1)",
                    [&project, &bravo],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        s.f.store
            .apply_snapshot(s.f.station.clone(), s.f.inc.clone(), 0, vec![pushed("s1")])
            .await
            .unwrap();
        // The clock of the store is manual: a pump moves it.
        let det = s.f.det.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                det.advance(Duration::from_millis(100));
            }
        });
        let (stop, _) = tokio::sync::watch::channel(false);
        let mut w = Self {
            bravo: b.station.clone(),
            s,
            a,
            b,
            log,
            stop,
            driving: false,
        };
        if drive {
            w.drive();
        }
        w
    }

    /// Starts the move task with the options of the tests.
    pub fn drive(&mut self) {
        self.drive_with(options());
    }

    pub fn drive_with(&mut self, opts: MoveOptions) {
        assert!(!self.driving);
        self.driving = true;
        let f = &self.s.f;
        tokio::spawn(fleet_git_move::run(
            f.store.clone(),
            Arc::clone(&f.ctx),
            Arc::clone(&f.ctx.move_kick),
            opts,
            self.stop.subscribe(),
        ));
    }

    pub async fn show(&self, sessions: Vec<SessionSummary>) {
        let f = &self.s.f;
        f.store
            .apply_snapshot(f.station.clone(), f.inc.clone(), 0, sessions)
            .await
            .unwrap();
    }

    pub fn path(&self, session: &str) -> String {
        format!("/api/sessions/fake/{session}/move")
    }

    /// `POST .../move` of `session` to `bravo` with the command id `id`.
    pub async fn post_move(&self, session: &str, id: &str) -> Http {
        self.s
            .c
            .post(
                &self.path(session),
                &self.s.h(),
                json!({ "to_station": "bravo", "command_id": id }),
            )
            .await
    }

    /// Starts a move of `s1` and returns its id.
    pub async fn start(&self) -> String {
        let r = self.post_move("s1", "c1").await;
        assert_eq!(r.status, 202, "{}", r.body);
        r.body["move_id"].as_str().unwrap().to_owned()
    }

    pub async fn row(&self, id: &str) -> MoveRow {
        self.s
            .f
            .store
            .move_row(id.to_owned())
            .await
            .unwrap()
            .unwrap()
    }

    /// Waits until the move is in `state`.
    pub async fn wait_state(&self, id: &str, state: MoveState) -> MoveRow {
        let deadline = tokio::time::Instant::now() + T;
        loop {
            let row = self.row(id).await;
            if row.state == state {
                return row;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: {state:?}; the row is {:?} ({:?} {:?}); the log is {:?}",
                row.state,
                row.code,
                row.message,
                self.log.all()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
