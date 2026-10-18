//! Byte ranges, HEAD and preconditions of MP4 artifacts (R13 P5b E1): the
//! owner raw route and the public share route, over a real link to a fake
//! station that serves virtual files (the byte at offset `i` is a function of
//! `i`, so a wrong offset shows in the bytes).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::board::{BoardClient, Extras, Http, board_with_extras};
use common::*;
use fleetdeck_deck::http::assets::{CSP, display_csp};
use fleetdeck_deck::http::display_artifact::INERT_CSP;
use fleetdeck_deck::store::auth::secret_hash;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::Inbound;
use fleetdeck_proto::artifacts::{
    Artifact, ArtifactDetail, ArtifactKind, ArtifactVersion, MAX_CHUNK_BYTES,
};
use fleetdeck_proto::link::Outcome;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const MIB: u64 = 1024 * 1024;
/// A 3 MiB MP4: five chunks of at most 716,800 bytes.
const VIDEO: u64 = 3 * MIB;
/// Above the 64 MiB cache limit: never cached.
const BIG: u64 = 70 * MIB;
const SHA: &str = "abababababababababababababababababababababababababababababababab";
const ETAG: &str = "\"sha256-abababababababababababababababababababababababababababababababab\"";

/// The byte of the virtual file at offset `i`: printable ASCII, so that the
/// test client can read a body as text, and not periodic in a chunk.
fn byte(i: u64) -> u8 {
    b'a' + u8::try_from(i % 26).unwrap()
}

/// The bytes `from..to` of a virtual file.
fn bytes(from: u64, to: u64) -> String {
    (from..to).map(|i| char::from(byte(i))).collect()
}

/// `(files, bundle size)` of a fake artifact.
fn spec(id: &str) -> (Vec<&'static str>, u64) {
    match id {
        "video" => (vec!["capture.mp4"], VIDEO),
        "big" => (vec!["big.mp4"], BIG),
        "small" => (vec!["small.mp4"], 5000),
        "png" => (vec!["capture.png"], 1000),
        "pair" => (vec!["clip.mp4", "notes.txt"], 2000),
        other => panic!("no fake artifact {other}"),
    }
}

fn file_size(id: &str, path: &str) -> u64 {
    match (id, path) {
        ("pair", "clip.mp4") => 1500,
        ("pair", _) => 500,
        _ => spec(id).1,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fault {
    None,
    Offset,
    Size,
    Length,
    Eof,
    Base64,
    Short,
    Empty,
    Path,
}

const FAULTS: [Fault; 8] = [
    Fault::Offset,
    Fault::Size,
    Fault::Length,
    Fault::Eof,
    Fault::Base64,
    Fault::Short,
    Fault::Empty,
    Fault::Path,
];

type Hook = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

/// What the fake does next: a fault from the `from`-th file request on, and
/// an action that runs while the `at`-th file request is served.
#[derive(Default)]
struct Plan {
    fault: Option<(Fault, usize)>,
    hook: Option<(usize, Hook)>,
}

type Seen = (String, u64, Option<u64>);

struct Fake {
    stop: oneshot::Sender<()>,
    task: JoinHandle<LiveLink>,
    plan: Arc<Mutex<Plan>>,
    files: Arc<Mutex<Vec<Seen>>>,
    overlap: Arc<AtomicBool>,
}

fn artifact(id: &str) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: None,
        title: format!("Title of {id}"),
        kind: if id == "png" {
            ArtifactKind::Image
        } else {
            ArtifactKind::Other
        },
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    }
}

