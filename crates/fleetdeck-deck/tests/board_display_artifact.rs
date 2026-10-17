//! The artifact route of a wall display over loopback HTTPS (R13 P2b, §5A
//! item 6, §5D items 1 and 17, §5E item 3b, DL-221): the checks and their
//! order, the one 404 for every miss, the single-file rule, the inert answer,
//! the live checks after every await, the fixed error texts and the chunk
//! gate of a streamed version.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::board::{BoardClient, Http};
use common::{Fixture, next_request, no_request};
use displays_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::DeckStore;
use fleetdeck_deck::store::display_directives::DirectiveSender;
use fleetdeck_deck::store::displays::DisplayChange;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{Artifact, ArtifactDetail, ArtifactKind, ArtifactVersion};
use fleetdeck_proto::display_directive::{ArtifactRef, DirectiveBody, DirectiveFilter};
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use fleetdeck_proto::session::RepoInfo;
use serde_json::json;
use share_support::*;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const LEAK: &str = "SENTINEL-LEAK-4711";
const CSP: &str = "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; media-src data:; font-src data:";
const OWNER_CSP: &str = "sandbox allow-scripts allow-popups allow-forms allow-modals; default-src * data: blob: 'unsafe-inline' 'unsafe-eval'";
const PAGE: &str = "<h1>a wall page</h1>";
/// A reported size above the 64 MiB cache limit.
const BIG: u64 = 70 * 1024 * 1024;
/// The bytes in one `artifacts.file` answer of the fake.
const STEP: usize = 7;
const LONG: &str = "<p>0123456789 a long page that comes in many chunks</p>";

struct Rig {
    _f: Fixture,
    station: Option<LiveLink>,
    c: BoardClient,
    st: String,
    /// A second paired station (no link).
    other: String,
    display: String,
    cookie: String,
}

fn repo_card(id: &str, root: &str) -> fleetdeck_proto::SessionSummary {
    let mut s = card(id);
    s.repo = Some(RepoInfo {
        root: root.to_owned(),
        origin: None,
    });
    s
}

/// `s1` (repo alpha) and `s2` (repo beta) on the fixture station, and a
/// display with that station in its ceiling.
async fn rig() -> Rig {
    let (f, station, c, _) = setup().await;
    let st = f.station.0.clone();
    let other = c
        .store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id
        .as_str()
        .to_owned();
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![
                repo_card("s1", "/srv/a/alpha"),
                repo_card("s2", "/srv/b/beta"),
            ],
        )
        .await
        .unwrap();
    let (display, cookie) = display_login(&c, "Wall", &[st.as_str()]).await;
    Rig {
        _f: f,
        other,
        station: Some(station),
        c,
        st,
        display,
        cookie,
    }
}

fn artifact(id: &str, session: Option<&str>) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: session.map(Into::into),
        title: format!("Title of {id}"),
        kind: ArtifactKind::Html,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    }
}

impl Rig {
    fn station_id(&self) -> StationId {
        StationId::new(self.st.clone())
    }

    /// The index row, and with `files` the version metadata and the cached
    /// bytes (version 1).
    async fn seed(&self, id: &str, session: Option<&str>, files: &[(&str, &str)]) {
        let store = &self.c.store;
        store
            .note_artifact(self.station_id(), &artifact(id, session))
            .await
            .unwrap();
        if files.is_empty() {
            return;
        }
        let size = files.iter().map(|(_, b)| b.len() as u64).sum();
        store
            .put_artifact_versions(
                self.station_id(),
                vec![ArtifactVersion {
                    artifact_id: id.to_owned(),
                    version: 1,
                    size,
                    sha256: String::new(),
                    files: files.iter().map(|(p, _)| (*p).to_owned()).collect(),
                    created_at: 1,
                    capture: None,
                }],
            )
            .await
            .unwrap();
        for (path, bytes) in files {
            store
                .put_artifact_file(
                    self.station_id(),
                    id.to_owned(),
                    1,
                    (*path).to_owned(),
                    bytes.as_bytes().to_vec(),
                )
                .await
                .unwrap();
        }
    }

