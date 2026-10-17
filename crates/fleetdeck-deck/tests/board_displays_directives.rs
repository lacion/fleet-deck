//! The directives of a wall display over loopback HTTPS (R13 P2b, §5D items
//! 3 to 5, R2): the owner's `POST /api/displays/{id}/directive`, the display's
//! poll `GET /api/displays/self/directive`, the refusals and the audit rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;

use common::Fixture;
use common::board::{BoardClient, Http};
use displays_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::displays::DisplayChange;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::session::BatchRef;
use fleetdeck_proto::{IncarnationId, StationId};
use serde_json::{Value, json};
use share_support::*;

const DAY: i64 = 24 * 60 * 60 * 1000;

fn batch_card(id: &str, batch: &str, index: u32, size: u32) -> fleetdeck_proto::SessionSummary {
    let mut s = card(id);
    s.batch = Some(BatchRef {
        batch_id: batch.to_owned(),
        index,
        size,
    });
    s
}

struct Rig {
    f: Fixture,
    c: BoardClient,
    h: Headers,
    /// The station of the fixture (it holds `s1`, `b1`, `b2`, `m1`).
    st: String,
    /// A station outside the ceiling (`o1`, `m2`).
    other: StationId,
    display: String,
    cookie: String,
}

async fn rig() -> Rig {
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![
                card("s1"),
                batch_card("b1", "BATCH1", 1, 2),
                batch_card("b2", "BATCH1", 2, 2),
                batch_card("m1", "MIXED", 1, 2),
            ],
        )
        .await
        .unwrap();
    let rec = c
        .store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    c.store
        .apply_snapshot(
            rec.station_id.clone(),
            IncarnationId::from("i2"),
            0,
            vec![card("o1"), batch_card("m2", "MIXED", 2, 2)],
        )
        .await
        .unwrap();
    c.store
        .note_artifact(
            f.station.clone(),
            &Artifact {
                artifact_id: "art1".to_owned(),
                station_id: String::new(),
                session_id: None,
                title: "t".to_owned(),
                kind: ArtifactKind::Html,
                latest_version: 2,
                created_at: 1,
                updated_at: 1,
                author: "agent".to_owned(),
            },
        )
        .await
        .unwrap();
    let (display, cookie) = display_login(&c, "Wall", &[&st]).await;
    Rig {
        f,
        c,
        h,
        st,
        other: rec.station_id,
        display,
        cookie,
    }
}

impl Rig {
    async fn send(&self, body: Value) -> Http {
        post(
            &self.c,
            &self.h,
            &format!("/api/displays/{}/directive", self.display),
            body,
        )
        .await
    }

    async fn poll(&self) -> Http {
        get_with(&self.c, "/api/displays/self/directive", &self.cookie).await
    }

    fn focus(&self, session: &str) -> Value {
        json!({ "mode": "focus", "pin": true,
            "session": { "station_id": self.st, "session_id": session } })
    }
}

#[tokio::test]
async fn the_owner_sends_each_mode_and_the_display_polls_it() {
    let r = rig().await;
    // A display without a directive polls `mission`.
    let first = r.poll().await;
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(
        first.body,
        json!({ "generation": 0, "directive": { "mode": "mission", "pin": false, "filter": null } })
    );
    let bodies = [
        json!({ "mode": "mission" }),
        r.focus("s1"),
        json!({ "mode": "review", "session": { "station_id": r.st, "session_id": "b1" } }),
        json!({ "mode": "race", "batch_id": "BATCH1", "pin": true }),
        json!({ "mode": "artifact",
            "artifact": { "station_id": r.st, "artifact_id": "art1", "version": 2 } }),
        json!({ "mode": "ambient",
            "filter": { "stations": [r.st], "repo": "app" } }),
    ];
    for (i, body) in bodies.into_iter().enumerate() {
        let sent = r.send(body.clone()).await;
        assert_eq!(sent.status, 200, "{body} {}", sent.body);
        assert_eq!(sent.body["generation"], i as u64 + 1);
        assert_eq!(sent.body["directive"]["mode"], body["mode"]);
        // The display polls the same view.
        let polled = r.poll().await;
        assert_eq!(polled.status, 200, "{}", polled.body);
        assert_eq!(polled.body, sent.body);
    }
}

