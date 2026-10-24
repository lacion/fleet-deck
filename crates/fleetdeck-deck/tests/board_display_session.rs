//! The login session of a wall display is checked again at every
//! authorization gate, and a projection reads the scope after its data (R13
//! P2b/P3b, PRE-PR round 1, F1 and F2): a session that expires or ends
//! during a streamed artifact cuts the next chunk, and a display route
//! answers 401 after a logout while the display stays valid.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::board::{BoardClient, Http};
use common::{Fixture, next_request};
use displays_support::*;
use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::display_directives::DirectiveSender;
use fleetdeck_deck::store::displays::{DisplayChange, DisplayLogin};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{Artifact, ArtifactDetail, ArtifactKind, ArtifactVersion};
use fleetdeck_proto::display_directive::{ArtifactRef, DirectiveBody};
use fleetdeck_proto::link::{Outcome, Scope};
use serde_json::json;
use share_support::*;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const LONG: &str = "<p>0123456789 a long page that comes in many chunks</p>";
/// A reported size above the 64 MiB cache limit, so the file streams.
const BIG: u64 = 70 * 1024 * 1024;
/// The bytes in one `artifacts.file` answer of the fake.
const STEP: usize = 7;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

struct Rig {
    f: Fixture,
    station: Option<LiveLink>,
    c: BoardClient,
    st: String,
    display: String,
    cookie: String,
    login: DisplayLogin,
}

/// A display with the fixture station in its ceiling, the card `s1` and a
/// directive for the streamed artifact `b1`.
async fn rig() -> Rig {
    let (f, station, c, _) = setup().await;
    let st = f.station.0.clone();
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let (display, cookie) = display_login(&c, "Wall", &[st.as_str()]).await;
    let login = c
        .store
        .display_session(&Secret::new(
            cookie.strip_prefix("fd_display=").unwrap().to_owned(),
        ))
        .await
        .unwrap()
        .expect("a live session");
    let station_id = StationId::new(st.clone());
    c.store
        .note_artifact(station_id.clone(), &artifact("b1"))
        .await
        .unwrap();
    c.store
        .display_directive_set(
            display.clone(),
            DirectiveSender::Owner,
            DirectiveBody::Artifact {
                artifact: ArtifactRef {
                    station_id,
                    artifact_id: "b1".to_owned(),
                    version: 1,
                },
                app: false,
                pin: false,
                filter: None,
            },
        )
        .await
        .unwrap();
    Rig {
        f,
        station: Some(station),
        c,
        st,
        display,
        cookie,
        login,
    }
}

fn artifact(id: &str) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: Some("s1".into()),
        title: format!("Title of {id}"),
        kind: ArtifactKind::Html,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    }
}

/// What the fake station does to the session before it answers request 3 (the
/// second chunk): ends it, or moves the deck clock past its expiry.
enum Hook {
    Logout,
    Expire,
}

impl Rig {
    async fn fetch(&self) -> Http {
        let url = format!("/api/displays/self/artifact/{}/b1/1/page.html", self.st);
        get_with(&self.c, &url, &self.cookie).await
    }

    /// The fake station for the artifact `b1` that streams `LONG`.
    fn serve(&mut self, hook: Hook) -> Fake {
        let mut station = self.station.take().expect("the link is held");
        let (stop, mut stopped) = oneshot::channel::<()>();
        let (store, login, det) = (self.c.store.clone(), self.login.clone(), self.f.det.clone());
        let task = tokio::spawn(async move {
            let mut n = 0;
            loop {
                let req = tokio::select! {
                    _ = &mut stopped => return station,
                    req = next_request(&mut station) => req,
                };
                n += 1;
                if n == 3 {
                    match hook {
                        Hook::Logout => {
                            assert!(
                                store
                                    .end_display_session(login.session_hash.clone())
                                    .await
                                    .unwrap()
                            );
                        }
                        Hook::Expire => {
                            let at = store
                                .display_session_expires(login.session_hash.clone())
                                .await
                                .unwrap()
                                .unwrap();
                            let left = u64::try_from(at).unwrap() - det.now_ms() + 1;
                            det.advance(Duration::from_millis(left));
                        }
                    }
                }
                let out = if req.method == "artifacts.get" {
                    let version = ArtifactVersion {
                        artifact_id: "b1".to_owned(),
                        version: 1,
                        size: BIG,
                        sha256: String::new(),
                        files: vec!["page.html".to_owned()],
                        created_at: 1,
                        capture: None,
                    };
                    Outcome::Ok(json!(ArtifactDetail {
                        artifact: artifact("b1"),
                        version: version.clone(),
                        versions: vec![version],
                    }))
                } else {
                    assert_eq!(req.method, "artifacts.file");
                    let bytes = LONG.as_bytes();
                    let start =
                        usize::try_from(req.params["offset"].as_u64().unwrap_or(0)).unwrap();
                    let end = (start + STEP).min(bytes.len());
                    Outcome::Ok(json!({"path": "page.html", "size": bytes.len(),
                        "offset": start, "length": end - start,
                        "data_base64": STANDARD.encode(&bytes[start..end]),
                        "eof": end == bytes.len()}))
                };
                station.control.respond(req.id, out).await.unwrap();
            }
        });
        Fake { stop, task }
    }
}

struct Fake {
    stop: oneshot::Sender<()>,
    task: JoinHandle<LiveLink>,
}

impl Fake {
    async fn stop(self) -> LiveLink {
        let _ = self.stop.send(());
        self.task.await.unwrap()
    }
}