fn file_answer(id: &str, path: &str, offset: u64, length: Option<u64>, fault: Fault) -> Outcome {
    let size = file_size(id, path);
    let wanted = length.unwrap_or(MAX_CHUNK_BYTES).min(MAX_CHUNK_BYTES);
    let end = offset.saturating_add(wanted).min(size);
    let (from, mut to) = (offset.min(size), end);
    match fault {
        Fault::Short => to = to.saturating_sub(1).max(from),
        Fault::Empty => to = from,
        _ => {}
    }
    let data = bytes(from, to);
    let mut a = json!({"path": path, "size": size, "offset": offset,
        "length": to - from, "data_base64": STANDARD.encode(data.as_bytes()),
        "eof": end == size});
    match fault {
        Fault::Offset => a["offset"] = json!(offset + 1),
        Fault::Size => a["size"] = json!(size + 1),
        Fault::Length => a["length"] = json!(to - from + 1),
        Fault::Eof => a["eof"] = json!(end != size),
        Fault::Base64 => {
            let mut text = STANDARD.encode(data.as_bytes());
            text.replace_range(0..4, "!!!!");
            a["data_base64"] = json!(text);
        }
        Fault::Path => a["path"] = json!("other.mp4"),
        Fault::None | Fault::Short | Fault::Empty => {}
    }
    Outcome::Ok(a)
}

/// Serves `artifacts.get` and `artifacts.file` of the fake artifacts until
/// `stop`.
fn serve(mut station: LiveLink) -> Fake {
    let (stop, mut stopped) = oneshot::channel::<()>();
    let plan: Arc<Mutex<Plan>> = Arc::default();
    let files: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let overlap = Arc::new(AtomicBool::new(false));
    let (p, f, o) = (Arc::clone(&plan), Arc::clone(&files), Arc::clone(&overlap));
    let task = tokio::spawn(async move {
        let mut backlog = VecDeque::new();
        loop {
            let req = match backlog.pop_front() {
                Some(req) => req,
                None => tokio::select! {
                    _ = &mut stopped => return station,
                    req = next_request(&mut station) => req,
                },
            };
            let id = req.params["artifact_id"].as_str().unwrap().to_owned();
            let out = if req.method == "artifacts.get" {
                let (names, size) = spec(&id);
                let version = ArtifactVersion {
                    artifact_id: id.clone(),
                    version: 1,
                    size,
                    sha256: SHA.to_owned(),
                    files: names.iter().map(|n| (*n).to_owned()).collect(),
                    created_at: 1,
                    capture: None,
                };
                Outcome::Ok(json!(ArtifactDetail {
                    artifact: artifact(&id),
                    version: version.clone(),
                    versions: vec![version],
                }))
            } else {
                assert_eq!(req.method, "artifacts.file");
                let path = req.params["path"].as_str().unwrap().to_owned();
                let offset = req.params["offset"].as_u64().unwrap();
                let length = req.params["length"].as_u64();
                let index = {
                    let mut log = f.lock().unwrap();
                    log.push((path.clone(), offset, length));
                    log.len() - 1
                };
                // A second request in flight before this one is answered.
                if let Ok(Some(Inbound::Request(extra))) =
                    tokio::time::timeout(Duration::from_millis(30), station.inbound.recv()).await
                    && extra.method != "station.info"
                {
                    o.store(true, Ordering::SeqCst);
                    backlog.push_back(extra);
                }
                let hook = {
                    let mut plan = p.lock().unwrap();
                    match plan.hook.take() {
                        Some((at, hook)) if at == index => Some(hook),
                        other => {
                            plan.hook = other;
                            None
                        }
                    }
                };
                if let Some(hook) = hook {
                    hook().await;
                }
                let fault = match p.lock().unwrap().fault {
                    Some((fault, from)) if index >= from => fault,
                    _ => Fault::None,
                };
                file_answer(&id, &path, offset, length, fault)
            };
            station.control.respond(req.id, out).await.unwrap();
        }
    });
    Fake {
        stop,
        task,
        plan,
        files,
        overlap,
    }
}

impl Fake {
    fn seen(&self) -> Vec<Seen> {
        self.files.lock().unwrap().clone()
    }

    fn fault(&self, fault: Fault, from: usize) {
        self.plan.lock().unwrap().fault = Some((fault, from));
    }

    fn clear(&self) {
        let mut plan = self.plan.lock().unwrap();
        plan.fault = None;
        plan.hook = None;
        self.files.lock().unwrap().clear();
    }