#[tokio::test]
async fn an_invalid_directive_is_refused_with_a_fixed_text_and_changes_nothing() {
    let r = rig().await;
    let st = &r.st;
    let bad = [
        // Not valid for the type: unknown field, a field of another mode, a
        // missing field, an unknown mode, a URL.
        json!({ "mode": "mission", "url": "https://x.example" }),
        json!({ "mode": "mission", "session": { "station_id": st, "session_id": "s1" } }),
        json!({ "mode": "focus" }),
        json!({ "mode": "bogus" }),
        json!({ "mode": "artifact", "artifact": { "station_id": st, "artifact_id": "art1" } }),
        json!({}),
        // The limits.
        json!({ "mode": "mission", "filter": { "stations": vec![st; 65] } }),
        json!({ "mode": "mission", "filter": { "stations": [], "repo": "" } }),
        json!({ "mode": "mission", "filter": { "stations": [], "repo": "a".repeat(256) } }),
        json!({ "mode": "mission", "filter": { "stations": [], "repo": "a/b" } }),
        json!({ "mode": "race", "batch_id": "has space" }),
        json!({ "mode": "race", "batch_id": "b".repeat(65) }),
        json!({ "mode": "artifact", "artifact": { "station_id": st, "artifact_id": "art1", "version": 0 } }),
        json!({ "mode": "artifact", "artifact": { "station_id": st, "artifact_id": "a".repeat(129), "version": 1 } }),
        // Not found.
        r.focus("does-not-exist"),
        json!({ "mode": "race", "batch_id": "NOBATCH" }),
        json!({ "mode": "artifact", "artifact": { "station_id": st, "artifact_id": "none", "version": 1 } }),
        json!({ "mode": "artifact", "artifact": { "station_id": st, "artifact_id": "art1", "version": 3 } }),
    ];
    for body in bad {
        let sent = r.send(body.clone()).await;
        assert_eq!(
            (sent.status, sent.code()),
            (400, "invalid_directive"),
            "{body} {}",
            sent.body
        );
        // The answer holds no text of the input.
        assert!(!sent.text.contains("example"), "{}", sent.text);
        assert!(!sent.text.contains("has space"), "{}", sent.text);
    }
    assert_eq!(r.poll().await.body["generation"], 0);
}

#[tokio::test]
async fn a_reference_outside_the_ceiling_is_refused_whole() {
    let r = rig().await;
    let other = r.other.as_str().to_owned();
    let outside = [
        json!({ "mode": "focus", "session": { "station_id": other, "session_id": "o1" } }),
        // The station is checked before the session is looked up.
        json!({ "mode": "focus", "session": { "station_id": other, "session_id": "nope" } }),
        json!({ "mode": "artifact", "artifact": { "station_id": other, "artifact_id": "x", "version": 1 } }),
        json!({ "mode": "mission", "filter": { "stations": [other] } }),
        // `MIXED` has one member on the other station.
        json!({ "mode": "race", "batch_id": "MIXED" }),
    ];
    for body in outside {
        let sent = r.send(body.clone()).await;
        assert_eq!(
            (sent.status, sent.code()),
            (403, "outside_ceiling"),
            "{body} {}",
            sent.body
        );
    }
    assert_eq!(r.poll().await.body["generation"], 0);
}

#[tokio::test]
async fn a_body_above_8_kib_is_refused() {
    let r = rig().await;
    let big = json!({ "mode": "mission", "filter": { "stations": [], "repo": "x".repeat(9000) } });
    let sent = r.send(big).await;
    assert_eq!(
        (sent.status, sent.code()),
        (413, "too_large"),
        "{}",
        sent.body
    );
    assert_eq!(r.poll().await.body["generation"], 0);
}

#[tokio::test]
async fn the_owner_only_row_refuses_other_principals_and_unknown_displays() {
    let r = rig().await;
    let path = format!("/api/displays/{}/directive", r.display);
    // A display login is read-only.
    let origin = r.c.origin();
    let by_display =
        r.c.post(
            &path,
            &[("Cookie", &r.cookie), ("Origin", &origin)],
            json!({ "mode": "mission" }),
        )
        .await;
    expect(&by_display, 403, "display_read_only");
    // A token with every scope is not the owner.
    let (_, t) =
        r.c.store
            .create_token(
                "ops".into(),
                [Scope::Read, Scope::Drive, Scope::Admin]
                    .into_iter()
                    .collect(),
                DAY,
            )
            .await
            .unwrap();
    let bearer = format!("Bearer {}", t.expose());
    let by_token =
        r.c.post(
            &path,
            &[("Authorization", &bearer)],
            json!({ "mode": "mission" }),
        )
        .await;
    expect(&by_token, 403, "owner_only");
    // An unknown display.
    let unknown = post(
        &r.c,
        &r.h,
        "/api/displays/nope/directive",
        json!({ "mode": "mission" }),
    )
    .await;
    expect(&unknown, 404, "display_unknown");
    // Without a login.
    let anon = r.c.post(&path, &[], json!({ "mode": "mission" })).await;
    assert_eq!(anon.status, 401, "{}", anon.body);
    assert_eq!(r.poll().await.body["generation"], 0);
}