/// The decoded chunks of a chunked body, and whether it ended with its last
/// chunk.
fn chunks_of(r: &Http) -> (String, bool) {
    assert_eq!(r.header("transfer-encoding"), Some("chunked"), "{}", r.text);
    let mut rest = r.text.as_str();
    let mut out = String::new();
    loop {
        let Some((size, tail)) = rest.split_once("\r\n") else {
            return (out, false);
        };
        let size = usize::from_str_radix(size, 16).expect("a hex chunk size");
        if size == 0 {
            return (out, true);
        }
        if tail.len() < size + 2 {
            return (out, false);
        }
        out.push_str(&tail[..size]);
        rest = &tail[size + 2..];
    }
}

fn assert_cut(res: &Http) {
    assert_eq!(res.status, 200, "{}", res.text);
    let (got, complete) = chunks_of(res);
    assert!(!complete, "the body was cut");
    assert!(LONG.starts_with(&got) && got.len() < LONG.len(), "{got}");
}

#[tokio::test]
async fn a_logout_during_a_streamed_artifact_cuts_the_body() {
    let mut r = rig().await;
    let fake = r.serve(Hook::Logout);
    let res = r.fetch().await;
    assert_cut(&res);
    // The display itself stays valid: a new login works.
    assert!(
        r.c.store
            .display_get(r.display.clone())
            .await
            .unwrap()
            .is_some()
    );
    drop(fake.stop().await);
}

#[tokio::test]
async fn a_session_that_expires_during_a_streamed_artifact_cuts_the_body() {
    let mut r = rig().await;
    // Use the session in steps that stay under the idle time, so that only
    // the absolute expiry ends it.
    let at =
        r.c.store
            .display_session_expires(r.login.session_hash.clone())
            .await
            .unwrap()
            .unwrap();
    while u64::try_from(at).unwrap() - r.f.det.now_ms() > 29 * DAY_MS {
        r.f.det.advance(Duration::from_millis(29 * DAY_MS));
        let res = get_with(&r.c, "/api/sessions", &r.cookie).await;
        assert_eq!(res.status, 200, "{}", res.text);
    }
    let fake = r.serve(Hook::Expire);
    let res = r.fetch().await;
    assert_cut(&res);
    drop(fake.stop().await);
}

#[tokio::test]
async fn a_display_route_answers_401_after_a_logout_while_the_display_stays() {
    let r = rig().await;
    for path in ["/api/sessions", "/api/stations", "/api/fleet/extras"] {
        let res = get_with(&r.c, path, &r.cookie).await;
        assert_eq!(res.status, 200, "{path} {}", res.text);
    }
    assert!(
        r.c.store
            .end_display_session(r.login.session_hash.clone())
            .await
            .unwrap()
    );
    for path in ["/api/sessions", "/api/stations", "/api/fleet/extras"] {
        let res = get_with(&r.c, path, &r.cookie).await;
        assert_eq!(res.status, 401, "{path} {}", res.text);
    }
    assert!(
        r.c.store
            .display_get(r.display.clone())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn the_session_check_follows_expiry_logout_and_revoke() {
    let r = rig().await;
    let hash = r.login.session_hash.clone();
    let store = &r.c.store;
    assert!(store.display_session_alive(hash.clone()).await.unwrap());
    assert!(
        !store
            .display_session_alive("unknown".to_owned())
            .await
            .unwrap()
    );
    // A revoke ends every session of the display.
    store.display_revoke(r.display.clone()).await.unwrap();
    assert!(!store.display_session_alive(hash.clone()).await.unwrap());
    // A session past its absolute expiry is not alive.
    let (second, _) = display_login(&r.c, "Second", &[r.st.as_str()]).await;
    let (cookie, expires) = store.create_display_session(second).await.unwrap();
    let hash = store
        .display_session(&Secret::new(cookie.expose().clone()))
        .await
        .unwrap()
        .unwrap()
        .session_hash;
    assert!(store.display_session_alive(hash.clone()).await.unwrap());
    r.f.det.advance(Duration::from_millis(
        u64::try_from(expires).unwrap() - r.f.det.now_ms() + 1,
    ));
    assert!(!store.display_session_alive(hash).await.unwrap());
}

#[tokio::test]
async fn a_narrowed_scope_leaves_no_card_of_the_removed_station_in_any_read() {
    let r = rig().await;
    let other =
        r.c.store
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: "other".to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap()
            .station_id;
    r.f.store
        .apply_snapshot(other.clone(), r.f.inc.clone(), 0, vec![card("o1")])
        .await
        .unwrap();
    let (wide, cookie) = display_login(&r.c, "Both", &[r.st.as_str(), other.as_str()]).await;
    let sessions = get_with(&r.c, "/api/sessions", &cookie).await;
    assert!(sessions.text.contains("\"o1\""), "{}", sessions.text);
    // The ceiling narrows: the same cookie reads only the first station.
    r.c.store
        .display_update(
            wide,
            DisplayChange {
                ceiling: Some(vec![StationId::new(r.st.clone())]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    for path in ["/api/sessions", "/api/stations", "/api/fleet/extras"] {
        let res = get_with(&r.c, path, &cookie).await;
        assert_eq!(res.status, 200, "{path} {}", res.text);
        assert!(!res.text.contains("\"o1\""), "{path} {}", res.text);
        assert!(!res.text.contains(other.as_str()), "{path} {}", res.text);
    }
}