    async fn direct_for(
        &self,
        display: &str,
        id: &str,
        version: u32,
        filter: Option<DirectiveFilter>,
    ) {
        let body = DirectiveBody::Artifact {
            artifact: ArtifactRef {
                station_id: self.station_id(),
                artifact_id: id.to_owned(),
                version,
            },
            pin: false,
            filter,
        };
        self.c
            .store
            .display_directive_set(display.to_owned(), DirectiveSender::Owner, body)
            .await
            .unwrap();
    }

    async fn direct(&self, id: &str) {
        self.direct_for(&self.display.clone(), id, 1, None).await;
    }

    fn url(&self, id: &str, version: &str, path: &str) -> String {
        format!(
            "/api/displays/self/artifact/{}/{id}/{version}/{path}",
            self.st
        )
    }

    async fn fetch(&self, id: &str, path: &str) -> Http {
        get_with(&self.c, &self.url(id, "1", path), &self.cookie).await
    }

    async fn quiet(&mut self) {
        let station = self.station.as_mut().expect("the link is held");
        no_request(station, Duration::from_millis(300)).await;
    }

    fn serve(&mut self, specs: Vec<(&str, Spec)>, hook: Option<Hook>) -> Fake {
        let station = self.station.take().expect("the link is held");
        serve(
            station,
            specs
                .into_iter()
                .map(|(id, s)| (id.to_owned(), s))
                .collect(),
            hook,
        )
    }

    async fn set_mission(&self) {
        self.c
            .store
            .display_directive_set(
                self.display.clone(),
                DirectiveSender::Owner,
                DirectiveBody::mission(),
            )
            .await
            .unwrap();
    }
}

/// What the fake station holds for one artifact.
struct Spec {
    files: Vec<(&'static str, &'static str)>,
    /// The size that it reports.
    size: u64,
    /// A station error code that it answers instead.
    error: Option<&'static str>,
}

impl Spec {
    fn ok(files: Vec<(&'static str, &'static str)>) -> Self {
        Self {
            files,
            size: 10,
            error: None,
        }
    }
}

/// What the fake does to the display before it answers the `at`-th request.
struct Hook {
    at: usize,
    store: DeckStore,
    display: String,
    revoke: bool,
}

type Log = Arc<Mutex<Vec<(String, String, u64)>>>;

struct Fake {
    stop: oneshot::Sender<()>,
    task: JoinHandle<LiveLink>,
    /// `(method, path, offset)` of each request that the fake served.
    log: Log,
}

impl Fake {
    fn methods(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(m, _, _)| m.clone())
            .collect()
    }

    async fn stop(self) -> LiveLink {
        let _ = self.stop.send(());
        self.task.await.unwrap()
    }
}

