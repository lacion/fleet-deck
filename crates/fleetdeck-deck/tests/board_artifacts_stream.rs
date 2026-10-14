//! A version over the cache limit is streamed from the station and never
//! cached; a small version is still cached (NV-01). The station is a fake
//! that reports the version size itself, so a small file can stand for a
//! version above 64 MiB.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::board::{BoardClient, Extras, Http, board_with_extras};
use common::*;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::artifacts::{Artifact, ArtifactDetail, ArtifactKind, ArtifactVersion};
use fleetdeck_proto::link::Outcome;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const CSP: &str = "sandbox allow-scripts allow-popups allow-forms allow-modals; default-src * data: blob: 'unsafe-inline' 'unsafe-eval'";
/// A reported size above the 64 MiB cache limit.
const BIG: u64 = 70 * 1024 * 1024;
const PAGE: &str = "<h1>a large page</h1><img src=\"img/a.png\">";
const PNG: &str = "PNGDATA-OF-A-LARGE-VERSION";
/// The bytes in one `artifacts.file` answer of the fake: small, so that a
/// file comes in many chunks.
const STEP: usize = 7;

type Log = Arc<Mutex<Vec<(String, String, u64)>>>;

struct Fake {
    stop: oneshot::Sender<()>,
    task: JoinHandle<LiveLink>,
    /// `(method, path, offset)` of each request that the fake served.
    log: Log,
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

fn files_of() -> Vec<(&'static str, &'static str)> {
    vec![("page.html", PAGE), ("img/a.png", PNG)]
}

/// Serves `artifacts.get` and `artifacts.file` of the artifacts `big`
/// (reports `BIG` bytes) and `small` (reports 10 bytes), until `stop`.
fn serve(mut station: LiveLink) -> Fake {
    let (stop, mut stopped) = oneshot::channel::<()>();
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let task = tokio::spawn(async move {
        loop {
            let req = tokio::select! {
                _ = &mut stopped => return station,
                req = next_request(&mut station) => req,
            };
            let id = req.params["artifact_id"].as_str().unwrap().to_owned();
            let files = files_of();
            let out = if req.method == "artifacts.get" {
                seen.lock()
                    .unwrap()
                    .push((req.method.clone(), String::new(), 0));
                let version = ArtifactVersion {
                    artifact_id: id.clone(),
                    version: 1,
                    size: if id == "big" { BIG } else { 10 },
                    sha256: String::new(),
                    files: files.iter().map(|(p, _)| (*p).to_owned()).collect(),
                    created_at: 1,
                };
                Outcome::Ok(json!(ArtifactDetail {
                    artifact: artifact(&id),
                    version: version.clone(),
                    versions: vec![version],
                }))
            } else {
                assert_eq!(req.method, "artifacts.file");
                let path = req.params["path"].as_str().unwrap();
                let offset = req.params["offset"].as_u64().unwrap();
                seen.lock()
                    .unwrap()
                    .push((req.method.clone(), path.to_owned(), offset));
                let bytes = files.iter().find(|(p, _)| *p == path).unwrap().1.as_bytes();
                let start = usize::try_from(offset).unwrap();
                let end = (start + STEP).min(bytes.len());
                Outcome::Ok(json!({"path": path, "size": bytes.len(), "offset": offset,
                    "length": end - start, "data_base64": STANDARD.encode(&bytes[start..end]),
                    "eof": end == bytes.len()}))
            };
            station.control.respond(req.id, out).await.unwrap();
        }
    });
    Fake { stop, task, log }
}

impl Fake {
    fn file_requests(&self) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _, _)| m == "artifacts.file")
            .count()
    }

    /// Stops the fake and gives the link back.
    async fn stop(self) -> LiveLink {
        let _ = self.stop.send(());
        self.task.await.unwrap()
    }
}

struct Rig {
    f: Fixture,
    c: BoardClient,
    fake: Fake,
    cookie: String,
    csrf: String,
}

impl Rig {
    fn h(&self) -> [(&str, &str); 2] {
        [
            ("Cookie", self.cookie.as_str()),
            ("X-CSRF-Token", self.csrf.as_str()),
        ]
    }
}

async fn rig() -> Rig {
    let mut f = fixture().await;
    let station = f.connect().await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    for id in ["big", "small"] {
        c.store
            .note_artifact(f.station.clone(), &artifact(id))
            .await
            .unwrap();
    }
    let (cookie, csrf) = c.login().await;
    let fake = serve(station);
    Rig {
        f,
        c,
        fake,
        cookie,
        csrf,
    }
}

