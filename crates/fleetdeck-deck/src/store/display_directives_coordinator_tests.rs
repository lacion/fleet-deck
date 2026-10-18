//! Tests of the coordinator sender of a directive (R13 P6 §4.2, OD-7, T5
//! items 1 to 5): the write, the reference rules, the provenance columns, the
//! delivery reads and the invalidation.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind};
use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::display_directive::{ArtifactRef, DirectiveBody, DirectiveFilter, SessionRef};
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{IncarnationId, SessionSummary, StationId};
use fleetdeck_testkit::Det;

use super::{CoordinatorRefs, CoordinatorSender};
use crate::store::displays::{DisplayChange, NewDisplay};
use crate::store::stations::NewStation;
use crate::store::{DeckStore, Grant, GrantClass, NewGrant, call};

const NOT_FOUND: &str = "desktop_display_not_found";
const NOT_GRANTED: &str = "desktop_reference_not_granted";
const LOST: &str = "authority_lost";

struct Rig {
    det: Det,
    s: DeckStore,
    a: StationId,
    b: StationId,
    grant: Grant,
    display: String,
}

fn card(id: &str) -> SessionSummary {
    SessionSummary::new(id.into())
}

async fn station(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([fleetdeck_proto::link::Scope::Read]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

/// Two stations; `alpha` holds `coord-1` (the caller), `child` and `other`;
/// `beta` holds `far`. A bound `desktop` grant of `coord-1` on both stations;
/// a display with the ceiling `alpha`, bound to `alpha`.
async fn rig_with(scope: GrantScope) -> Rig {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let a = station(&s, "alpha").await;
    let b = station(&s, "beta").await;
    s.apply_snapshot(
        a.clone(),
        IncarnationId::from("ia"),
        0,
        vec![card("coord-1"), card("child"), card("other")],
    )
    .await
    .unwrap();
    s.apply_snapshot(b.clone(), IncarnationId::from("ib"), 0, vec![card("far")])
        .await
        .unwrap();
    let mut new = NewGrant::new(a.clone(), "coord-1".into());
    new.classes = BTreeSet::from([GrantClass::Desktop]);
    new.stations = BTreeSet::from([a.clone(), b.clone()]);
    new.scope = scope;
    let grant = s.create_grant(new).await.unwrap();
    let bound = s
        .bind_grant(CoordinatorBinding {
            binding_id: "b-1".into(),
            grant_id: grant.grant_id.clone(),
            generation: 1,
            provider_instance: "claude".into(),
            native_id: "n-1".into(),
        })
        .await
        .unwrap();
    assert!(bound);
    let display = s
        .create_display(NewDisplay {
            name: "Wall".to_owned(),
            ceiling: vec![a.clone()],
        })
        .await
        .unwrap()
        .display_id;
    let r = Rig {
        det,
        s,
        a,
        b,
        grant,
        display,
    };
    r.bind(&r.display, &r.a).await;
    r
}

async fn rig() -> Rig {
    rig_with(GrantScope::Lineage).await
}

impl Rig {
    async fn bind(&self, display: &str, station: &StationId) {
        self.s
            .display_set_binding(
                display.to_owned(),
                Some(DisplayBinding {
                    station_id: station.clone(),
                    monitor: "DP-1".to_owned(),
                }),
            )
            .await
            .unwrap();
    }

    fn sender(&self) -> CoordinatorSender {
        CoordinatorSender::new(
            self.grant.grant_id.clone(),
            1,
            self.a.clone(),
            "coord-1".into(),
        )
    }

    fn refs(&self) -> CoordinatorRefs {
        CoordinatorRefs {
            target: self.a.clone(),
        }
    }

    async fn write(
        &self,
        display: &str,
        body: DirectiveBody,
    ) -> std::result::Result<u64, &'static str> {
        self.s
            .display_directive_coordinator(
                display.to_owned(),
                self.sender(),
                body,
                self.refs(),
                self.s.now_ms(),
            )
            .await
            .unwrap()
            .map(|v| v.generation)
    }

    async fn show(&self, body: DirectiveBody) -> std::result::Result<u64, &'static str> {
        self.write(&self.display, body).await
    }

    async fn sql(&self, sql: &'static str) {
        call(&self.s.inner.db, move |conn| {
            conn.execute_batch(sql)?;
            Ok(())
        })
        .await
        .unwrap();
    }

    async fn rows(&self) -> i64 {
        let d = self.display.clone();
        call(&self.s.inner.db, move |conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM display_directives WHERE display_id = ?1",
                [d],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
    }

    /// An artifact of `alpha` with `latest` versions and the session `owner`.
    async fn artifact(&self, id: &str, owner: Option<&str>, latest: u32) {
        self.s
            .note_artifact(
                self.a.clone(),
                &Artifact {
                    artifact_id: id.to_owned(),
                    station_id: String::new(),
                    session_id: owner.map(Into::into),
                    title: "t".to_owned(),
                    kind: ArtifactKind::Html,
                    latest_version: latest,
                    created_at: 1,
                    updated_at: 1,
                    author: "agent".to_owned(),
                },
            )
            .await
            .unwrap();
    }

    /// A deck capture record whose artifact is `id`, with `context` as its
    /// `coordinator` value.
    async fn capture(&self, id: &str, context: Option<String>) {
        let (a, id) = (self.a.clone(), id.to_owned());
        call(&self.s.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO desktop_captures
                 (station_id, capture_id, action_hash, state, monitor, published, indexed,
                  created_at, updated_at, coordinator)
                 VALUES (?1, ?2, 'h', 'committed', 'DP-1', ?3, 1, 1, 1, ?4)",
                rusqlite::params![
                    a.as_str(),
                    format!("cap-{id}"),
                    serde_json::json!({"artifact": {"artifact_id": id}}).to_string(),
                    context
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }

    fn context(&self, grant: &str, session: &str) -> String {
        serde_json::json!({
            "grant_id": grant, "binding_id": "b-1", "generation": 1,
            "caller_station": self.a, "caller_session": session, "caller_label": "x"
        })
        .to_string()
    }
}

fn filter(station: &StationId) -> Option<DirectiveFilter> {
    Some(DirectiveFilter {
        stations: vec![station.clone()],
        repo: None,
    })
}

fn focus(station: &StationId, session: &str) -> DirectiveBody {
    DirectiveBody::Focus {
        session: SessionRef {
            station_id: station.clone(),
            session_id: session.into(),
        },
        pin: false,
        filter: filter(station),
    }
}

fn artifact(station: &StationId, id: &str, version: u32) -> DirectiveBody {
    DirectiveBody::Artifact {
        artifact: ArtifactRef {
            station_id: station.clone(),
            artifact_id: id.to_owned(),
            version,
        },
        pin: false,
        filter: filter(station),
    }
}

#[tokio::test]
async fn a_focus_on_the_caller_is_stored_with_the_coordinator_provenance() {
    let r = rig().await;
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Ok(1));
    let d = r.display.clone();
    let row: (String, String, i64, String, String) = call(&r.s.inner.db, move |conn| {
        Ok(conn.query_row(
            "SELECT sender_kind, sender_id, sender_generation, sender_station, sender_session
             FROM display_directives WHERE display_id = ?1",
            [d],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?)
    })
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "coordinator".to_owned(),
            r.grant.grant_id.clone(),
            1,
            r.a.to_string(),
            "coord-1".to_owned()
        )
    );
    // The synchronous readers fail closed: `mission` with the generation, and
    // the scope is the whole ceiling.
    let view = r.s.display_directive(r.display.clone()).await.unwrap();
    let view = view.unwrap();
    assert_eq!(
        (view.generation, view.directive),
        (1, DirectiveBody::mission())
    );
    let scope = r.s.display_scope(r.display.clone()).await.unwrap().unwrap();
    assert_eq!(scope.stations, BTreeSet::from([r.a.clone()]));
    // The delivery read sees the directive.
    let got =
        r.s.display_coordinator_row(r.display.clone())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(got.generation, 1);
    assert_eq!(got.directive, focus(&r.a, "coord-1"));
    assert_eq!(got.sender, r.sender());
    assert_eq!(got.bound_station, Some(r.a.clone()));
}

#[tokio::test]
async fn a_lineage_child_is_allowed_and_a_session_outside_the_lineage_is_not() {
    let r = rig().await;
    assert_eq!(r.show(focus(&r.a, "child")).await, Err(NOT_GRANTED));
    r.s.add_lineage(
        r.grant.grant_id.clone(),
        r.a.clone(),
        "child".into(),
        "cmd-1".to_owned(),
    )
    .await
    .unwrap();
    assert_eq!(r.show(focus(&r.a, "child")).await, Ok(1));
    assert_eq!(r.show(focus(&r.a, "other")).await, Err(NOT_GRANTED));
    // Another grant's lineage is not this grant's.
    assert_eq!(r.rows().await, 1);
}

#[tokio::test]
async fn a_session_outside_the_lineage_is_refused_under_the_fleet_scope_too() {
    let r = rig_with(GrantScope::Fleet).await;
    assert_eq!(r.grant.scope, GrantScope::Fleet);
    assert_eq!(r.show(focus(&r.a, "other")).await, Err(NOT_GRANTED));
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Ok(1));
    assert_eq!(r.rows().await, 1);
}

