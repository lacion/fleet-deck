//! Tests of the directive store (R13 §5D items 3 to 5, R2).

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind};
use fleetdeck_proto::display_directive::{ArtifactRef, DirectiveBody, DirectiveFilter, SessionRef};
use fleetdeck_proto::session::BatchRef;
use fleetdeck_proto::{IncarnationId, SessionSummary, StationId};
use fleetdeck_testkit::Det;

use super::{DirectiveError, DirectiveSender, EffectiveScope};
use crate::store::displays::{DisplayChange, NewDisplay};
use crate::store::stations::NewStation;
use crate::store::{DeckStore, call};

fn card(id: &str, batch: Option<(&str, u32, u32)>) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.batch = batch.map(|(b, index, size)| BatchRef {
        batch_id: b.to_owned(),
        index,
        size,
    });
    s
}

struct Rig {
    s: DeckStore,
    a: StationId,
    b: StationId,
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

/// Station a holds `s1`, `s2` (batch `B1`), `s4` (batch `B3`); station b
/// holds `s3` (batch `B2`) and `s5` (batch `B3`).
async fn rig() -> Rig {
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
        vec![
            card("s1", None),
            card("s2", Some(("B1", 2, 2))),
            card("s1b", Some(("B1", 1, 2))),
            card("s4", Some(("B3", 1, 2))),
        ],
    )
    .await
    .unwrap();
    s.apply_snapshot(
        b.clone(),
        IncarnationId::from("ib"),
        0,
        vec![
            card("s3", Some(("B2", 1, 1))),
            card("s5", Some(("B3", 2, 2))),
        ],
    )
    .await
    .unwrap();
    Rig { s, a, b }
}

impl Rig {
    async fn display(&self, ceiling: &[&StationId]) -> String {
        self.s
            .create_display(NewDisplay {
                name: "Wall".to_owned(),
                ceiling: ceiling.iter().map(|s| (*s).clone()).collect(),
            })
            .await
            .unwrap()
            .display_id
    }

    async fn set(&self, d: &str, body: DirectiveBody) -> Result<u64, DirectiveError> {
        self.s
            .display_directive_set(d.to_owned(), DirectiveSender::Owner, body)
            .await
            .map(|v| v.generation)
    }

    async fn sql(&self, sql: &'static str, d: &str) {
        let d = d.to_owned();
        call(&self.s.inner.db, move |conn| {
            conn.execute(sql, [d])?;
            Ok(())
        })
        .await
        .unwrap();
    }