async fn cache_rows(c: &BoardClient) -> i64 {
    c.store
        .db()
        .call(|conn| {
            Ok(
                conn.query_row("SELECT COUNT(*) FROM artifact_cache", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap()
}

/// The body of an answer. The test client does not decode a chunked body,
/// and a streamed file has no length: it comes chunked.
fn body_of(r: &Http) -> String {
    assert_eq!(r.header("transfer-encoding"), Some("chunked"), "{}", r.text);
    let mut rest = r.text.as_str();
    let mut out = String::new();
    loop {
        let (size, tail) = rest.split_once("\r\n").expect("a chunk size");
        let size = usize::from_str_radix(size, 16).expect("a hex chunk size");
        if size == 0 {
            return out;
        }
        out.push_str(&tail[..size]);
        rest = &tail[size + 2..];
    }
}

fn token_of(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn a_version_over_the_cache_limit_is_streamed_by_the_raw_route_and_leaves_no_cache_row() {
    let r = rig().await;
    let h = r.h();
    let page = r.c.get("/api/artifacts/fake/big/1/raw/page.html", &h).await;
    assert_eq!(page.status, 200, "{}", page.text);
    assert_eq!(body_of(&page), PAGE);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(page.header("content-security-policy"), Some(CSP));
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    let png = r.c.get("/api/artifacts/fake/big/1/raw/img/a.png", &h).await;
    assert_eq!(png.status, 200, "{}", png.text);
    assert_eq!(body_of(&png), PNG);
    assert_eq!(png.header("content-type"), Some("image/png"));
    assert_eq!(png.header("content-security-policy"), None);
    // Each file came in many chunks, and nothing was cached.
    assert!(r.fake.file_requests() > 8, "{:?}", r.fake.log);
    assert_eq!(cache_rows(&r.c).await, 0);
    let cached =
        r.c.store
            .cached_versions(r.f.station.clone(), "big".to_owned())
            .await
            .unwrap();
    assert!(cached.is_empty());
    // The metadata is stored, so the second read does not ask `artifacts.get`.
    let versions =
        r.c.store
            .artifact_versions(r.f.station.clone(), "big".to_owned())
            .await
            .unwrap();
    assert_eq!(versions[0].size, BIG);
    let gets = |f: &Fake| {
        f.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _, _)| m == "artifacts.get")
            .count()
    };
    assert_eq!(gets(&r.fake), 1);
}

#[tokio::test]
async fn a_path_that_is_not_a_file_of_a_large_version_is_not_found() {
    let r = rig().await;
    let h = r.h();
    let got = r.c.get("/api/artifacts/fake/big/1/raw/nope.png", &h).await;
    assert_eq!((got.status, got.code()), (404, "not_found"), "{}", got.text);
    // The first read stored the metadata; the station got no file request.
    assert_eq!(r.fake.file_requests(), 0);
    assert_eq!(cache_rows(&r.c).await, 0);
}

#[tokio::test]
async fn a_small_version_is_still_cached_one_row_for_each_file_and_served_from_the_cache() {
    let r = rig().await;
    let h = r.h();
    let page =
        r.c.get("/api/artifacts/fake/small/1/raw/page.html", &h)
            .await;
    assert_eq!(page.status, 200, "{}", page.text);
    assert_eq!(page.text, PAGE);
    assert_eq!(cache_rows(&r.c).await, 2);
    let asked = r.fake.file_requests();
    let png =
        r.c.get("/api/artifacts/fake/small/1/raw/img/a.png", &h)
            .await;
    assert_eq!(png.text, PNG);
    assert_eq!(r.fake.file_requests(), asked, "the second read is cached");
    let cached =
        r.c.store
            .cached_versions(r.f.station.clone(), "small".to_owned())
            .await
            .unwrap();
    assert_eq!(cached, [1]);
}

#[tokio::test]
async fn a_public_share_of_a_large_version_streams_while_the_station_is_linked_and_is_gone_when_not()
 {
    let r = rig().await;
    let h = r.h();
    let made =
        r.c.post(
            "/api/artifacts/fake/big/shares",
            &h,
            json!({"expires_in_days": 7}),
        )
        .await;
    assert_eq!(made.status, 200, "{}", made.text);
    let tok = token_of(made.body["url"].as_str().unwrap());
    let public = |path: &str| format!("/a/{tok}/{path}");
    let main = r.c.get(&public(""), &[]).await;
    assert_eq!(main.status, 200, "{}", main.text);
    assert_eq!(body_of(&main), PAGE);
    assert_eq!(main.header("content-security-policy"), Some(CSP));
    assert_eq!(body_of(&r.c.get(&public("img/a.png"), &[]).await), PNG);
    assert_eq!(r.c.get(&public("nope.png"), &[]).await.status, 404);
    assert_eq!(cache_rows(&r.c).await, 0);
    // One view for the open of the main file, not one for each file.
    let shares = r.c.get("/api/artifacts/fake/big", &h).await;
    assert_eq!(shares.body["shares"][0]["views"], 1, "{}", shares.text);

    // The station goes away: the share is gone, and nothing was cached.
    let Rig { f, c, fake, .. } = r;
    let station = fake.stop().await;
    f.drop_link(station).await;
    assert_eq!(c.get(&public(""), &[]).await.status, 404);
    assert_eq!(c.get(&public("img/a.png"), &[]).await.status, 404);
    assert_eq!(cache_rows(&c).await, 0);
}
