//! The P6 `wall_show` and the live delivery of a coordinator directive (R13
//! P6 task T5): the call, the reference rules, the provenance, the audit rows,
//! the delivery after both policies, and the `mission` result on every
//! revocation, expiry, new generation, tombstone, ceiling change and policy
//! answer. The store rules are in `store/display_directives_coordinator_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod coordinator_desktop_support;

use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, Ws, board};
use common::fleet::card;
use coordinator_desktop_support::*;
use fleetdeck_deck::store::coordinator_desktop::{Charges, OpState, OpTool, request_key};
use fleetdeck_deck::store::display_directives::DirectiveSender;
use fleetdeck_deck::store::displays::{DisplayChange, NewDisplay};
use fleetdeck_deck::store::{GrantClass, NewGrant};
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind};
use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::display_directive::{DirectiveBody, SessionRef};
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

const NOT_FOUND: &str = "desktop_display_not_found";
const NOT_GRANTED: &str = "desktop_reference_not_granted";

/// A display with this ceiling, bound to `bound`.
async fn display_of(w: &World, ceiling: &[&StationId], bound: &StationId) -> String {
    let id =
        w.f.store
            .create_display(NewDisplay {
                name: "Wall".to_owned(),
                ceiling: ceiling.iter().map(|s| (*s).clone()).collect(),
            })
            .await
            .unwrap()
            .display_id;
    w.f.store
        .display_set_binding(
            id.clone(),
            Some(DisplayBinding {
                station_id: bound.clone(),
                monitor: "DP-1".to_owned(),
            }),
        )
        .await
        .unwrap();
    id
}

/// The wall of the fixture station.
async fn wall(w: &World) -> String {
    display_of(w, &[&w.station()], &w.station()).await
}

fn session_of(station: &StationId, session: &str) -> Value {
    json!({"kind": "session", "session": {
        "station_id": station, "session_id": session}})
}

fn artifact_of(station: &StationId, id: &str, version: u32) -> Value {
    json!({"kind": "artifact", "artifact": {
        "station_id": station, "artifact_id": id, "version": version}})
}

fn show(request_id: &str, display: &str, content: Value) -> Value {
    json!({"tool": "wall_show", "arguments": {
        "request_id": request_id, "display_id": display, "content": content}})
}

/// A directive row of the caller's own session on `station`.
fn focus(station: &StationId, session: &str) -> Value {
    json!({"mode": "focus", "pin": false,
        "session": {"station_id": station, "session_id": session},
        "filter": {"stations": [station], "repo": null}})
}

async fn cookie_of(w: &World, display: &str) -> String {
    let (secret, _) =
        w.f.store
            .create_display_session(display.to_owned())
            .await
            .unwrap();
    format!("fd_display={}", secret.expose())
}

/// The board of the world and the cookie of a display login.
async fn served(w: &World, display: &str) -> (BoardClient, String) {
    (
        board(Arc::clone(&w.f.ctx)).await,
        cookie_of(w, display).await,
    )
}

async fn directive(c: &BoardClient, cookie: &str) -> Http {
    c.get("/api/displays/self/directive", &[("Cookie", cookie)])
        .await
}

async fn stored(w: &World, display: &str) -> bool {
    w.f.store
        .display_coordinator_row(display.to_owned())
        .await
        .unwrap()
        .is_some()
}

async fn generation(w: &World, display: &str) -> u64 {
    w.f.store
        .display_directive(display.to_owned())
        .await
        .unwrap()
        .unwrap()
        .generation
}

fn false_policy(method: &'static str) -> Handler {
    except(method, move |_| {
        Some(ok(if method == "desktop.policy_now" {
            json!({"allow_fleet_control": false, "allow_desktop": true,
                "allow_desktop_capture": true, "monitor_allowed": true})
        } else {
            json!({"fleet_control": false, "bound": true, "binding_matches": true})
        }))
    })
}

/// A world with a shown session of the caller and its display.
async fn shown() -> (World, String) {
    let w = world().await;
    let d = wall(&w).await;
    let st = w.station();
    let reply = w
        .call("c1", show("req-1", &d, session_of(&st, "coord-1")))
        .await;
    assert_eq!(reply, json!({"status": "ok", "result": {"generation": 1}}));
    (w, d)
}