#[tokio::test]
async fn the_poll_needs_the_display_cookie_and_ends_with_a_revoke() {
    let r = rig().await;
    // The owner cookie alone is not a display login.
    expect(
        &get(&r.c, &r.h, "/api/displays/self/directive").await,
        401,
        "login_required",
    );
    // With both cookies the display principal answers.
    r.send(r.focus("s1")).await;
    let both = format!("{}; {}", r.h[0].1, r.cookie);
    let polled =
        r.c.get("/api/displays/self/directive", &[("Cookie", &both)])
            .await;
    assert_eq!(polled.status, 200, "{}", polled.body);
    assert_eq!(polled.body["directive"]["mode"], "focus");
    // A revoke ends the login and the directive.
    let revoked = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{}/revoke", r.display),
        json!({}),
    )
    .await;
    assert_eq!(revoked.status, 204, "{}", revoked.body);
    expect(&r.poll().await, 401, "login_required");
    let after = r.send(json!({ "mode": "mission" })).await;
    expect(&after, 404, "display_unknown");
}

#[tokio::test]
async fn a_ceiling_change_returns_the_display_to_mission() {
    let r = rig().await;
    let second = r.other.clone();
    r.send(r.focus("s1")).await;
    assert_eq!(r.poll().await.body["directive"]["mode"], "focus");
    r.c.store
        .display_update(
            r.display.clone(),
            DisplayChange {
                ceiling: Some(vec![StationId::from(r.st.as_str()), second]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    let polled = r.poll().await;
    assert_eq!(
        polled.body["directive"]["mode"], "mission",
        "{}",
        polled.body
    );
    assert_eq!(polled.body["generation"], 2);
    // A new directive continues the counter.
    assert_eq!(r.send(r.focus("s1")).await.body["generation"], 3);
}

#[tokio::test]
async fn a_directive_is_audited_with_identifiers_only() {
    let r = rig().await;
    // A sentinel in a title and in the filter repository.
    let mut titled = card("s1");
    titled.title = Some(format!("title {SECRET}"));
    r.f.store
        .apply_snapshot(
            r.f.station.clone(),
            r.f.inc.clone(),
            0,
            vec![
                titled,
                batch_card("b1", "BATCH1", 1, 2),
                batch_card("b2", "BATCH1", 2, 2),
            ],
        )
        .await
        .unwrap();
    let ok = r
        .send(json!({ "mode": "focus",
            "session": { "station_id": r.st, "session_id": "s1" },
            "filter": { "stations": [r.st], "repo": SECRET } }))
        .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    let refused = r
        .send(json!({ "mode": "focus",
            "session": { "station_id": r.st, "session_id": "gone" },
            "filter": { "repo": SECRET, "stations": [] } }))
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    let oks = r.c.audited("display.directive", AuditResult::Ok).await;
    assert_eq!(
        oks,
        vec![Some(format!(
            "mode=focus session={}/s1 filter_stations=1 repo_filter=yes",
            r.st
        ))]
    );
    let failed = r.c.audited("display.directive", AuditResult::Failed).await;
    assert_eq!(failed, vec![Some("invalid_directive".to_owned())]);
    let rows = r.c.store.audit_log(100).await.unwrap();
    for row in rows.iter().filter(|e| e.action == "display.directive") {
        assert_eq!(row.target.as_deref(), Some(r.display.as_str()));
        assert!(!format!("{row:?}").contains(SECRET), "{row:?}");
    }
    // A race records the number of members.
    let race = r
        .send(json!({ "mode": "race", "batch_id": "BATCH1" }))
        .await;
    assert_eq!(race.status, 200, "{}", race.body);
    let oks = r.c.audited("display.directive", AuditResult::Ok).await;
    assert!(
        oks.contains(&Some("mode=race batch=BATCH1 members=2".to_owned())),
        "{oks:?}"
    );
}