    fn hook(&self, at: usize, hook: impl FnOnce() -> BoxFuture<'static, ()> + Send + 'static) {
        self.plan.lock().unwrap().hook = Some((at, Box::new(hook)));
    }

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

async fn rig() -> Rig {
    let mut f = fixture().await;
    let station = f.connect().await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    for id in ["video", "big", "small", "png", "pair"] {
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

fn raw(id: &str, path: &str) -> String {
    format!("/api/artifacts/fake/{id}/1/raw/{path}")
}

impl Rig {
    fn owner<'a>(&'a self, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut h = vec![("Cookie", self.cookie.as_str())];
        h.extend_from_slice(extra);
        h
    }

    async fn get(&self, path: &str, extra: &[(&str, &str)]) -> Http {
        self.c.get(path, &self.owner(extra)).await
    }

    async fn head(&self, path: &str, extra: &[(&str, &str)]) -> Http {
        self.c.req("HEAD", path, &self.owner(extra), None).await
    }

    /// A share link of the artifact: the path of its public URL.
    async fn share(&self, id: &str) -> String {
        let h = [
            ("Cookie", self.cookie.as_str()),
            ("X-CSRF-Token", self.csrf.as_str()),
        ];
        let made = self
            .c
            .post(
                &format!("/api/artifacts/fake/{id}/shares"),
                &h,
                json!({"expires_in_days": 7}),
            )
            .await;
        assert_eq!(made.status, 200, "{}", made.text);
        let url = made.body["url"].as_str().unwrap();
        let token = url.trim_end_matches('/').rsplit('/').next().unwrap();
        format!("/a/{token}/")
    }
}

/// The length of the cached file, `None` when it is not cached.
async fn cached(r: &Rig, id: &str, path: &str) -> Option<u64> {
    let (station, id, path) = (r.f.station.clone(), id.to_owned(), path.to_owned());
    r.c.store
        .cached_artifact_len(station, id, 1, path)
        .await
        .unwrap()
}

async fn rget(r: &Rig, id: &str, path: &str, range: &str) -> Http {
    r.get(&raw(id, path), &[("Range", range)]).await
}

async fn ranged(c: &BoardClient, url: &str, range: &str) -> Http {
    c.get(url, &[("Range", range)]).await
}

fn length_of(r: &Http) -> usize {
    r.header("content-length").unwrap().parse().unwrap()
}

/// A 206 with exactly the bytes `from..=to` of the file of `size`.
fn assert_partial(r: &Http, from: u64, to: u64, size: u64) {
    assert_eq!(r.status, 206, "{:.200}", r.text);
    assert_eq!(
        r.header("content-range"),
        Some(format!("bytes {from}-{to}/{size}").as_str())
    );
    assert_eq!(length_of(r) as u64, to - from + 1);
    assert_eq!(r.text.len() as u64, to - from + 1);
    assert!(r.text == bytes(from, to + 1), "the bytes of {from}-{to}");
    assert_video_headers(r);
}

fn assert_video_headers(r: &Http) {
    assert_eq!(r.header("accept-ranges"), Some("bytes"));
    assert_eq!(r.header("content-type"), Some("video/mp4"));
    assert_eq!(r.header("etag"), Some(ETAG));
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert_eq!(r.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(r.header("content-encoding"), None);
}

/// The answer of a body that was cut: the headers came, fewer bytes than
/// the `Content-Length` followed.
fn assert_cut(r: &Http, status: u16) {
    assert_eq!(r.status, status);
    assert!(r.text.len() < length_of(r), "{} bytes", r.text.len());
    assert!(!r.text.contains("station_error"), "no JSON in the body");
}

#[tokio::test]
async fn a_full_get_is_200_with_the_headers_and_one_request_for_each_chunk() {
    let r = rig().await;
    let got = r.get(&raw("video", "capture.mp4"), &[]).await;
    assert_eq!(got.status, 200);
    assert_eq!(length_of(&got) as u64, VIDEO);
    assert_eq!(got.header("content-range"), None);
    assert_video_headers(&got);
    assert!(got.text == bytes(0, VIDEO));
    // One request for each chunk, from the start, and never a cache fill.
    let seen = r.fake.seen();
    let offsets: Vec<u64> = seen.iter().map(|s| s.1).collect();
    assert_eq!(offsets, [0, 716_800, 1_433_600, 2_150_400, 2_867_200]);
    assert!(seen.iter().all(|s| s.2.is_some_and(|l| l <= 716_800)));
    assert!(!r.fake.overlap.load(Ordering::SeqCst));
    assert_eq!(cached(&r, "video", "capture.mp4").await, None);
}

#[tokio::test]
async fn a_range_is_206_416_400_or_ignored_and_the_errors_keep_their_texts() {
    let r = rig().await;
    for (range, from, to) in [
        ("bytes=0-0", 0, 0),
        ("bytes=1000-", 1000, 4999),
        ("bytes=-500", 4500, 4999),
        ("bytes=4990-9999999", 4990, 4999),
        ("bytes=10-19", 10, 19),
    ] {
        assert_partial(&rget(&r, "small", "small.mp4", range).await, from, to, 5000);
    }
    for range in ["bytes=5000000-", "bytes=-0", "bytes=5000-5001"] {
        let got = rget(&r, "small", "small.mp4", range).await;
        assert_eq!((got.status, got.code()), (416, "range_not_satisfiable"));
        assert_eq!(got.header("content-range"), Some("bytes */5000"));
        assert_eq!(got.header("content-type"), Some("application/json"));
        assert_eq!(length_of(&got), got.text.len());
        let said = "The requested byte range is not satisfiable.";
        assert_eq!(got.body["message"], said);
    }
    let long = format!("bytes=0-{}5", "0".repeat(260));
    for bad in [
        "bytes=a-b",
        "bytes=5-2",
        "bytes=99999999999999999999-",
        &long,
    ] {
        let got = rget(&r, "small", "small.mp4", bad).await;
        assert_eq!((got.status, got.code()), (400, "invalid_range"), "{bad}");
        assert_eq!(got.body["message"], "The byte range is not valid.");
    }
    for ignored in ["bytes=0-1,5-6", "items=0-1"] {
        let got = rget(&r, "small", "small.mp4", ignored).await;
        assert_eq!(got.status, 200, "{ignored}");
        assert_eq!(got.header("content-range"), None);
        assert!(got.text == bytes(0, 5000));
        assert_video_headers(&got);
    }
}

#[tokio::test]
async fn head_is_200_with_the_headers_and_no_body_and_reads_no_bytes() {
    let r = rig().await;
    let url = raw("video", "capture.mp4");
    // HEAD ignores Range, even a malformed one.
    for extra in [vec![], vec![("Range", "bytes=5-2")]] {
        let got = r.head(&url, &extra).await;
        assert_eq!(got.status, 200);
        assert_eq!(length_of(&got) as u64, VIDEO);
        assert_video_headers(&got);
        assert_eq!(got.text, "");
    }
    assert!(r.fake.seen().is_empty(), "{:?}", r.fake.seen());
    assert_eq!(cached(&r, "video", "capture.mp4").await, None);
}

#[tokio::test]
async fn a_range_reads_only_from_its_start_one_chunk_at_a_time_and_never_caches() {
    let r = rig().await;
    let got = rget(&r, "video", "capture.mp4", "bytes=2000000-2999999").await;
    assert_partial(&got, 2_000_000, 2_999_999, VIDEO);
    let asked = |r: &Rig| r.fake.seen().iter().map(|s| (s.1, s.2)).collect::<Vec<_>>();
    let chunks = [(2_000_000, Some(716_800)), (2_716_800, Some(283_200))];
    assert_eq!(asked(&r), chunks);
    assert!(!r.fake.overlap.load(Ordering::SeqCst));
    // A suffix reads only its own bytes too.
    r.fake.clear();
    let tail = rget(&r, "video", "capture.mp4", "bytes=-100").await;
    assert_partial(&tail, VIDEO - 100, VIDEO - 1, VIDEO);
    assert_eq!(asked(&r), [(VIDEO - 100, Some(100))]);
    // A version over the cache limit streams its ranges as well.
    let big = rget(&r, "big", "big.mp4", "bytes=69999990-69999999").await;
    assert_partial(&big, 69_999_990, 69_999_999, BIG);
    for (id, file) in [("video", "capture.mp4"), ("big", "big.mp4")] {
        assert_eq!(cached(&r, id, file).await, None);
    }
}

#[tokio::test]
async fn a_cached_version_serves_ranges_from_the_cache_with_no_station_read() {
    let r = rig().await;
    // A share of a small version caches the whole file.
    let public = r.share("video").await;
    assert_eq!(cached(&r, "video", "capture.mp4").await, Some(VIDEO));
    let slice = |path: &'static str, offset, len| {
        let store = r.c.store.clone();
        let station = r.f.station.clone();
        async move {
            store
                .cached_artifact_slice(station, "video".into(), 1, path.into(), offset, len)
                .await
                .unwrap()
        }
    };
    let tail = slice("capture.mp4", VIDEO - 10, 100).await.unwrap();
    assert_eq!(String::from_utf8(tail).unwrap(), bytes(VIDEO - 10, VIDEO));
    assert_eq!(slice("x.mp4", 0, 10).await, None);

    r.fake.clear();
    let url = raw("video", "capture.mp4");
    let mid = rget(&r, "video", "capture.mp4", "bytes=1000000-1999999").await;
    assert_partial(&mid, 1_000_000, 1_999_999, VIDEO);
    let full = r.get(&url, &[]).await;
    assert_eq!(full.status, 200);
    assert!(full.text == bytes(0, VIDEO));
    assert_partial(
        &ranged(&r.c, &public, "bytes=-50").await,
        VIDEO - 50,
        VIDEO - 1,
        VIDEO,
    );
    assert!(r.fake.seen().is_empty(), "{:?}", r.fake.seen());

    // The station goes away: the cached file still serves ranges.
    let Rig {
        f, c, fake, cookie, ..
    } = r;
    f.drop_link(fake.stop().await).await;
    let h = [("Cookie", cookie.as_str()), ("Range", "bytes=5-9")];
    assert_partial(&c.get(&url, &h).await, 5, 9, VIDEO);
}

#[tokio::test]
async fn an_uncached_video_of_a_station_that_is_off_is_409_but_head_answers_from_metadata() {
    let r = rig().await;
    let url = raw("small", "small.mp4");
    // The first HEAD stores the metadata.
    assert_eq!(r.head(&url, &[]).await.status, 200);
    let Rig {
        f, c, fake, cookie, ..
    } = r;
    f.drop_link(fake.stop().await).await;
    let h = [("Cookie", cookie.as_str())];
    let got = c.get(&url, &h).await;
    assert_eq!((got.status, got.code()), (409, "station_unreachable"));
    assert_eq!(got.body["message"], "The station is not connected.");
    let head = c.req("HEAD", &url, &h, None).await;
    assert_eq!((head.status, length_of(&head)), (200, 5000));
}

#[tokio::test]
async fn a_station_answer_that_is_not_valid_is_502_before_the_headers() {
    let r = rig().await;
    for fault in FAULTS {
        for range in ["bytes=0-4999", "bytes=100-199"] {
            r.fake.clear();
            r.fake.fault(fault, 0);
            let got = rget(&r, "small", "small.mp4", range).await;
            let code = (got.status, got.code());
            assert_eq!(code, (502, "station_error"), "{fault:?} {range}");
            let text = "The station returned invalid artifact data.";
            assert_eq!(got.body["message"], text);
            assert_eq!(got.header("content-range"), None);
        }
    }
}

#[tokio::test]
async fn a_station_answer_that_is_not_valid_after_the_headers_cuts_the_body() {
    let r = rig().await;
    for fault in FAULTS {
        for (range, status) in [("bytes=0-3145727", 206), ("bytes=100-2999999", 206)] {
            r.fake.clear();
            r.fake.fault(fault, 1);
            let got = rget(&r, "video", "capture.mp4", range).await;
            assert_cut(&got, status);
            // Only the first chunk came, and it is the right bytes.
            let from = range
                .split(['=', '-'])
                .nth(1)
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let n = got.text.len() as u64;
            assert!(
                n <= 716_800 && got.text == bytes(from, from + n),
                "{fault:?}"
            );
        }
        r.fake.clear();
        r.fake.fault(fault, 1);
        assert_cut(&r.get(&raw("video", "capture.mp4"), &[]).await, 200);
    }
}

#[tokio::test]
async fn a_tombstone_ends_the_body_and_before_the_headers_is_a_404() {
    let r = rig().await;
    let delete = |id: &'static str| {
        let (store, station) = (r.c.store.clone(), r.f.station.clone());
        move || -> BoxFuture<'static, ()> {
            Box::pin(async move {
                store.artifact_delete(station, id.into()).await.unwrap();
            })
        }
    };
    // After the first chunk: the headers came, the body is cut.
    r.fake.hook(1, delete("video"));
    let got = r.get(&raw("video", "capture.mp4"), &[]).await;
    assert_cut(&got, 200);
    assert!(got.text.len() <= 716_800);
    let after = r.get(&raw("video", "capture.mp4"), &[]).await;
    assert_eq!((after.status, after.code()), (404, "unknown_artifact"));
    assert_eq!(r.head(&raw("video", "capture.mp4"), &[]).await.status, 404);
    // During the first read: a 404 before the headers.
    r.fake.clear();
    r.fake.hook(0, delete("small"));
    let got = r.get(&raw("small", "small.mp4"), &[]).await;
    assert_eq!((got.status, got.code()), (404, "unknown_artifact"));
    assert_eq!(got.header("accept-ranges"), None);
}

