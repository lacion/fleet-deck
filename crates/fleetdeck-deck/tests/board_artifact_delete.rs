//! The owner's artifact deletion over loopback HTTPS (R13 P5a task C, A4 deck
//! side): `DELETE /api/artifacts/{station}/{id}` and `GET .../deletion` with a
//! fake station, the 404 of every read path after the tombstone, the offline,
//! unsupported and retry states, the retry task, the audit rows and the chunked
//! delivery of a streamed body.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_core::Secret;
use fleetdeck_deck::DeckStore;
use fleetdeck_deck::artifact_deletions;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{
    Artifact, ArtifactDetail, ArtifactKind, ArtifactVersion, MAX_CHUNK_BYTES,
};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};
use tokio::sync::watch;

const PASSWORD: &str = "a long owner password 123";
const PAGE: &str = "<h1>a page</h1>";
/// A reported size above the 64 MiB cache limit.
const BIG: u64 = 70 * 1024 * 1024;
const MIB: usize = 1024 * 1024;

/// What the fake station answers to `artifacts.delete`.
#[derive(Clone, Copy)]
enum Del {
    Deleted,
    Wire(&'static str),
}

/// A deletion of the store that the fake runs when it serves the second file
/// request of a streamed body.
type Hook = (DeckStore, StationId, String);

struct Fake {
    log: Mutex<Vec<(String, Value)>>,
    modes: Mutex<VecDeque<Del>>,
}

impl Fake {
    fn requests(&self, method: &str) -> Vec<Value> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    fn offsets(&self) -> Vec<u64> {
        self.requests("artifacts.file")
            .iter()
            .map(|p| p["offset"].as_u64().unwrap())
            .collect()
    }
}

fn artifact(id: &str) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: None,
        title: format!("Title of {id}"),
        kind: ArtifactKind::Html,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    }
}

/// Serves `artifacts.get`, `artifacts.file` (of `content`) and
/// `artifacts.delete` (one mode for each request, the last one repeats).
fn serve(
    mut station: LiveLink,
    modes: Vec<Del>,
    size: u64,
    content: Vec<u8>,
    hook: Option<Hook>,
) -> Arc<Fake> {
    let fake = Arc::new(Fake {
        log: Mutex::default(),
        modes: Mutex::new(modes.into()),
    });
    let me = Arc::clone(&fake);
    tokio::spawn(async move {
        loop {
            let req = next_request(&mut station).await;
            me.log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((req.method.clone(), req.params.clone()));
            let id = req.params["artifact_id"].as_str().unwrap().to_owned();
            let out = match req.method.as_str() {
                "artifacts.get" => {
                    let version = ArtifactVersion {
                        artifact_id: id.clone(),
                        version: 1,
                        size,
                        sha256: String::new(),
                        files: vec!["page.html".to_owned()],
                        created_at: 1,
                        capture: None,
                    };
                    Outcome::Ok(json!(ArtifactDetail {
                        artifact: artifact(&id),
                        version: version.clone(),
                        versions: vec![version],
                    }))
                }
                "artifacts.file" => {
                    let offset = usize::try_from(req.params["offset"].as_u64().unwrap()).unwrap();
                    if offset > 0
                        && let Some((store, st, hid)) = &hook
                    {
                        store
                            .artifact_delete(st.clone(), hid.clone())
                            .await
                            .unwrap();
                    }
                    let want = req.params["length"]
                        .as_u64()
                        .map_or(MAX_CHUNK_BYTES, |l| l.min(MAX_CHUNK_BYTES));
                    let end = (offset + usize::try_from(want).unwrap()).min(content.len());
                    Outcome::Ok(json!({
                        "path": "page.html", "size": content.len(), "offset": offset,
                        "length": end - offset,
                        "data_base64": STANDARD.encode(&content[offset..end]),
                        "eof": end == content.len(),
                    }))
                }
                "artifacts.delete" => {
                    let mode = {
                        let mut modes = me.modes.lock().unwrap_or_else(PoisonError::into_inner);
                        if modes.len() > 1 {
                            modes.pop_front()
                        } else {
                            modes.front().copied()
                        }
                    };
                    match mode.unwrap_or(Del::Deleted) {
                        Del::Deleted => {
                            Outcome::Ok(json!({ "artifact_id": id, "state": "deleted" }))
                        }
                        Del::Wire(code) => Outcome::Err(WireError::new(code, "the fake says no")),
                    }
                }
                other => panic!("unexpected request {other}"),
            };
            station.control.respond(req.id, out).await.unwrap();
        }
    });
    fake
}