#[tokio::test]
async fn an_artifact_needs_an_explicit_existing_untombstoned_version_of_an_allowed_session() {
    let r = rig().await;
    r.artifact("mine", Some("coord-1"), 2).await;
    r.artifact("theirs", Some("other"), 1).await;
    assert_eq!(r.show(artifact(&r.a, "mine", 2)).await, Ok(1));
    // An implicit version (0), a version that does not exist, an artifact of a
    // session outside the lineage and an unknown artifact.
    for body in [
        artifact(&r.a, "mine", 0),
        artifact(&r.a, "mine", 3),
        artifact(&r.a, "theirs", 1),
        artifact(&r.a, "none", 1),
    ] {
        assert_eq!(r.show(body).await, Err(NOT_GRANTED));
    }
    // A tombstone of the artifact.
    let a = r.a.to_string();
    call(&r.s.inner.db, move |conn| {
        conn.execute(
            "INSERT OR REPLACE INTO artifact_tombstones
             (station_id, artifact_id, state, attempts, created_at, updated_at)
             VALUES (?1, 'mine', 'pending', 0, 1, 1)",
            [a],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(r.show(artifact(&r.a, "mine", 2)).await, Err(NOT_GRANTED));
}

#[tokio::test]
async fn a_sessionless_capture_is_allowed_only_for_this_grant_and_caller() {
    let r = rig().await;
    r.artifact("cap-ours", None, 1).await;
    r.artifact("cap-other-grant", None, 1).await;
    r.artifact("cap-other-session", None, 1).await;
    r.artifact("cap-owner", None, 1).await;
    r.artifact("no-record", None, 1).await;
    let grant = r.grant.grant_id.clone();
    r.capture("cap-ours", Some(r.context(&grant, "coord-1")))
        .await;
    r.capture(
        "cap-other-grant",
        Some(r.context("another-grant", "coord-1")),
    )
    .await;
    r.capture("cap-other-session", Some(r.context(&grant, "w1")))
        .await;
    r.capture("cap-owner", None).await;
    assert_eq!(r.show(artifact(&r.a, "cap-ours", 1)).await, Ok(1));
    for id in [
        "cap-other-grant",
        "cap-other-session",
        "cap-owner",
        "no-record",
    ] {
        assert_eq!(
            r.show(artifact(&r.a, id, 1)).await,
            Err(NOT_GRANTED),
            "{id}"
        );
    }
}

#[tokio::test]
async fn an_unknown_a_revoked_and_a_foreign_display_are_the_same_refusal() {
    let r = rig().await;
    let elsewhere =
        r.s.create_display(NewDisplay {
            name: "Elsewhere".to_owned(),
            ceiling: vec![r.a.clone(), r.b.clone()],
        })
        .await
        .unwrap()
        .display_id;
    r.bind(&elsewhere, &r.b).await;
    let unbound =
        r.s.create_display(NewDisplay {
            name: "Unbound".to_owned(),
            ceiling: vec![r.a.clone()],
        })
        .await
        .unwrap()
        .display_id;
    let revoked =
        r.s.create_display(NewDisplay {
            name: "Gone".to_owned(),
            ceiling: vec![r.a.clone()],
        })
        .await
        .unwrap()
        .display_id;
    r.bind(&revoked, &r.a).await;
    assert!(r.s.display_revoke(revoked.clone()).await.unwrap());
    for display in ["no-such-display", &elsewhere, &unbound, &revoked] {
        assert_eq!(
            r.write(display, focus(&r.a, "coord-1")).await,
            Err(NOT_FOUND),
            "{display}"
        );
    }
    assert_eq!(r.rows().await, 0);
}

#[tokio::test]
async fn the_display_ceiling_narrows_what_can_be_shown() {
    let r = rig().await;
    // `far` is on beta, which is a grant station but outside the ceiling.
    let body = DirectiveBody::Focus {
        session: SessionRef {
            station_id: r.b.clone(),
            session_id: "far".into(),
        },
        pin: false,
        filter: filter(&r.b),
    };
    r.s.add_lineage(
        r.grant.grant_id.clone(),
        r.b.clone(),
        "far".into(),
        "cmd-far".to_owned(),
    )
    .await
    .unwrap();
    assert_eq!(r.show(body.clone()).await, Err(NOT_GRANTED));
    // Widening the ceiling allows it.
    r.s.display_update(
        r.display.clone(),
        DisplayChange {
            ceiling: Some(vec![r.a.clone(), r.b.clone()]),
            ..DisplayChange::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r.show(body).await, Ok(1));
}

#[tokio::test]
async fn a_station_outside_the_grant_a_foreign_filter_and_another_mode_are_refused() {
    let r = rig().await;
    let c = station(&r.s, "gamma").await;
    r.s.apply_snapshot(c.clone(), IncarnationId::from("ic"), 0, vec![card("g1")])
        .await
        .unwrap();
    r.s.display_update(
        r.display.clone(),
        DisplayChange {
            ceiling: Some(vec![r.a.clone(), c.clone()]),
            ..DisplayChange::default()
        },
    )
    .await
    .unwrap();
    // `gamma` is in the ceiling but not a grant station.
    assert_eq!(r.show(focus(&c, "g1")).await, Err(NOT_GRANTED));
    // A filter that is not exactly the referenced station, and a mode that the
    // deck does not build.
    let wide = DirectiveBody::Focus {
        session: SessionRef {
            station_id: r.a.clone(),
            session_id: "coord-1".into(),
        },
        pin: false,
        filter: Some(DirectiveFilter {
            stations: vec![r.a.clone(), c.clone()],
            repo: None,
        }),
    };
    let none = DirectiveBody::Focus {
        session: SessionRef {
            station_id: r.a.clone(),
            session_id: "coord-1".into(),
        },
        pin: false,
        filter: None,
    };
    for body in [
        wide,
        none,
        DirectiveBody::mission(),
        DirectiveBody::Ambient {
            pin: false,
            filter: filter(&r.a),
        },
    ] {
        assert_eq!(r.show(body).await, Err(NOT_GRANTED));
    }
    assert_eq!(r.rows().await, 0);
}

#[tokio::test]
async fn a_grant_that_does_not_hold_is_authority_lost() {
    let r = rig().await;
    // Another binding generation.
    let stale = CoordinatorSender::new(r.grant.grant_id.clone(), 2, r.a.clone(), "coord-1".into());
    let out =
        r.s.display_directive_coordinator(
            r.display.clone(),
            stale,
            focus(&r.a, "coord-1"),
            r.refs(),
            r.s.now_ms(),
        )
        .await
        .unwrap();
    assert_eq!(out, Err(LOST));
    // Another caller session.
    let other = CoordinatorSender::new(r.grant.grant_id.clone(), 1, r.a.clone(), "other".into());
    let out =
        r.s.display_directive_coordinator(
            r.display.clone(),
            other,
            focus(&r.a, "other"),
            r.refs(),
            r.s.now_ms(),
        )
        .await
        .unwrap();
    assert_eq!(out, Err(LOST));
    // An idle grant, an expired grant and a revoked grant.
    r.det.advance(Duration::from_secs(31 * 60));
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Err(LOST));
    let r = rig().await;
    assert!(
        r.s.revoke_grant(r.grant.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Err(LOST));
    assert_eq!(r.rows().await, 0);
}

#[tokio::test]
async fn the_precheck_gives_the_same_refusals_and_writes_nothing() {
    let r = rig().await;
    let check = |display: &str, body| {
        r.s.display_coordinator_precheck(
            display.to_owned(),
            r.sender(),
            body,
            r.refs(),
            r.s.now_ms(),
        )
    };
    assert_eq!(
        check(&r.display, focus(&r.a, "coord-1")).await.unwrap(),
        Ok(())
    );
    assert_eq!(
        check(&r.display, focus(&r.a, "other")).await.unwrap(),
        Err(NOT_GRANTED)
    );
    assert_eq!(
        check("nope", focus(&r.a, "coord-1")).await.unwrap(),
        Err(NOT_FOUND)
    );
    assert_eq!(r.rows().await, 0);
}

#[tokio::test]
async fn revoking_the_grant_and_its_idle_expiry_delete_the_directive_and_move_the_generation() {
    let r = rig().await;
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Ok(1));
    assert!(
        r.s.revoke_grant(r.grant.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(r.rows().await, 0);
    let view = r.s.display_directive(r.display.clone()).await.unwrap();
    assert_eq!(view.unwrap().generation, 2);
    // The sweep ends a grant that is idle and deletes its directive too.
    let r = rig().await;
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Ok(1));
    r.det.advance(Duration::from_secs(31 * 60));
    r.s.sweep_grants().await.unwrap();
    assert_eq!(r.rows().await, 0);
    let view = r.s.display_directive(r.display.clone()).await.unwrap();
    assert_eq!(view.unwrap().generation, 2);
}

#[tokio::test]
async fn a_row_of_another_owner_is_never_a_coordinator_row_and_an_owner_write_replaces_it() {
    let r = rig().await;
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Ok(1));
    let a = r.a.clone();
    r.s.display_directive_set(
        r.display.clone(),
        crate::store::display_directives::DirectiveSender::Owner,
        focus(&a, "other"),
    )
    .await
    .unwrap();
    // The owner row has no coordinator provenance.
    assert!(
        r.s.display_coordinator_row(r.display.clone())
            .await
            .unwrap()
            .is_none()
    );
    // The invalidation of the old generation keeps the newer owner directive.
    assert!(
        !r.s.display_coordinator_invalidate(r.display.clone(), 1)
            .await
            .unwrap()
    );
    assert_eq!(r.rows().await, 1);
}

#[tokio::test]
async fn the_local_pass_checks_the_generation_the_grant_and_the_binding() {
    let r = rig().await;
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Ok(1));
    let now = r.s.now_ms();
    let valid = |generation: u64| {
        r.s.display_coordinator_valid(r.display.clone(), generation, now)
    };
    assert!(valid(1).await.unwrap().is_some());
    // Another generation is not this row.
    assert!(valid(2).await.unwrap().is_none());
    // A binding to a station outside the grant fails the pass.
    r.sql("UPDATE tokens SET stations = '[]'").await;
    assert!(valid(1).await.unwrap().is_none());
}

#[tokio::test]
async fn invalidating_the_current_generation_deletes_the_row_once() {
    let r = rig().await;
    assert_eq!(r.show(focus(&r.a, "coord-1")).await, Ok(1));
    assert!(
        r.s.display_coordinator_invalidate(r.display.clone(), 1)
            .await
            .unwrap()
    );
    assert!(
        !r.s.display_coordinator_invalidate(r.display.clone(), 1)
            .await
            .unwrap()
    );
    let view = r.s.display_directive(r.display.clone()).await.unwrap();
    assert_eq!(view.unwrap().generation, 2);
}