#[tokio::test]
async fn an_owner_login_that_ends_after_the_first_chunk_ends_the_body() {
    let r = rig().await;
    let value = r.cookie.split_once('=').unwrap().1.to_owned();
    let (store, session) = (r.c.store.clone(), secret_hash(&value));
    r.fake.hook(1, move || {
        Box::pin(async move {
            assert!(store.end_login(session).await.unwrap());
        })
    });
    let got = rget(&r, "video", "capture.mp4", "bytes=0-3145727").await;
    assert_cut(&got, 206);
    assert!(got.text.len() <= 716_800);
}

#[tokio::test]
async fn a_share_revoked_after_the_first_chunk_ends_the_body() {
    let r = rig().await;
    let public = r.share("big").await;
    let token = public
        .trim_matches('/')
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let ok = ranged(&r.c, &public, "bytes=69000000-69000099").await;
    assert_partial(&ok, 69_000_000, 69_000_099, BIG);
    assert_eq!(cached(&r, "big", "big.mp4").await, None);
    r.fake.clear();
    let (store, station) = (r.c.store.clone(), r.f.station.clone());
    let hash = secret_hash(&token);
    let share_id: String = hash.chars().take(16).collect();
    r.fake.hook(1, move || {
        Box::pin(async move {
            let revoked = store.revoke_artifact_share(station, "big".into(), hash, share_id);
            assert!(revoked.await.unwrap());
        })
    });
    let got = ranged(&r.c, &public, "bytes=1000000-3999999").await;
    assert_cut(&got, 206);
    assert!(got.text.len() <= 716_800);
    // The revoked link is gone.
    assert_eq!(r.c.get(&public, &[]).await.status, 404);
    assert_eq!(r.c.req("HEAD", &public, &[], None).await.status, 404);
}