struct Rig {
    f: Fixture,
    c: BoardClient,
    cookie: String,
    csrf: String,
    elevation: String,
    fake: Option<Arc<Fake>>,
}

/// The deck, the owner (with a password and an elevation) and, when
/// `station` has modes, a linked fake station. `skip` is a feature that the
/// station leaves out of its offer.
async fn rig(station: Option<Vec<Del>>, skip: Option<&str>, size: u64, content: Vec<u8>) -> Rig {
    let mut f = fixture().await;
    if let Some(skip) = skip {
        f.station_node.local.features.retain(|x| x.as_str() != skip);
    }
    let link = match &station {
        Some(_) => Some(f.connect().await),
        None => None,
    };
    let c = board(Arc::clone(&f.ctx)).await;
    c.store
        .set_password(Secret::new(PASSWORD.to_owned()))
        .await
        .unwrap();
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let r = c
        .post("/api/auth/elevate", &h, json!({ "password": PASSWORD }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let elevation = r.body["elevation"].as_str().unwrap().to_owned();
    let fake = link.map(|l| serve(l, station.unwrap(), size, content, None));
    Rig {
        f,
        c,
        cookie,
        csrf,
        elevation,
        fake,
    }
}

impl Rig {
    fn h(&self) -> [(&str, &str); 2] {
        [
            ("Cookie", self.cookie.as_str()),
            ("X-CSRF-Token", self.csrf.as_str()),
        ]
    }

    fn station(&self) -> StationId {
        self.f.station.clone()
    }

    /// The index row, the version and the cached page of `id`.
    async fn seed(&self, id: &str) {
        let store = &self.c.store;
        store
            .note_artifact(self.station(), &artifact(id))
            .await
            .unwrap();
        store
            .put_artifact_versions(
                self.station(),
                vec![ArtifactVersion {
                    artifact_id: id.to_owned(),
                    version: 1,
                    size: PAGE.len() as u64,
                    sha256: String::new(),
                    files: vec!["page.html".to_owned()],
                    created_at: 1,
                    capture: None,
                }],
            )
            .await
            .unwrap();
        store
            .put_artifact_file(
                self.station(),
                id.to_owned(),
                1,
                "page.html".to_owned(),
                PAGE.as_bytes().to_vec(),
            )
            .await
            .unwrap();
    }

    async fn delete_with(&self, path: &str, elevation: Option<&str>) -> Http {
        let h = self.h();
        let mut headers = h.to_vec();
        if let Some(e) = elevation {
            headers.push(("X-FleetDeck-Elevation", e));
        }
        self.c.req("DELETE", path, &headers, None).await
    }

    async fn delete(&self, id: &str) -> Http {
        let path = format!("/api/artifacts/fake/{id}");
        self.delete_with(&path, Some(&self.elevation)).await
    }

    async fn deletion(&self, id: &str) -> Http {
        let h = self.h();
        self.c
            .get(&format!("/api/artifacts/fake/{id}/deletion"), &h)
            .await
    }

    async fn get(&self, path: &str) -> Http {
        let h = self.h();
        self.c.get(path, &h).await
    }

    /// The detail of the `artifacts.delete` audit rows.
    async fn audit(&self, result: AuditResult) -> Vec<Option<String>> {
        self.c.audited("artifacts.delete", result).await
    }

    async fn tombstones(&self) -> i64 {
        self.c
            .store
            .db()
            .call(|conn| {
                Ok(
                    conn.query_row("SELECT COUNT(*) FROM artifact_tombstones", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap()
    }

    /// Waits until the deletion of `id` reads `state`.
    async fn wait_state(&self, id: &str, state: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let r = self.deletion(id).await;
            if r.body["state"] == state {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "{}", r.text);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.text);
}

fn token_of(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned()
}

fn gone(r: &Http) {
    assert_eq!(r.status, 404, "{}", r.text);
}

fn no_such(r: &Http) {
    expect(r, 404, "unknown_artifact");
    assert_eq!(r.body["message"], "No such artifact.", "{}", r.text);
}

fn pending(reason: &str) -> Value {
    json!({ "state": "pending", "reason": reason })
}

#[tokio::test]
async fn an_artifact_deleted_on_a_live_station_answers_200_and_every_read_path_answers_404() {
    let r = rig(Some(vec![Del::Deleted]), None, 10, Vec::new()).await;
    r.seed("doc").await;
    r.seed("keep").await;
    // A share and a view link of the artifact, made before the deletion.
    let share =
        r.c.post(
            "/api/artifacts/fake/doc/shares",
            &r.h(),
            json!({ "expires_in_days": 7 }),
        )
        .await;
    assert_eq!(share.status, 200, "{}", share.text);
    let share_token = token_of(share.body["url"].as_str().unwrap());
    let view =
        r.c.post("/api/artifacts/fake/doc/view", &r.h(), json!({}))
            .await;
    assert_eq!(view.status, 200, "{}", view.text);
    let view_token = token_of(view.body["url"].as_str().unwrap());
    assert_eq!(r.c.get(&format!("/a/{share_token}/"), &[]).await.text, PAGE);
    assert_eq!(
        r.get("/api/artifacts/fake/doc/1/raw/page.html").await.text,
        PAGE
    );

    let done = r.delete("doc").await;
    assert_eq!(done.status, 200, "{}", done.text);
    assert_eq!(done.body, json!({ "state": "deleted" }));
    // The station got one request, with the identity.
    let asked = r.fake.as_ref().unwrap().requests("artifacts.delete");
    assert_eq!(asked, [json!({ "artifact_id": "doc" })]);

    // Every read path: 404 or no row.
    let list = r.get("/api/artifacts").await;
    let ids: Vec<&str> = list
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["artifact_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["keep"]);
    no_such(&r.get("/api/artifacts/fake/doc").await);
    no_such(&r.get("/api/artifacts/fake/doc/1/raw/page.html").await);
    no_such(
        &r.c.post(
            "/api/artifacts/fake/doc/shares",
            &r.h(),
            json!({ "expires_in_days": 7 }),
        )
        .await,
    );
    no_such(
        &r.c.post("/api/artifacts/fake/doc/view", &r.h(), json!({}))
            .await,
    );
    gone(&r.c.get(&format!("/a/{share_token}/"), &[]).await);
    gone(&r.c.get(&format!("/a/{share_token}/page.html"), &[]).await);
    gone(&r.c.get(&format!("/a/{view_token}/"), &[]).await);
    // The other artifact is untouched.
    assert_eq!(
        r.get("/api/artifacts/fake/keep/1/raw/page.html").await.text,
        PAGE
    );
    // The deck holds no copy of the deleted artifact.
    let rows =
        r.c.store
            .db()
            .call(|conn| {
                let mut n = 0;
                for table in [
                    "artifacts",
                    "artifact_versions",
                    "artifact_cache",
                    "artifact_shares",
                ] {
                    n += conn.query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE artifact_id = 'doc'"),
                        [],
                        |r| r.get::<_, i64>(0),
                    )?;
                }
                Ok(n)
            })
            .await
            .unwrap();
    assert_eq!(rows, 0);
    // The state is readable, and the audit row names the identity and the
    // result code only.
    let state = r.deletion("doc").await;
    assert_eq!(
        (state.status, &state.body),
        (200, &json!({ "state": "deleted" }))
    );
    let log = r.c.store.audit_log(100).await.unwrap();
    let rows: Vec<_> = log
        .iter()
        .filter(|e| e.action == "artifacts.delete")
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].target.as_deref(),
        Some(format!("{}/doc", r.station()).as_str())
    );
    assert_eq!(rows[0].detail.as_deref(), Some("deleted"));
    assert_eq!(rows[0].result, AuditResult::Ok);
}

#[tokio::test]
async fn an_offline_station_gives_202_offline_and_a_repeated_delete_gives_the_same_state() {
    let r = rig(None, None, 10, Vec::new()).await;
    r.seed("doc").await;
    let first = r.delete("doc").await;
    assert_eq!(first.status, 202, "{}", first.text);
    assert_eq!(first.body, pending("offline"));
    no_such(&r.get("/api/artifacts/fake/doc").await);
    let again = r.delete("doc").await;
    assert_eq!((again.status, &again.body), (202, &pending("offline")));
    assert_eq!(r.tombstones().await, 1);
    let state = r.deletion("doc").await;
    assert_eq!((state.status, &state.body), (202, &pending("offline")));
    let rows = r.audit(AuditResult::Ok).await;
    assert_eq!(
        rows,
        [
            Some("pending:offline".to_owned()),
            Some("pending:offline".to_owned())
        ]
    );
}

#[tokio::test]
async fn a_station_without_the_feature_gives_202_unsupported_and_gets_no_request() {
    let r = rig(
        Some(vec![Del::Deleted]),
        Some(feature::ARTIFACTS_DELETE),
        10,
        Vec::new(),
    )
    .await;
    r.seed("doc").await;
    let done = r.delete("doc").await;
    assert_eq!(done.status, 202, "{}", done.text);
    assert_eq!(done.body, pending("unsupported"));
    assert!(
        r.fake
            .as_ref()
            .unwrap()
            .requests("artifacts.delete")
            .is_empty()
    );
    assert_eq!(
        r.audit(AuditResult::Ok).await,
        [Some("pending:unsupported".to_owned())]
    );
    no_such(&r.get("/api/artifacts/fake/doc/1/raw/page.html").await);
}

#[tokio::test]
async fn a_station_error_gives_202_retry_and_the_timed_retry_reaches_deleted_without_an_audit_row()
{
    let r = rig(
        Some(vec![Del::Wire("busy"), Del::Deleted]),
        None,
        10,
        Vec::new(),
    )
    .await;
    r.seed("doc").await;
    let first = r.delete("doc").await;
    assert_eq!(first.status, 202, "{}", first.text);
    assert_eq!(first.body, pending("retry"));
    // The retry task is not started yet: the row waits for its time.
    let (stop, shutdown) = watch::channel(false);
    let task = artifact_deletions::spawn(r.c.store.clone(), Arc::clone(&r.f.ctx), shutdown);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        r.fake.as_ref().unwrap().requests("artifacts.delete").len(),
        1
    );
    // The first delay is 1 s: the clock moves past it.
    r.f.det.advance(Duration::from_secs(2));
    r.wait_state("doc", "deleted").await;
    assert_eq!(
        r.fake.as_ref().unwrap().requests("artifacts.delete").len(),
        2
    );
    // Only the owner's request is audited.
    assert_eq!(
        r.audit(AuditResult::Ok).await,
        [Some("pending:retry".to_owned())]
    );
    let _ = stop.send(true);
    task.await.unwrap();
}

#[tokio::test]
async fn a_pending_row_is_tried_first_when_the_link_comes_up_after_a_deck_start() {
    let mut r = rig(None, None, 10, Vec::new()).await;
    for id in ["older", "newer"] {
        r.seed(id).await;
    }
    // `older` becomes a retry row that is due; `newer` waits for a link.
    assert_eq!(r.delete("older").await.body, pending("offline"));
    r.c.store
        .tombstone_set(
            r.station(),
            "older".to_owned(),
            fleetdeck_deck::store::artifact_tombstones::TryOutcome::Retry,
            r.c.store.now_ms() - 10_000,
        )
        .await
        .unwrap();
    assert_eq!(r.delete("newer").await.body, pending("offline"));
    // The deck starts its task, and then the link comes up.
    let (stop, shutdown) = watch::channel(false);
    let task = artifact_deletions::spawn(r.c.store.clone(), Arc::clone(&r.f.ctx), shutdown);
    let link = r.f.connect().await;
    let fake = serve(link, vec![Del::Deleted], 10, Vec::new(), None);
    r.fake = Some(Arc::clone(&fake));
    r.wait_state("older", "deleted").await;
    r.wait_state("newer", "deleted").await;
    let order: Vec<String> = fake
        .requests("artifacts.delete")
        .iter()
        .map(|p| p["artifact_id"].as_str().unwrap().to_owned())
        .collect();
    // The row that waited for a link goes before the due retry row.
    assert_eq!(order, ["newer", "older"]);
    let _ = stop.send(true);
    task.await.unwrap();
}

#[tokio::test]
async fn an_unknown_identity_a_missing_elevation_and_an_unknown_station_are_refused() {
    let r = rig(Some(vec![Del::Deleted]), None, 10, Vec::new()).await;
    r.seed("doc").await;
    // Unknown identity: 404, no tombstone, one audit row.
    let unknown = r.delete("nothing").await;
    no_such(&unknown);
    assert_eq!(r.tombstones().await, 0);
    assert_eq!(
        r.audit(AuditResult::Refused).await,
        [Some("unknown_artifact".to_owned())]
    );
    // No elevation, and a token that is none: 400, nothing deleted.
    for elevation in [None, Some("not-an-elevation")] {
        let refused = r.delete_with("/api/artifacts/fake/doc", elevation).await;
        expect(&refused, 400, "elevation_required");
        assert_eq!(refused.body["message"], "a recent sign-in is needed");
        assert!(refused.body.get("keys").is_none(), "{}", refused.text);
    }
    assert_eq!(r.tombstones().await, 0);
    assert_eq!(
        r.get("/api/artifacts/fake/doc/1/raw/page.html").await.text,
        PAGE
    );
    assert!(
        r.fake
            .as_ref()
            .unwrap()
            .requests("artifacts.delete")
            .is_empty()
    );
    // An unknown station.
    let station = r
        .delete_with("/api/artifacts/nowhere/doc", Some(&r.elevation))
        .await;
    expect(&station, 404, "unknown_station");
    // `GET .../deletion`: no deletion for a known and for an unknown artifact.
    for id in ["doc", "nothing"] {
        let r = r.deletion(id).await;
        expect(&r, 404, "no_deletion");
        assert_eq!(r.body["message"], "This artifact has no deletion.");
    }
    // The reads write no audit row: only the refusals above are there.
    let rows = r.c.store.audit_log(100).await.unwrap();
    assert!(
        rows.iter()
            .filter(|e| e.action == "artifacts.delete")
            .all(|e| e.result != AuditResult::Ok)
    );
}

#[tokio::test]
async fn a_streamed_artifact_comes_in_chunks_of_at_most_700_kib() {
    let content = vec![b'a'; 2 * MIB];
    let r = rig(Some(vec![Del::Deleted]), None, BIG, content).await;
    // The station reports a version that is too large to cache.
    r.c.store
        .note_artifact(r.station(), &artifact("doc"))
        .await
        .unwrap();
    let got = r.get("/api/artifacts/fake/doc/1/raw/page.html").await;
    assert_eq!(got.status, 200);
    let sizes = chunk_sizes(&got.text).expect("a complete chunked body");
    let max = usize::try_from(MAX_CHUNK_BYTES).unwrap();
    assert_eq!(sizes.iter().sum::<usize>(), 2 * MIB);
    assert!(sizes.iter().all(|s| *s <= max), "{sizes:?}");
    assert!(sizes.len() >= 3, "{sizes:?}");
}

/// The chunk sizes of a chunked body that ends with the last chunk; `None`
/// for a body that is cut.
fn chunk_sizes(text: &str) -> Option<Vec<usize>> {
    let mut rest = text;
    let mut sizes = Vec::new();
    loop {
        let (size, tail) = rest.split_once("\r\n")?;
        let size = usize::from_str_radix(size, 16).ok()?;
        if size == 0 {
            return Some(sizes);
        }
        if tail.len() < size + 2 {
            return None;
        }
        sizes.push(size);
        rest = &tail[size + 2..];
    }
}

#[tokio::test]
async fn a_deletion_between_two_chunks_of_a_streamed_artifact_cuts_the_body() {
    let mut f = fixture().await;
    let link = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    c.store
        .set_password(Secret::new(PASSWORD.to_owned()))
        .await
        .unwrap();
    let (cookie, csrf) = c.login().await;
    let st = f.station.clone();
    c.store
        .note_artifact(st.clone(), &artifact("doc"))
        .await
        .unwrap();
    // The fake deletes the artifact in the store when it serves the second
    // file request: the first chunk was checked, the second one is not.
    let fake = serve(
        link,
        vec![Del::Deleted],
        BIG,
        vec![b'a'; 2 * MIB],
        Some((c.store.clone(), st, "doc".to_owned())),
    );
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let got = c.get("/api/artifacts/fake/doc/1/raw/page.html", &h).await;
    assert_eq!(got.status, 200, "{}", got.text.len());
    // The body ends with an error, not with the last chunk, and it holds
    // less than the whole file.
    assert!(chunk_sizes(&got.text).is_none(), "{}", got.text.len());
    assert!(got.text.len() < 2 * MIB, "{}", got.text.len());
    // The station was asked for the first two chunks only.
    assert_eq!(fake.offsets(), [0, MAX_CHUNK_BYTES], "{:?}", fake.offsets());
    // The artifact is gone for every read.
    let after = c.get("/api/artifacts/fake/doc", &h).await;
    no_such(&after);
}