fn serve(mut station: LiveLink, specs: HashMap<String, Spec>, hook: Option<Hook>) -> Fake {
    let (stop, mut stopped) = oneshot::channel::<()>();
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let task = tokio::spawn(async move {
        let mut n = 0;
        loop {
            let req = tokio::select! {
                _ = &mut stopped => return station,
                req = next_request(&mut station) => req,
            };
            n += 1;
            if let Some(h) = &hook
                && h.at == n
            {
                if h.revoke {
                    h.store.display_revoke(h.display.clone()).await.unwrap();
                } else {
                    h.store
                        .display_directive_set(
                            h.display.clone(),
                            DirectiveSender::Owner,
                            DirectiveBody::mission(),
                        )
                        .await
                        .unwrap();
                }
            }
            let id = req.params["artifact_id"].as_str().unwrap().to_owned();
            let spec = specs.get(&id).expect("a known artifact");
            let (path, offset) = (
                req.params["path"].as_str().unwrap_or_default().to_owned(),
                req.params["offset"].as_u64().unwrap_or(0),
            );
            seen.lock()
                .unwrap()
                .push((req.method.clone(), path.clone(), offset));
            let out = if let Some(code) = spec.error {
                Outcome::Err(WireError::new(code, format!("{LEAK} station alpha {code}")))
            } else if req.method == "artifacts.get" {
                let version = ArtifactVersion {
                    artifact_id: id.clone(),
                    version: u32::try_from(req.params["version"].as_u64().unwrap()).unwrap(),
                    size: spec.size,
                    sha256: String::new(),
                    files: spec.files.iter().map(|(p, _)| (*p).to_owned()).collect(),
                    created_at: 1,
                    capture: None,
                };
                Outcome::Ok(json!(ArtifactDetail {
                    artifact: artifact(&id, Some("s1")),
                    version: version.clone(),
                    versions: vec![version],
                }))
            } else {
                assert_eq!(req.method, "artifacts.file");
                let bytes = spec
                    .files
                    .iter()
                    .find(|(p, _)| *p == path)
                    .unwrap()
                    .1
                    .as_bytes();
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

/// The decoded chunks of a chunked body, and whether the body ended with its
/// last chunk.
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

fn miss(r: &Http) {
    assert_eq!(
        (r.status, r.code(), r.body["message"].as_str()),
        (404, "unknown_artifact", Some("No such artifact.")),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn a_matching_directive_serves_the_file_inert_from_the_cache() {
    let mut r = rig().await;
    r.seed("a1", Some("s1"), &[("page.html", PAGE)]).await;
    r.direct("a1").await;
    let res = r.fetch("a1", "page.html").await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.text, PAGE);
    assert_eq!(res.header("content-security-policy"), Some(CSP));
    assert!(!CSP.contains("allow-scripts"));
    assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(res.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert_eq!(res.header("content-disposition"), None);
    // `_` names the one file; the content type comes from its real name.
    let one = r.fetch("a1", "_").await;
    assert_eq!(one.status, 200, "{}", one.text);
    assert_eq!(one.text, PAGE);
    assert_eq!(one.header("content-type"), Some("text/html; charset=utf-8"));
    // A cached file needs no station.
    r.quiet().await;
}

#[tokio::test]
async fn the_five_kinds_are_served_and_another_kind_is_not_displayable() {
    let r = rig().await;
    let kinds = [
        ("k1", "a.html", "text/html; charset=utf-8"),
        ("k2", "a.md", "text/markdown; charset=utf-8"),
        ("k3", "a.png", "image/png"),
        ("k4", "a.svg", "image/svg+xml"),
        ("k5", "a.pdf", "application/pdf"),
    ];
    for (id, file, content_type) in kinds {
        r.seed(id, Some("s1"), &[(file, "BYTES")]).await;
        r.direct(id).await;
        let res = r.fetch(id, file).await;
        assert_eq!(res.status, 200, "{file}: {}", res.text);
        assert_eq!(res.header("content-type"), Some(content_type), "{file}");
        assert_eq!(res.header("content-security-policy"), Some(CSP), "{file}");
        assert_eq!(res.text, "BYTES");
    }
    for (id, file) in [("o1", "a.txt"), ("o2", "a.mp4")] {
        r.seed(id, Some("s1"), &[(file, "BYTES")]).await;
        r.direct(id).await;
        let res = r.fetch(id, file).await;
        assert_eq!((res.status, res.code()), (409, "not_displayable"), "{file}");
    }
}

#[tokio::test]
async fn a_multi_file_version_is_not_displayable_and_no_bytes_are_read() {
    let mut r = rig().await;
    // The deck knows the metadata: no station call at all.
    r.seed(
        "m1",
        Some("s1"),
        &[("page.html", PAGE), ("img/a.png", "PNG")],
    )
    .await;
    r.direct("m1").await;
    let res = r.fetch("m1", "page.html").await;
    assert_eq!((res.status, res.code()), (409, "not_displayable"));
    assert_eq!(
        res.body["message"],
        "This artifact cannot be shown on a wall; open it on your own device."
    );
    r.quiet().await;
    // The deck does not know the metadata: it asks the station for the
    // metadata and for no file, and caches nothing.
    r.seed("m2", Some("s1"), &[]).await;
    r.direct("m2").await;
    let fake = r.serve(
        vec![(
            "m2",
            Spec::ok(vec![("page.html", PAGE), ("img/a.png", "PNG")]),
        )],
        None,
    );
    let res = r.fetch("m2", "page.html").await;
    assert_eq!((res.status, res.code()), (409, "not_displayable"));
    assert_eq!(fake.methods(), ["artifacts.get"]);
    // Only the rows that this test seeded for `m1`.
    assert_eq!(cache_rows(&r.c).await, 2);
}

#[tokio::test]
async fn every_miss_is_the_same_404() {
    let r = rig().await;
    r.seed("a1", Some("s1"), &[("page.html", PAGE)]).await;
    r.seed("a2", Some("s1"), &[("page.html", PAGE)]).await;
    // No directive yet: the display reads `mission`.
    miss(&r.fetch("a1", "page.html").await);
    // A directive of another mode.
    r.set_mission().await;
    miss(&r.fetch("a1", "page.html").await);
    r.direct("a1").await;
    assert_eq!(r.fetch("a1", "page.html").await.status, 200);
    let st = r.st.clone();
    let urls = [
        r.url("a2", "1", "page.html"),
        r.url("a1", "2", "page.html"),
        r.url("a1", "0", "page.html"),
        r.url("a1", "x", "page.html"),
        r.url("a1", "-1", "page.html"),
        r.url("zz", "1", "page.html"),
        r.url("a1", "1", "other.html"),
        r.url("a1", "1", "../page.html"),
        r.url("a1", "1", "a/page.html"),
        r.url("a1", "1", "page.html%00"),
        "/api/displays/self/artifact/not-a-station/a1/1/page.html".to_owned(),
        format!("/api/displays/self/artifact/{st}/a1/99999999999/page.html"),
    ];
    for url in urls {
        miss(&get_with(&r.c, &url, &r.cookie).await);
    }
}

#[tokio::test]
async fn the_station_filter_and_the_repository_filter_narrow_the_route() {
    let r = rig().await;
    r.seed("a1", Some("s1"), &[("page.html", PAGE)]).await;
    r.seed("nosession", None, &[("page.html", PAGE)]).await;
    // A second station in the ceiling, and a filter that names only it.
    let (wide, wide_cookie) = display_login(&r.c, "Wide", &[&r.st, &r.other]).await;
    let filter = |stations: &[&str], repo: Option<&str>| DirectiveFilter {
        stations: stations.iter().map(|s| StationId::new(*s)).collect(),
        repo: repo.map(str::to_owned),
    };
    let fetch = |id: &'static str| {
        let (c, url, cookie) = (&r.c, r.url(id, "1", "page.html"), wide_cookie.clone());
        async move { get_with(c, &url, &cookie).await }
    };
    r.direct_for(&wide, "a1", 1, Some(filter(&[r.other.as_str()], None)))
        .await;
    miss(&fetch("a1").await);
    r.direct_for(&wide, "a1", 1, Some(filter(&[r.st.as_str()], None)))
        .await;
    assert_eq!(fetch("a1").await.status, 200);
    // The repository filter equals the basename of the session's repository.
    r.direct_for(&wide, "a1", 1, Some(filter(&[], Some("alpha"))))
        .await;
    assert_eq!(fetch("a1").await.status, 200);
    r.direct_for(&wide, "a1", 1, Some(filter(&[], Some("beta"))))
        .await;
    miss(&fetch("a1").await);
    // An artifact without a session fails a repository filter, and passes
    // when there is none.
    r.direct_for(&wide, "nosession", 1, Some(filter(&[], Some("alpha"))))
        .await;
    miss(&fetch("nosession").await);
    r.direct_for(&wide, "nosession", 1, None).await;
    assert_eq!(fetch("nosession").await.status, 200);
}

#[tokio::test]
async fn a_change_of_authority_stops_the_route() {
    let r = rig().await;
    r.seed("a1", Some("s1"), &[("page.html", PAGE)]).await;
    r.seed("a2", Some("s1"), &[("page.html", PAGE)]).await;
    // A new directive.
    r.direct("a1").await;
    assert_eq!(r.fetch("a1", "page.html").await.status, 200);
    r.direct("a2").await;
    miss(&r.fetch("a1", "page.html").await);
    assert_eq!(r.fetch("a2", "page.html").await.status, 200);
    // A change of the ceiling deletes the directive.
    r.c.store
        .display_update(
            r.display.clone(),
            DisplayChange {
                ceiling: Some(vec![StationId::new(r.other.clone())]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    miss(&r.fetch("a2", "page.html").await);
    // A revoke ends the login.
    r.c.store
        .display_update(
            r.display.clone(),
            DisplayChange {
                ceiling: Some(vec![r.station_id()]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    r.direct("a2").await;
    assert_eq!(r.fetch("a2", "page.html").await.status, 200);
    r.c.store.display_revoke(r.display.clone()).await.unwrap();
    let res = r.fetch("a2", "page.html").await;
    assert_eq!((res.status, res.code()), (401, "login_required"));
}

#[tokio::test]
async fn only_the_display_cookie_logs_in_this_route() {
    let r = rig().await;
    r.seed("a1", Some("s1"), &[("page.html", PAGE)]).await;
    r.direct("a1").await;
    let h = owner(&r.c).await;
    let url = r.url("a1", "1", "page.html");
    expect(&get(&r.c, &h, &url).await, 401, "login_required");
    let both = format!("{}; {}", h[0].1, r.cookie);
    let res = r.c.get(&url, &[("Cookie", &both)]).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.text, PAGE);
}

#[tokio::test]
async fn an_uncached_version_is_read_from_the_station_once() {
    let mut r = rig().await;
    r.seed("u1", Some("s1"), &[]).await;
    r.direct("u1").await;
    let fake = r.serve(vec![("u1", Spec::ok(vec![("page.html", LONG)]))], None);
    let res = r.fetch("u1", "page.html").await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.text, LONG);
    assert_eq!(res.header("content-security-policy"), Some(CSP));
    let methods = fake.methods();
    assert_eq!(methods[0], "artifacts.get");
    assert!(methods[1..].iter().all(|m| m == "artifacts.file"));
    assert!(methods.len() > 2, "{methods:?}");
    // Now it is cached: the second request needs no station request.
    let before = methods.len();
    let res = r.fetch("u1", "page.html").await;
    assert_eq!(res.text, LONG);
    assert_eq!(fake.methods().len(), before);
}

#[tokio::test]
async fn station_failures_have_fixed_answers_with_no_station_text() {
    let mut r = rig().await;
    let cases = [
        ("e1", "not_found", 404, "unknown_artifact"),
        ("e2", "unknown_artifact", 404, "unknown_artifact"),
        ("e3", "unsupported", 502, "station_error"),
        ("e4", "outside_roots", 502, "station_error"),
        ("e5", "whatever", 502, "station_error"),
    ];
    let mut specs = Vec::new();
    for (id, code, _, _) in cases {
        r.seed(id, Some("s1"), &[]).await;
        specs.push((
            id,
            Spec {
                files: vec![("page.html", PAGE)],
                size: 10,
                error: Some(code),
            },
        ));
    }
    let fake = r.serve(specs, None);
    for (id, code, status, error) in cases {
        r.direct(id).await;
        let res = r.fetch(id, "page.html").await;
        assert_eq!((res.status, res.code()), (status, error), "{code}");
        let text = res.text.as_str();
        assert!(
            !text.contains("SENTINEL") && !text.contains("alpha"),
            "{text}"
        );
    }
    // A station that is off: 409 with the fixed text.
    drop(fake.stop().await);
    r.seed("off", Some("s1"), &[]).await;
    r.direct("off").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let res = r.fetch("off", "page.html").await;
        if res.status == 409 {
            assert_eq!(res.code(), "station_unreachable");
            assert_eq!(res.body["message"], "The station is not connected.");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", res.text);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_change_during_the_station_call_ends_in_a_miss() {
    let mut r = rig().await;
    r.seed("w1", Some("s1"), &[]).await;
    r.direct("w1").await;
    // The directive turns to `mission` while the deck waits for the
    // metadata: the deck checks again after the await and sends no byte.
    let hook = Hook {
        at: 1,
        store: r.c.store.clone(),
        display: r.display.clone(),
        revoke: false,
    };
    let _fake = r.serve(
        vec![("w1", Spec::ok(vec![("page.html", PAGE)]))],
        Some(hook),
    );
    let res = r.fetch("w1", "page.html").await;
    miss(&res);
    assert!(!res.text.contains(PAGE));
    assert_eq!(cache_rows(&r.c).await, 0);
}

#[tokio::test]
async fn a_version_over_the_cache_limit_streams_inert_and_leaves_no_cache_row() {
    let mut r = rig().await;
    r.seed("b1", Some("s1"), &[]).await;
    r.direct("b1").await;
    let spec = Spec {
        files: vec![("page.html", LONG)],
        size: BIG,
        error: None,
    };
    let fake = r.serve(vec![("b1", spec)], None);
    let res = r.fetch("b1", "page.html").await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.header("content-security-policy"), Some(CSP));
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert_eq!(chunks_of(&res), (LONG.to_owned(), true));
    assert_eq!(cache_rows(&r.c).await, 0);
    drop(fake);
}

#[tokio::test]
async fn a_revoke_during_a_streamed_file_cuts_the_body() {
    let mut r = rig().await;
    r.seed("b1", Some("s1"), &[]).await;
    r.direct("b1").await;
    // Request 1 is the metadata, 2 the first chunk, 3 the second: the display
    // is revoked before the second chunk is answered.
    let hook = Hook {
        at: 3,
        store: r.c.store.clone(),
        display: r.display.clone(),
        revoke: true,
    };
    let spec = Spec {
        files: vec![("page.html", LONG)],
        size: BIG,
        error: None,
    };
    let _fake = r.serve(vec![("b1", spec)], Some(hook));
    let res = r.fetch("b1", "page.html").await;
    assert_eq!(res.status, 200, "{}", res.text);
    let (got, complete) = chunks_of(&res);
    assert!(!complete, "the body was cut");
    assert!(LONG.starts_with(&got) && got.len() < LONG.len(), "{got}");
    assert_eq!(cache_rows(&r.c).await, 0);
}

#[tokio::test]
async fn a_streamed_file_is_refused_before_the_response_when_the_directive_ended() {
    let mut r = rig().await;
    r.seed("b1", Some("s1"), &[]).await;
    r.direct("b1").await;
    // The directive turns to `mission` before the first chunk is answered:
    // the gate refuses before the response leaves.
    let hook = Hook {
        at: 2,
        store: r.c.store.clone(),
        display: r.display.clone(),
        revoke: false,
    };
    let spec = Spec {
        files: vec![("page.html", LONG)],
        size: BIG,
        error: None,
    };
    let _fake = r.serve(vec![("b1", spec)], Some(hook));
    miss(&r.fetch("b1", "page.html").await);
}

#[tokio::test]
async fn the_owner_artifact_routes_keep_their_sandbox_policy() {
    let r = rig().await;
    r.seed("a1", Some("s1"), &[("page.html", PAGE)]).await;
    let h = owner(&r.c).await;
    let raw = format!("/api/artifacts/{}/a1/1/raw/page.html", r.st);
    let res = get(&r.c, &h, &raw).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.header("content-security-policy"), Some(OWNER_CSP));
    // A display login does not read the owner route.
    let res = get_with(&r.c, &raw, &r.cookie).await;
    assert_eq!(res.status, 403, "{}", res.text);
    assert_eq!(res.code(), "display_read_only");
}