#[tokio::test]
async fn a_public_share_answers_ranges_head_and_errors_and_counts_one_view_per_open() {
    let r = rig().await;
    let public = r.share("big").await;
    let range = |v: &'static str| ranged(&r.c, &public, v);
    assert_partial(&range("bytes=0-99").await, 0, 99, BIG);
    assert_partial(&range("bytes=100-199").await, 100, 199, BIG);
    assert_partial(&range("bytes=-10").await, BIG - 10, BIG - 1, BIG);
    let bad = range("bytes=x").await;
    assert_eq!((bad.status, bad.code()), (400, "invalid_range"));
    let over = range("bytes=80000000-").await;
    assert_eq!((over.status, over.code()), (416, "range_not_satisfiable"));
    assert_eq!(over.header("content-range"), Some("bytes */73400320"));
    let head = r.c.req("HEAD", &public, &[], None).await;
    assert_eq!((head.status, length_of(&head) as u64), (200, BIG));
    assert_video_headers(&head);
    // The open at byte 0 counts; the seek, the tail and HEAD do not.
    let shares = r.get("/api/artifacts/fake/big", &[]).await;
    assert_eq!(shares.body["shares"][0]["views"], 1, "{}", shares.text);
    // The faults of the station keep their video codes on the public route.
    r.fake.fault(Fault::Size, 0);
    let got = range("bytes=0-9").await;
    assert_eq!((got.status, got.code()), (502, "station_error"));
}

