//! The shared setup of the land queue tests (FG-8).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::event::{Event, EventBody, SessionState};
use fleetdeck_proto::fleet_git::{
    GitSettings, GitSettingsPut, HomeCheck, HomeCheckState, LandInfo, LandPhase, LandState,
    SnapshotAck,
};
use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind, ProjectInput};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{EventEnvelope, SessionSummary, StationId};
use fleetdeck_testkit::Det;

use super::land_queue::{LandRequest, LandRequested, LandRow};

use super::stations::NewStation;
use crate::store::DeckStore;

pub(super) const REMOTE: &str = "https://github.com/acme/app.git";

pub(super) struct World {
    pub(super) s: DeckStore,
    pub(super) det: Det,
    pub(super) st: StationId,
    /// `(project id, folder)` of each project.
    pub(super) projects: Vec<(String, String)>,
}

pub(super) fn input(name: &str, path: &str) -> ProjectInput {
    ProjectInput {
        name: name.into(),
        station: "x".into(),
        path: path.into(),
        forge: Some(ForgeBinding {
            kind: ForgeKind::Github,
            host: "github.com".into(),
            repo: "acme/app".into(),
        }),
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
    }
}

/// A store with one station that negotiated `fleet_git.land.v1`, and one
/// verified sync project `App` in `/work/app` for each of `folders`.
pub(super) async fn world(folders: &[&str]) -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let rec = s
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "alpha".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    let st = rec.station_id;
    s.set_link_features(&st, 1, vec![feature::FLEET_GIT_LAND.to_owned()]);
    let mut projects = Vec::new();
    for (i, folder) in folders.iter().enumerate() {
        let p = s
            .create_project(st.0.clone(), input(&format!("App{i}"), folder))
            .await
            .unwrap();
        let put = GitSettingsPut {
            settings: GitSettings {
                sync: true,
                ..GitSettings::default()
            },
            revision: 0,
            remote_url: Some(REMOTE.to_owned()),
        };
        s.set_project_git(p.project_id.clone(), put).await.unwrap();
        projects.push((p.project_id, (*folder).to_owned()));
    }
    let sent = s.git_snapshot_next(&st, true).await.unwrap().unwrap();
    let checks = projects
        .iter()
        .map(|(id, folder)| HomeCheck {
            project_id: id.clone(),
            generation: 1,
            state: HomeCheckState::Verified,
            canonical_path: Some(folder.clone()),
            seen: None,
            code: None,
        })
        .collect();
    let ack = SnapshotAck {
        stored: true,
        revision: sent.revision,
        checks,
    };
    s.git_snapshot_acked(&st, sent, ack).await.unwrap();
    s.db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
                 SELECT p.repo, 'alpha', h.station_id, 1 FROM projects p
                   JOIN project_homes h ON h.project_id = p.project_id LIMIT 1",
                [],
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    World {
        s,
        det,
        st,
        projects,
    }
}

/// An ended session on `folder` with a Fleet Git branch.
pub(super) fn session(id: &str, folder: &str, state: SessionState) -> SessionSummary {
    let mut summary = SessionSummary::new(id.into());
    summary.state = state;
    summary.cwd = Some(folder.to_owned());
    summary.worktree = Some(format!("{folder}--fd-{id}"));
    summary.branch = Some(format!("fd/alpha/{id}"));
    summary.callsign = Some(id.to_owned());
    summary.title = Some(format!("Work of {id}"));
    summary
}

impl World {
    pub(super) async fn show(&self, sessions: Vec<SessionSummary>) {
        self.s
            .apply_snapshot(self.st.clone(), "inc".into(), 0, sessions)
            .await
            .unwrap();
    }

    pub(super) async fn ask(&self, command: &str, session: &str) -> LandRequested {
        self.s
            .land_request(LandRequest {
                requested_by: "owner".to_owned(),
                request_command_id: command.to_owned(),
                station_id: self.st.0.clone(),
                session_id: session.to_owned(),
                authority: None,
                required_classes: None,
            })
            .await
            .unwrap()
    }

    pub(super) async fn queued(&self, command: &str, session: &str) -> String {
        match self.ask(command, session).await {
            LandRequested::Queued { land_id, .. } => land_id,
            LandRequested::Refused(r) => panic!("refused: {r:?}"),
        }
    }

    pub(super) async fn row(&self, land: &str) -> LandRow {
        self.s.land_row(land.to_owned()).await.unwrap().unwrap()
    }

    /// Admits until nothing changes; returns the number of passes.
    pub(super) async fn admit(&self) {
        while self.s.land_admit_next().await.unwrap() {}
    }

    pub(super) async fn event(&self, seq: u64, session: &str, info: LandInfo) {
        let env = EventEnvelope {
            station_seq: seq,
            session_id: session.into(),
            session_seq: seq,
            at: 1_000,
            proto_version: 1,
            raw_ref: None,
            event: EventBody::Known(Event::SessionLand(Box::new(info))),
        };
        self.s
            .apply_events(self.st.clone(), "inc".into(), vec![env])
            .await
            .unwrap();
    }
}

pub(super) fn info(land: &str, state: LandState, phase: LandPhase) -> LandInfo {
    LandInfo {
        land_id: land.to_owned(),
        attempt: 1,
        state,
        phase,
        reconciling: false,
        code: None,
        text: None,
        files: Vec::new(),
        source_oid: None,
        base_oid: None,
        rebased_oid: None,
        branch_published_oid: None,
        default_published_oid: None,
        pr_url: None,
    }
}

pub(super) fn refused(r: LandRequested) -> (&'static str, String) {
    match r {
        LandRequested::Refused(r) => (r.code, r.message),
        LandRequested::Queued { .. } => panic!("queued"),
    }
}