async fn note_artifact(w: &World, id: &str, session: Option<&str>) {
    let artifact = Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: session.map(Into::into),
        title: "t".to_owned(),
        kind: ArtifactKind::Html,
        latest_version: 2,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    };
    w.f.store
        .note_artifact(w.station(), &artifact)
        .await
        .unwrap();
}

async fn sql(w: &World, text: String) {
    w.f.store
        .db()
        .call(move |conn| {
            conn.execute_batch(&text)?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_wall_show_stores_a_directive_and_answers_only_its_generation() {
    let w = world().await;
    let d = wall(&w).await;
    let st = w.station();
    let sentinel = "SENTINEL-REQUEST-4711";
    let reply = w
        .call("c1", show(sentinel, &d, session_of(&st, "coord-1")))
        .await;
    assert_eq!(reply, json!({"status": "ok", "result": {"generation": 1}}));
    // The two policies, and no `desktop.act`: nothing reaches the desktop.
    let methods: Vec<String> = w.fake.log().into_iter().map(|(m, _)| m).collect();
    assert_eq!(methods, ["coordinator.policy_now", "desktop.policy_now"]);
    assert_eq!(
        w.fake.params_of("desktop.policy_now")[0],
        json!({"purpose": "desktop", "monitor": null})
    );
    // The operation: done, charged one desktop action, no action hash, no
    // request ID.
    let op =
        w.f.store
            .desktop_op_by_key(&w.b.grant_id, &request_key(sentinel))
            .await
            .unwrap()
            .expect("an operation row");
    assert_eq!((op.tool, op.state), (OpTool::WallShow, OpState::Done));
    assert_eq!(op.result, Some(json!({"generation": 1})));
    assert_eq!(op.resource, json!({"display_id": d}));
    assert_eq!(
        op.charged,
        Charges {
            desktop: 1,
            capture: 0
        }
    );
    assert!(op.action_hash.is_none());
    assert!(!format!("{op:?}").contains(sentinel));
    // The provenance of the stored row.
    let (wd, grant) = (d.clone(), w.b.grant_id.clone());
    let row: (String, String, i64, String, String) =
        w.f.store
            .db()
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT sender_kind, sender_id, sender_generation, sender_station,
                            sender_session FROM display_directives WHERE display_id = ?1",
                    [wd],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            "coordinator".to_owned(),
            grant,
            1,
            st.to_string(),
            "coord-1".to_owned()
        )
    );
    // Two `display.directive` rows of the coordinator: identifiers and codes.
    let rows = w.audit_rows(Some("display.directive")).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let stages: Vec<String> = rows
        .iter()
        .map(|r| {
            serde_json::from_str::<Value>(&r.2).unwrap()["stage"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(stages, ["attempt", "outcome"]);
    assert_eq!(rows[0].1, op.operation_id);
    let detail: Value = serde_json::from_str(&rows[1].2).unwrap();
    assert_eq!(detail["grant_id"], w.b.grant_id.as_str());
    assert_eq!(detail["generation"], 1);
    assert_eq!(detail["display_id"], d.as_str());
    assert_eq!(detail["directive_generation"], 1);
    assert_eq!(
        detail["reference"],
        json!({"kind": "session", "station_id": st, "session_id": "coord-1"})
    );
    let dump = w.audit_text().await;
    assert!(!dump.contains(sentinel), "{dump}");
}

#[tokio::test]
async fn a_lineage_child_is_shown_and_an_outsider_is_refused_also_under_fleet_scope() {
    for fleet in [false, true] {
        let w = world_with(&[GrantClass::Desktop], |g: &mut NewGrant| {
            if fleet {
                g.scope = GrantScope::Fleet;
            }
        })
        .await;
        let (d, st) = (wall(&w).await, w.station());
        // `w1` is a session of the station, but not of this grant.
        let out = w.call("c1", show("r1", &d, session_of(&st, "w1"))).await;
        assert_eq!(code(&out), NOT_GRANTED, "fleet={fleet}");
        w.f.store
            .add_lineage(
                w.b.grant_id.clone(),
                st.clone(),
                "w1".into(),
                "cmd-1".to_owned(),
            )
            .await
            .unwrap();
        let out = w.call("c2", show("r2", &d, session_of(&st, "w1"))).await;
        assert_eq!(out["status"], "ok", "{out}");
        assert!(stored(&w, &d).await);
    }
}

#[tokio::test]
async fn an_artifact_needs_an_allowed_session_and_an_explicit_untombstoned_version() {
    let w = world().await;
    let (d, st) = (wall(&w).await, w.station());
    note_artifact(&w, "mine", Some("coord-1")).await;
    note_artifact(&w, "theirs", Some("w1")).await;
    let ok = w
        .call("c1", show("r1", &d, artifact_of(&st, "mine", 2)))
        .await;
    assert_eq!(ok, json!({"status": "ok", "result": {"generation": 1}}));
    let refused = [
        artifact_of(&st, "theirs", 1),
        artifact_of(&st, "mine", 3),
        artifact_of(&st, "unknown", 1),
    ];
    for (i, content) in refused.into_iter().enumerate() {
        let out = w
            .call(&format!("x{i}"), show(&format!("rx{i}"), &d, content))
            .await;
        assert_eq!(code(&out), NOT_GRANTED, "{i}");
    }
    // An implicit version is no valid argument.
    let out = w
        .call("c3", show("r3", &d, artifact_of(&st, "mine", 0)))
        .await;
    assert_eq!(code(&out), "invalid_params");
    let mut missing = show("r4", &d, artifact_of(&st, "mine", 1));
    missing["arguments"]["content"]["artifact"]
        .as_object_mut()
        .unwrap()
        .remove("version");
    let Outcome::Err(e) = w.call_raw(Some("c4"), w.call_env("c4", missing)).await else {
        panic!("a wire error");
    };
    assert_eq!(e.code, "invalid_params");
    // A tombstoned artifact.
    sql(
        &w,
        format!(
            "INSERT INTO artifact_tombstones
             (station_id, artifact_id, state, attempts, created_at, updated_at)
             VALUES ('{st}', 'mine', 'pending', 0, 1, 1)"
        ),
    )
    .await;
    let out = w
        .call("c5", show("r5", &d, artifact_of(&st, "mine", 2)))
        .await;
    assert_eq!(code(&out), NOT_GRANTED);
}

#[tokio::test]
async fn a_sessionless_capture_is_shown_only_for_this_grant_and_caller() {
    let (w, _, _) = committed_shot(&cap_png(300)).await;
    let (d, st) = (wall(&w).await, w.station());
    let art = artifact_of(&st, ART, 1);
    let out = w.call("w1", show("wr1", &d, art.clone())).await;
    assert_eq!(out["status"], "ok", "{out}");
    // Another grant's capture, an owner capture and another caller's capture.
    let context = |grant: &str, session: &str| {
        json!({"grant_id": grant, "binding_id": "b", "generation": 1,
            "caller_station": st, "caller_session": session, "caller_label": "x"})
        .to_string()
        .replace('\'', "''")
    };
    let cases = [
        format!("'{}'", context("another-grant", "coord-1")),
        "NULL".to_owned(),
        format!("'{}'", context(&w.b.grant_id, "w1")),
    ];
    for (i, value) in cases.into_iter().enumerate() {
        sql(
            &w,
            format!("UPDATE desktop_captures SET coordinator = {value}"),
        )
        .await;
        let out = w
            .call(&format!("n{i}"), show(&format!("nr{i}"), &d, art.clone()))
            .await;
        assert_eq!(code(&out), NOT_GRANTED, "{i}");
    }
}

#[tokio::test]
async fn an_unknown_a_foreign_and_a_revoked_display_are_one_answer_that_charges_nothing() {
    let w = world().await;
    let (st, beta) = (w.station(), w.beta.clone());
    let foreign = display_of(&w, &[&st, &beta], &beta).await;
    let revoked = wall(&w).await;
    assert!(w.f.store.display_revoke(revoked.clone()).await.unwrap());
    let mut replies = Vec::new();
    for (i, display) in ["no-such-display", &foreign, &revoked]
        .into_iter()
        .enumerate()
    {
        let out = w
            .call(
                &format!("c{i}"),
                show(&format!("r{i}"), display, session_of(&st, "coord-1")),
            )
            .await;
        assert_eq!(code(&out), NOT_FOUND);
        replies.push(out);
    }
    assert_eq!(replies[0], replies[1]);
    assert_eq!(replies[1], replies[2]);
    // The refusal rows hold the code alone: no display and no reference.
    let rows = w.audit_rows(Some("display.directive")).await;
    assert_eq!(rows.len(), 3);
    for row in &rows {
        let detail: Value = serde_json::from_str(&row.2).unwrap();
        assert_eq!(detail["stage"], "refused");
        assert_eq!(detail["code"], NOT_FOUND);
        assert!(detail.get("display_id").is_none() && detail.get("reference").is_none());
    }
    let g =
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(g.desktop_used, 0);
    assert!(w.fake.params_of("desktop.act").is_empty());
}

#[tokio::test]
async fn the_display_ceiling_narrows_what_a_wall_can_show() {
    let w = world().await;
    let (st, beta) = (w.station(), w.beta.clone());
    w.f.store
        .apply_snapshot(beta.clone(), "ib".to_owned().into(), 0, vec![card("far")])
        .await
        .unwrap();
    w.f.store
        .add_lineage(
            w.b.grant_id.clone(),
            beta.clone(),
            "far".into(),
            "cmd-far".to_owned(),
        )
        .await
        .unwrap();
    // The wall is bound to the fixture station; `far` is on a grant station
    // that the ceiling does not hold.
    let d = wall(&w).await;
    let out = w.call("c1", show("r1", &d, session_of(&beta, "far"))).await;
    assert_eq!(code(&out), NOT_GRANTED);
    w.f.store
        .display_update(
            d.clone(),
            DisplayChange {
                ceiling: Some(vec![st.clone(), beta.clone()]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    let out = w.call("c2", show("r2", &d, session_of(&beta, "far"))).await;
    assert_eq!(out["status"], "ok", "{out}");
    // The filter is exactly the referenced station.
    let row = w.f.store.display_coordinator_row(d).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&row.directive).unwrap(),
        focus(&beta, "far")
    );
}

#[tokio::test]
async fn a_duplicate_request_returns_the_recorded_state_and_another_payload_conflicts() {
    let (w, d) = shown().await;
    let st = w.station();
    let again = w
        .call("c2", show("req-1", &d, session_of(&st, "coord-1")))
        .await;
    assert_eq!(again["status"], "ok");
    assert_eq!(again["result"]["state"], "done");
    assert_eq!(again["result"]["result"], json!({"generation": 1}));
    // No second write and no second charge.
    assert_eq!(generation(&w, &d).await, 1);
    let g =
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(g.desktop_used, 1);
    let mut pinned = show("req-1", &d, session_of(&st, "coord-1"));
    pinned["arguments"]["pin"] = json!(true);
    let other = w.call("c3", pinned).await;
    assert_eq!(code(&other), "request_id_conflict");
    assert_eq!(generation(&w, &d).await, 1);
}

#[tokio::test]
async fn a_valid_directive_is_delivered_after_both_live_policies() {
    let (w, d) = shown().await;
    let (c, cookie) = served(&w, &d).await;
    let before = w.fake.count("desktop.policy_now");
    let got = directive(&c, &cookie).await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(
        got.body,
        json!({"generation": 1, "directive": focus(&w.station(), "coord-1")})
    );
    // This delivery asked the caller station and the target station.
    assert_eq!(w.fake.count("desktop.policy_now"), before + 1);
    assert_eq!(w.fake.count("coordinator.policy_now"), before + 1);
    // The synchronous readers (the wake task) see `mission` for the same row.
    let view =
        w.f.store
            .display_directive(d.clone())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(view.directive, DirectiveBody::mission());
    let scope = w.f.store.display_scope(d.clone()).await.unwrap().unwrap();
    assert_eq!(scope.stations.len(), 1);
    assert!(stored(&w, &d).await);
}

/// The delivery shows `mission` and the row is gone, with a new generation.
async fn assert_invalidated(w: &World, c: &BoardClient, cookie: &str, d: &str) {
    let got = directive(c, cookie).await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(
        got.body,
        json!({"generation": 2, "directive": {"mode": "mission", "pin": false, "filter": null}})
    );
    assert!(!stored(w, d).await);
}

#[tokio::test]
async fn a_revoked_an_expired_and_an_idle_grant_show_mission_and_invalidate() {
    // Revoked: the revoke itself deletes the row; the delivery is `mission`.
    let (w, d) = shown().await;
    let (c, cookie) = served(&w, &d).await;
    assert!(
        w.f.store
            .revoke_grant(w.b.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert_invalidated(&w, &c, &cookie, &d).await;
    // Idle and past the lifetime: the local pass of the delivery finds it.
    for secs in [31 * 60, 13 * 3600] {
        let (w, d) = shown().await;
        let (c, cookie) = served(&w, &d).await;
        w.f.det.advance(Duration::from_secs(secs));
        assert!(stored(&w, &d).await);
        assert_invalidated(&w, &c, &cookie, &d).await;
    }
}

#[tokio::test]
async fn a_new_generation_a_tombstone_and_a_ceiling_change_show_mission_and_invalidate() {
    // A binding generation that is not the row's.
    let (w, d) = shown().await;
    let (c, cookie) = served(&w, &d).await;
    w.rebind().await;
    assert_invalidated(&w, &c, &cookie, &d).await;
    // A tombstone of the shown artifact.
    let w = world().await;
    let (d, st) = (wall(&w).await, w.station());
    note_artifact(&w, "mine", Some("coord-1")).await;
    let out = w
        .call("c1", show("r1", &d, artifact_of(&st, "mine", 2)))
        .await;
    assert_eq!(out["status"], "ok", "{out}");
    let (c, cookie) = served(&w, &d).await;
    sql(
        &w,
        format!(
            "INSERT INTO artifact_tombstones
             (station_id, artifact_id, state, attempts, created_at, updated_at)
             VALUES ('{st}', 'mine', 'pending', 0, 1, 1)"
        ),
    )
    .await;
    assert_invalidated(&w, &c, &cookie, &d).await;
    // A ceiling that no longer holds the station, changed behind the
    // invalidation of the owner path.
    let (w, d) = shown().await;
    let (c, cookie) = served(&w, &d).await;
    sql(&w, "UPDATE displays SET ceiling = '[]'".to_owned()).await;
    let got = directive(&c, &cookie).await;
    assert_eq!(got.body["directive"]["mode"], "mission", "{}", got.body);
    assert!(!stored(&w, &d).await);
    // The owner's ceiling change deletes the row at once.
    let (w, d) = shown().await;
    let (st, beta) = (w.station(), w.beta.clone());
    w.f.store
        .display_update(
            d.clone(),
            DisplayChange {
                ceiling: Some(vec![st, beta]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    assert!(!stored(&w, &d).await);
}

#[tokio::test]
async fn a_policy_that_says_false_shows_mission_and_invalidates() {
    for method in ["coordinator.policy_now", "desktop.policy_now"] {
        let (w, d) = shown().await;
        let (c, cookie) = served(&w, &d).await;
        w.fake.set(false_policy(method));
        assert_invalidated(&w, &c, &cookie, &d).await;
    }
}

#[tokio::test]
async fn an_uncertain_policy_shows_mission_for_that_response_only_and_keeps_the_row() {
    // A malformed target answer.
    let (w, d) = shown().await;
    let (c, cookie) = served(&w, &d).await;
    w.fake.set(except("desktop.policy_now", |_| {
        Some(ok(json!({"junk": 1})))
    }));
    let got = directive(&c, &cookie).await;
    assert_eq!(
        got.body,
        json!({"generation": 1, "directive": {"mode": "mission", "pin": false, "filter": null}})
    );
    assert!(stored(&w, &d).await);
    // The same row is delivered when the policies answer again.
    w.fake.set(standard());
    let got = directive(&c, &cookie).await;
    assert_eq!(got.body["directive"], focus(&w.station(), "coord-1"));
    // A caller station that does not answer in 3 s.
    w.fake.set(except("coordinator.policy_now", |_| None));
    let got = directive(&c, &cookie).await;
    assert_eq!(got.body["directive"]["mode"], "mission");
    assert!(stored(&w, &d).await);
}

#[tokio::test]
async fn a_deck_switch_that_is_off_shows_mission_and_keeps_the_row() {
    let (w, d) = shown().await;
    let (c, cookie) = served(&w, &d).await;
    let got = directive(&c, &cookie).await;
    assert_eq!(got.body["directive"], focus(&w.station(), "coord-1"));
    // `desktop_control` off: `mission` for this response, the row stays.
    w.live.publish(deck_live(false));
    let before = w.fake.count("desktop.policy_now");
    let got = directive(&c, &cookie).await;
    assert_eq!(
        got.body,
        json!({"generation": 1, "directive": {"mode": "mission", "pin": false, "filter": null}})
    );
    assert!(stored(&w, &d).await);
    assert_eq!(w.fake.count("desktop.policy_now"), before);
    // `fleet_control` off does the same.
    w.live.publish(deck_live(true));
    w.f.store.set_fleet_control(false);
    let got = directive(&c, &cookie).await;
    assert_eq!(got.body["directive"]["mode"], "mission");
    assert!(stored(&w, &d).await);
    // Both on again: the directive is delivered again, after both policies.
    w.f.store.set_fleet_control(true);
    let before = w.fake.count("desktop.policy_now");
    let got = directive(&c, &cookie).await;
    assert_eq!(
        got.body,
        json!({"generation": 1, "directive": focus(&w.station(), "coord-1")})
    );
    assert_eq!(w.fake.count("desktop.policy_now"), before + 1);
}

#[tokio::test]
async fn the_artifact_route_and_the_socket_never_serve_coordinator_content_without_the_checks() {
    // The artifact route asks the policies, and a `false` is the same miss.
    let w = world().await;
    let (d, st) = (wall(&w).await, w.station());
    note_artifact(&w, "mine", Some("coord-1")).await;
    let out = w
        .call("c1", show("r1", &d, artifact_of(&st, "mine", 2)))
        .await;
    assert_eq!(out["status"], "ok", "{out}");
    let (c, cookie) = served(&w, &d).await;
    let path = format!("/api/displays/self/artifact/{st}/mine/2/_");
    let before = w.fake.count("desktop.policy_now");
    let _ = c.get(&path, &[("Cookie", &cookie)]).await;
    assert!(w.fake.count("desktop.policy_now") > before);
    w.fake.set(false_policy("desktop.policy_now"));
    let denied = c.get(&path, &[("Cookie", &cookie)]).await;
    assert_eq!((denied.status, denied.code()), (404, "unknown_artifact"));
    assert!(!stored(&w, &d).await);
    // The socket: the scope of a delivered directive is narrowed to its
    // station; when a policy turns false the scope is the ceiling again.
    let w = world().await;
    let (st, beta) = (w.station(), w.beta.clone());
    let d = display_of(&w, &[&st, &beta], &st).await;
    let out = w
        .call("c1", show("r1", &d, session_of(&st, "coord-1")))
        .await;
    assert_eq!(out["status"], "ok", "{out}");
    let (c, cookie) = served(&w, &d).await;
    let t = c
        .post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
        .await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut ws: Ws = c
        .ws(&format!("/api/ws?ticket={ticket}"), &[])
        .await
        .expect("a socket");
    assert_eq!(ws.next().await.unwrap()["type"], "hello");
    let snapshot = ws.next().await.unwrap();
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(
        snapshot["stations"].as_array().unwrap().len(),
        1,
        "{snapshot}"
    );
    w.fake.set(false_policy("desktop.policy_now"));
    let delta = ws.next_of("delta").await;
    assert_eq!(delta["stations"].as_array().unwrap().len(), 2, "{delta}");
    assert!(!stored(&w, &d).await);
}

#[tokio::test]
async fn the_owner_and_the_station_senders_are_unchanged_and_ask_no_policy() {
    let w = world().await;
    let (d, st) = (wall(&w).await, w.station());
    let body = DirectiveBody::Focus {
        session: SessionRef {
            station_id: st.clone(),
            session_id: "w1".into(),
        },
        pin: false,
        filter: None,
    };
    w.f.store
        .display_directive_set(d.clone(), DirectiveSender::Owner, body.clone())
        .await
        .unwrap();
    let (c, cookie) = served(&w, &d).await;
    let got = directive(&c, &cookie).await;
    assert_eq!(got.body["generation"], 1);
    assert_eq!(
        serde_json::from_value::<DirectiveBody>(got.body["directive"].clone()).unwrap(),
        body
    );
    assert!(w.fake.log().is_empty(), "{:?}", w.fake.log());
    // A coordinator directive replaces the owner's, and the owner's replaces it.
    let out = w
        .call("c1", show("r1", &d, session_of(&st, "coord-1")))
        .await;
    assert_eq!(out["status"], "ok", "{out}");
    w.f.store
        .display_directive_set(d.clone(), DirectiveSender::Owner, body.clone())
        .await
        .unwrap();
    assert!(!stored(&w, &d).await);
    let got = directive(&c, &cookie).await;
    assert_eq!(got.body["generation"], 3);
}