#[tokio::test]
async fn a_png_and_a_two_file_version_are_served_as_before() {
    let r = rig().await;
    let public = r.share("png").await;
    let pair = r.share("pair").await;
    let owner = [
        ("capture.png", raw("png", "capture.png"), 1000),
        ("clip.mp4", raw("pair", "clip.mp4"), 1500),
        ("notes.txt", raw("pair", "notes.txt"), 500),
    ];
    for (name, url, size) in owner {
        let got = r.get(&url, &[("Range", "bytes=0-9")]).await;
        assert_eq!(got.status, 200, "{name}");
        assert!(got.text == bytes(0, size), "{name}");
        for absent in ["accept-ranges", "etag", "content-range"] {
            assert_eq!(got.header(absent), None, "{name} {absent}");
        }
        // HEAD is not a method of the raw route for these files.
        assert_eq!(r.head(&url, &[]).await.status, 404, "{name}");
    }
    assert_eq!(
        rget(&r, "png", "capture.png", "bytes=0-9")
            .await
            .header("content-type"),
        Some("image/png")
    );
    for (url, size) in [(public, 1000), (pair, 1500)] {
        let got = ranged(&r.c, &url, "bytes=0-9").await;
        assert_eq!((got.status, got.header("accept-ranges")), (200, None));
        assert!(got.text == bytes(0, size));
    }
}