    async fn sender(&self, d: &str) -> (String, String) {
        let d = d.to_owned();
        call(&self.s.inner.db, move |conn| {
            Ok(conn.query_row(
                "SELECT sender_kind, sender_id FROM display_directives WHERE display_id = ?1",
                [d],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
    }

    async fn rows(&self, d: &str) -> i64 {
        let d = d.to_owned();
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

    async fn read(&self, d: &str) -> (u64, DirectiveBody) {
        let v = self
            .s
            .display_directive(d.to_owned())
            .await
            .unwrap()
            .unwrap();
        (v.generation, v.directive)
    }
}

fn sref(st: &StationId, id: &str) -> SessionRef {
    SessionRef {
        station_id: st.clone(),
        session_id: id.into(),
    }
}

fn focus(st: &StationId, id: &str) -> DirectiveBody {
    DirectiveBody::Focus {
        session: sref(st, id),
        pin: true,
        filter: None,
    }
}

fn race(batch: &str) -> DirectiveBody {
    DirectiveBody::Race {
        batch_id: batch.to_owned(),
        pin: false,
        filter: None,
    }
}

fn artifact(st: &StationId, id: &str, version: u32) -> DirectiveBody {
    DirectiveBody::Artifact {
        artifact: ArtifactRef {
            station_id: st.clone(),
            artifact_id: id.to_owned(),
            version,
        },
        pin: false,
        filter: None,
    }
}

fn mission_with(filter: DirectiveFilter) -> DirectiveBody {
    DirectiveBody::Mission {
        pin: false,
        filter: Some(filter),
    }
}

async fn note(s: &DeckStore, st: &StationId, id: &str, latest: u32) {
    s.note_artifact(
        st.clone(),
        &Artifact {
            artifact_id: id.to_owned(),
            station_id: String::new(),
            session_id: None,
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

#[tokio::test]
async fn a_display_without_a_directive_reads_mission_and_a_missing_display_reads_none() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    assert_eq!(r.read(&d).await, (0, DirectiveBody::mission()));
    assert!(
        r.s.display_directive("nope".into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(r.s.display_scope("nope".into()).await.unwrap().is_none());
}

#[tokio::test]
async fn each_mode_is_stored_read_back_and_moves_the_generation_up() {
    let r = rig().await;
    let d = r.display(&[&r.a, &r.b]).await;
    note(&r.s, &r.a, "art", 3).await;
    let filter = DirectiveFilter {
        stations: vec![r.a.clone()],
        repo: Some("app".to_owned()),
    };
    let bodies = [
        DirectiveBody::mission(),
        focus(&r.a, "s1"),
        DirectiveBody::Review {
            session: sref(&r.b, "s3"),
            pin: false,
            filter: Some(filter),
        },
        race("B1"),
        artifact(&r.a, "art", 3),
        DirectiveBody::Ambient {
            pin: true,
            filter: None,
        },
    ];
    for (i, body) in bodies.into_iter().enumerate() {
        let generation = r.set(&d, body.clone()).await.unwrap();
        assert_eq!(generation, i as u64 + 1);
        // Mode and sender are columns of the row.
        assert_eq!(r.read(&d).await, (generation, body));
        assert_eq!(r.rows(&d).await, 1);
    }
}

#[tokio::test]
async fn a_ceiling_change_deletes_the_row_and_the_generation_continues() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    assert_eq!(r.set(&d, focus(&r.a, "s1")).await.unwrap(), 1);
    r.s.display_update(
        d.clone(),
        DisplayChange {
            ceiling: Some(vec![r.a.clone(), r.b.clone()]),
            ..DisplayChange::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r.rows(&d).await, 0);
    assert_eq!(r.read(&d).await, (2, DirectiveBody::mission()));
    assert_eq!(r.set(&d, focus(&r.a, "s1")).await.unwrap(), 3);
}

#[tokio::test]
async fn a_revoked_display_has_no_directive_and_cannot_take_one() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    r.set(&d, focus(&r.a, "s1")).await.unwrap();
    assert!(r.s.display_revoke(d.clone()).await.unwrap());
    assert_eq!(r.rows(&d).await, 0);
    assert!(r.s.display_directive(d.clone()).await.unwrap().is_none());
    assert!(matches!(
        r.set(&d, DirectiveBody::mission()).await,
        Err(DirectiveError::DisplayUnknown)
    ));
}

#[tokio::test]
async fn the_scope_is_the_ceiling_narrowed_by_the_valid_filter() {
    let r = rig().await;
    let d = r.display(&[&r.a, &r.b]).await;
    let both: BTreeSet<StationId> = [r.a.clone(), r.b.clone()].into();
    let scope = |stations: &BTreeSet<StationId>, repo: Option<&str>| EffectiveScope {
        stations: stations.clone(),
        repo: repo.map(str::to_owned),
    };
    assert_eq!(
        r.s.display_scope(d.clone()).await.unwrap().unwrap(),
        scope(&both, None)
    );
    // A station list narrows; the repository is carried.
    r.set(
        &d,
        mission_with(DirectiveFilter {
            stations: vec![r.a.clone()],
            repo: Some("app".to_owned()),
        }),
    )
    .await
    .unwrap();
    let only_a: BTreeSet<StationId> = [r.a.clone()].into();
    assert_eq!(
        r.s.display_scope(d.clone()).await.unwrap().unwrap(),
        scope(&only_a, Some("app"))
    );
    // An empty station list does not narrow the stations.
    r.set(
        &d,
        mission_with(DirectiveFilter {
            stations: Vec::new(),
            repo: Some("app".to_owned()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        r.s.display_scope(d.clone()).await.unwrap().unwrap(),
        scope(&both, Some("app"))
    );
}

#[tokio::test]
async fn a_coordinator_sender_is_not_granted_and_stores_nothing() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    let out =
        r.s.display_directive_set(
            d.clone(),
            DirectiveSender::Coordinator {
                grant_id: "g1".to_owned(),
            },
            DirectiveBody::mission(),
        )
        .await;
    assert!(matches!(out, Err(DirectiveError::NotGranted)), "{out:?}");
    assert_eq!(r.rows(&d).await, 0);
    assert_eq!(r.read(&d).await.0, 0);
}

#[tokio::test]
async fn a_station_sender_stores_its_id_and_the_row_reads_back() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    let view =
        r.s.display_directive_set(
            d.clone(),
            DirectiveSender::Station(r.a.clone()),
            focus(&r.a, "s1"),
        )
        .await
        .unwrap();
    assert_eq!(view.generation, 1);
    assert_eq!(r.read(&d).await, (1, focus(&r.a, "s1")));
    let (kind, id) = r.sender(&d).await;
    assert_eq!(kind, "station");
    assert_eq!(id, r.a.as_str());
}

#[tokio::test]
async fn a_stored_row_of_another_sender_or_of_a_bad_format_reads_mission() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    r.set(&d, focus(&r.a, "s1")).await.unwrap();
    r.sql(
        "UPDATE display_directives SET sender_kind = 'coordinator' WHERE display_id = ?1",
        &d,
    )
    .await;
    assert_eq!(r.read(&d).await, (1, DirectiveBody::mission()));
    // The foundation test format is not the stored format.
    r.sql(
        "UPDATE display_directives SET sender_kind = 'owner', body = '{\"mode\":\"mission\"}'
         WHERE display_id = ?1",
        &d,
    )
    .await;
    assert_eq!(r.read(&d).await, (1, DirectiveBody::mission()));
    r.sql(
        "UPDATE display_directives SET body = 'not json' WHERE display_id = ?1",
        &d,
    )
    .await;
    assert_eq!(r.read(&d).await, (1, DirectiveBody::mission()));
}

#[tokio::test]
async fn a_reference_that_is_gone_reads_mission_and_the_filter_stops_narrowing() {
    let r = rig().await;
    let d = r.display(&[&r.a, &r.b]).await;
    r.set(
        &d,
        DirectiveBody::Focus {
            session: sref(&r.a, "s1"),
            pin: false,
            filter: Some(DirectiveFilter {
                stations: vec![r.a.clone()],
                repo: None,
            }),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        r.s.display_scope(d.clone())
            .await
            .unwrap()
            .unwrap()
            .stations
            .len(),
        1
    );
    // The session leaves the deck: the read is pure, so the row stays.
    r.s.apply_snapshot(
        r.a.clone(),
        IncarnationId::from("ia2"),
        1,
        vec![card("other", None)],
    )
    .await
    .unwrap();
    assert_eq!(r.read(&d).await, (1, DirectiveBody::mission()));
    assert_eq!(r.rows(&d).await, 1);
    assert_eq!(
        r.s.display_scope(d).await.unwrap().unwrap().stations.len(),
        2
    );
}

#[tokio::test]
async fn a_race_is_expanded_in_full_and_one_member_outside_the_ceiling_refuses_it() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    // `B1` has two members, both on a.
    let (_, members) =
        r.s.display_directive_set_counted(d.clone(), DirectiveSender::Owner, race("B1"))
            .await
            .unwrap();
    assert_eq!(members, 2);
    // `B3` has a member on b, outside the ceiling.
    let g = r.read(&d).await.0;
    assert!(matches!(
        r.set(&d, race("B3")).await,
        Err(DirectiveError::OutsideCeiling)
    ));
    assert!(matches!(
        r.set(&d, race("B2")).await,
        Err(DirectiveError::OutsideCeiling)
    ));
    // The refusal changed nothing.
    assert_eq!(r.read(&d).await, (g, race("B1")));
    // A member that joins the batch outside the ceiling ends the directive
    // at the next read.
    r.s.apply_snapshot(
        r.b.clone(),
        IncarnationId::from("ib2"),
        1,
        vec![card("late", Some(("B1", 1, 3)))],
    )
    .await
    .unwrap();
    assert_eq!(r.read(&d).await, (g, DirectiveBody::mission()));
    // An unknown batch is invalid.
    assert!(matches!(
        r.set(&d, race("NOBATCH")).await,
        Err(DirectiveError::Invalid(_))
    ));
}

#[tokio::test]
async fn references_are_checked_against_the_ceiling_before_they_are_looked_up() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    note(&r.s, &r.a, "art", 2).await;
    note(&r.s, &r.b, "bart", 2).await;
    let outside = [
        focus(&r.b, "s3"),
        focus(&r.b, "does-not-exist"),
        artifact(&r.b, "bart", 1),
        mission_with(DirectiveFilter {
            stations: vec![r.b.clone()],
            repo: None,
        }),
    ];
    for body in outside {
        let out = r.set(&d, body).await;
        assert!(
            matches!(out, Err(DirectiveError::OutsideCeiling)),
            "{out:?}"
        );
    }
    let invalid = [
        focus(&r.a, "does-not-exist"),
        artifact(&r.a, "missing", 1),
        artifact(&r.a, "art", 3),
    ];
    for body in invalid {
        let out = r.set(&d, body).await;
        assert!(matches!(out, Err(DirectiveError::Invalid(_))), "{out:?}");
    }
    assert!(r.set(&d, artifact(&r.a, "art", 2)).await.is_ok());
    // A station that is not a station of the deck is outside the ceiling.
    let out = r.set(&d, focus(&StationId::new("ghost"), "s1")).await;
    assert!(
        matches!(out, Err(DirectiveError::OutsideCeiling)),
        "{out:?}"
    );
}

#[tokio::test]
async fn the_shape_limits_are_enforced() {
    let r = rig().await;
    let d = r.display(&[&r.a]).await;
    let many = (0..65).map(|i| StationId::new(format!("st{i}"))).collect();
    let filter = |stations: Vec<StationId>, repo: Option<&str>| {
        mission_with(DirectiveFilter {
            stations,
            repo: repo.map(str::to_owned),
        })
    };
    let bad = [
        filter(many, None),
        filter(Vec::new(), Some("")),
        filter(Vec::new(), Some(&"x".repeat(256))),
        filter(Vec::new(), Some("a/b")),
        filter(Vec::new(), Some("a\nb")),
        race(""),
        race(&"b".repeat(65)),
        race("has space"),
        artifact(&r.a, "art", 0),
        artifact(&r.a, "", 1),
        artifact(&r.a, &"a".repeat(129), 1),
        artifact(&r.a, "a\u{0}b", 1),
    ];
    for body in bad {
        let out = r.set(&d, body).await;
        assert!(matches!(out, Err(DirectiveError::Invalid(_))), "{out:?}");
    }
    assert_eq!(r.rows(&d).await, 0);
    // The edges pass: 255 bytes of repository, 64 listed stations (ids that
    // are in the ceiling are one, the rest repeat it).
    let edge = filter(vec![r.a.clone(); 64], Some(&"x".repeat(255)));
    assert!(r.set(&d, edge).await.is_ok());
}

#[tokio::test]
async fn the_directive_survives_a_restart() {
    let det = Det::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let s = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    let a = station(&s, "alpha").await;
    s.apply_snapshot(
        a.clone(),
        IncarnationId::from("ia"),
        0,
        vec![card("s1", None)],
    )
    .await
    .unwrap();
    let d = s
        .create_display(NewDisplay {
            name: "Wall".to_owned(),
            ceiling: vec![a.clone()],
        })
        .await
        .unwrap()
        .display_id;
    s.display_directive_set(d.clone(), DirectiveSender::Owner, focus(&a, "s1"))
        .await
        .unwrap();
    drop(s);
    let s = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    let v = s.display_directive(d).await.unwrap().unwrap();
    assert_eq!((v.generation, v.directive), (1, focus(&a, "s1")));
}