#[tokio::test]
async fn preconditions_answer_304_and_412_before_the_range_and_if_range_gates_the_range() {
    let r = rig().await;
    let url = raw("small", "small.mp4");
    let weak = format!("W/{ETAG}");
    let list = format!("\"other\", {ETAG}");
    for value in [ETAG, weak.as_str(), "*", list.as_str()] {
        let got = r.get(&url, &[("If-None-Match", value)]).await;
        assert_eq!(got.status, 304, "{value}");
        assert_eq!(got.header("etag"), Some(ETAG));
        assert_eq!(
            (got.header("content-length"), got.text.as_str()),
            (None, "")
        );
        assert_eq!(r.head(&url, &[("If-None-Match", value)]).await.status, 304);
        // The 304 comes before the Range is read.
        let both = [("If-None-Match", value), ("Range", "bytes=a-b")];
        assert_eq!(r.get(&url, &both).await.status, 304);
    }
    assert!(r.fake.seen().is_empty());
    assert_eq!(
        r.get(&url, &[("If-None-Match", "\"other\"")]).await.status,
        200
    );
    for value in ["\"other\"", weak.as_str()] {
        let got = r.get(&url, &[("If-Match", value)]).await;
        assert_eq!(
            (got.status, got.code()),
            (412, "precondition_failed"),
            "{value}"
        );
        assert_eq!(r.head(&url, &[("If-Match", value)]).await.status, 412);
    }
    for value in [ETAG, "*"] {
        assert_eq!(
            r.get(&url, &[("If-Match", value)]).await.status,
            200,
            "{value}"
        );
    }
    let first = [("If-Match", "\"other\""), ("If-None-Match", "*")];
    assert_eq!(r.get(&url, &first).await.status, 412);
    // If-Range: a strong exact tag lets the Range apply; anything else is 200.
    let ok = [("Range", "bytes=10-19"), ("If-Range", ETAG)];
    assert_partial(&r.get(&url, &ok).await, 10, 19, 5000);
    for value in ["\"other\"", weak.as_str(), "Wed, 21 Oct 2015 07:28:00 GMT"] {
        for range in ["bytes=10-19", "bytes=a-b"] {
            let got = r.get(&url, &[("Range", range), ("If-Range", value)]).await;
            assert_eq!(
                (got.status, got.header("content-range")),
                (200, None),
                "{value}"
            );
            assert!(got.text == bytes(0, 5000));
        }
    }
}

#[test]
fn the_board_csp_allows_media_from_self_and_the_display_policy_is_unchanged() {
    assert!(CSP.contains("media-src 'self'"), "{CSP}");
    assert!(display_csp(None).contains("media-src 'self'"));
    let inert = "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; media-src data:; font-src data:";
    assert_eq!(INERT_CSP, inert);
}
