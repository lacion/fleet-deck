//! The capture service with a fake `AppBridge` (B15, design §4.3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::apps::MAX_APP_DOC_BYTES;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Event, EventBody, SessionId};
use fleetdeck_providers::apps::{
    AppBridge, AppCandidate, AppError, AppTool, READ_TIMEOUT, ToolAnswer,
};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::{Capture, sanitize};
use crate::store::apps::AppLookup;
use crate::store::{NewSession, OfflineApprovalPolicy, ReadResult, Store, StoreConfig};

const S1: &str = "s1";
const MIME: &str = "text/html;profile=mcp-app";
const URI: &str = "ui://demo/counter.html";

/// Counts how many reads run at the same time, and the most.
#[derive(Default)]
struct Gauge {
    live: AtomicUsize,
    max: AtomicUsize,
}

impl Gauge {
    fn enter(&self) {
        let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
    }

    fn exit(&self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    fn max(&self) -> usize {
        self.max.load(Ordering::SeqCst)
    }
}

/// A bridge that answers from tables.
struct Fake {
    tools: Mutex<Result<Vec<AppTool>, AppError>>,
    contents: Mutex<HashMap<(String, String), Result<Value, AppError>>>,
    reads: Mutex<Vec<(String, String)>>,
    tools_calls: AtomicUsize,
    /// When set, a read waits for a permit before it answers.
    gate: Option<Arc<Semaphore>>,
    /// When set, a read never answers.
    hang: bool,
    own: Gauge,
    shared: Arc<Gauge>,
}

impl Fake {
    fn new() -> Self {
        Self {
            tools: Mutex::new(Ok(Vec::new())),
            contents: Mutex::new(HashMap::new()),
            reads: Mutex::new(Vec::new()),
            tools_calls: AtomicUsize::new(0),
            gate: None,
            hang: false,
            own: Gauge::default(),
            shared: Arc::new(Gauge::default()),
        }
    }

    fn with_tools(self, tools: Vec<AppTool>) -> Self {
        *self.tools.lock().unwrap() = Ok(tools);
        self
    }

    fn with_resource(self, server: &str, uri: &str, answer: Value) -> Self {
        self.contents
            .lock()
            .unwrap()
            .insert((server.to_owned(), uri.to_owned()), Ok(answer));
        self
    }

    fn reads(&self) -> Vec<(String, String)> {
        self.reads.lock().unwrap().clone()
    }
}

#[async_trait]
impl AppBridge for Fake {
    async fn tools(&self) -> Result<Vec<AppTool>, AppError> {
        self.tools_calls.fetch_add(1, Ordering::SeqCst);
        self.tools.lock().unwrap().clone()
    }

    async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, AppError> {
        self.reads
            .lock()
            .unwrap()
            .push((server.to_owned(), uri.to_owned()));
        self.own.enter();
        self.shared.enter();
        if self.hang {
            std::future::pending::<()>().await;
        }
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
        self.own.exit();
        self.shared.exit();
        self.contents
            .lock()
            .unwrap()
            .get(&(server.to_owned(), uri.to_owned()))
            .cloned()
            .unwrap_or_else(|| Err(AppError::Answered("no such resource".to_owned())))
    }

    async fn call_tool(
        &self,
        _server: &str,
        _tool: &str,
        _arguments: Value,
    ) -> Result<ToolAnswer, AppError> {
        Err(AppError::Unsupported)
    }
}

/// A resource answer with one app document.
fn doc(html: &str) -> Value {
    json!({"contents": [{
        "uri": URI,
        "mimeType": MIME,
        "text": html,
        "_meta": {"ui": {
            "csp": {"connectDomains": ["https://api.example.com", "*", "https://x.com/path"]},
            "prefersBorder": true,
        }},
    }]})
}

fn tool(server: &str, name: &str, uri: Option<&str>) -> AppTool {
    AppTool {
        server: server.to_owned(),
        name: name.to_owned(),
        title: None,
        read_only: false,
        visibility: None,
        resource_uri: uri.map(str::to_owned),
    }
}

fn codex(server: &str, uri: &str) -> AppCandidate {
    AppCandidate {
        node_id: "node-1".into(),
        server: Some(server.to_owned()),
        server_hint: None,
        tool: "show_counter".to_owned(),
        resource_uri: Some(uri.to_owned()),
        arguments: json!({"start": 1}),
        result: Some(json!({"content": [{"type": "text", "text": "2"}]})),
    }
}

fn claude(full: &str, hint: Option<&str>) -> AppCandidate {
    AppCandidate {
        node_id: "node-2".into(),
        server: None,
        server_hint: hint.map(str::to_owned),
        tool: full.to_owned(),
        resource_uri: None,
        arguments: json!({}),
        result: None,
    }
}

async fn make_session(s: &Store, id: &str) {
    s.create_session(NewSession {
        session_id: id.into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
}

async fn rig() -> (Store, Capture) {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    make_session(&store, S1).await;
    let capture = Capture::new(store.clone());
    (store, capture)
}

/// Waits until no capture runs or waits.
async fn settle(capture: &Capture) {
    for _ in 0..5000 {
        let stats = capture.stats();
        if stats.running == 0 && stats.queued == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the captures did not finish: {:?}", capture.stats());
}

async fn tool_app_events(s: &Store) -> Vec<Event> {
    let ReadResult::Events { events, .. } = s.read_after(0, 10_000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(ev) if ev.kind_name() == "tool.app" => Some(ev),
            _ => None,
        })
        .collect()
}

async fn offer_and_settle(capture: &Capture, candidate: AppCandidate, fake: &Arc<Fake>) {
    capture.offer(SessionId::from(S1), candidate, Arc::<Fake>::clone(fake));
    settle(capture).await;
}

#[tokio::test]
async fn capture_codex_candidate_stores_and_emits_tool_app() {
    let (store, capture) = rig().await;
    let fake = Arc::new(Fake::new().with_resource("demo", URI, doc("<html>app</html>")));
    offer_and_settle(&capture, codex("demo", URI), &fake).await;
    assert_eq!(fake.reads(), [("demo".to_owned(), URI.to_owned())]);
    let events = tool_app_events(&store).await;
    let [
        Event::ToolApp {
            node_id,
            app_id,
            server,
            tool,
            resource_uri,
            prefers_border,
        },
    ] = events.as_slice()
    else {
        panic!("one tool.app event expected, got {events:?}");
    };
    assert_eq!(node_id.as_str(), "node-1");
    assert_eq!((server.as_str(), tool.as_str()), ("demo", "show_counter"));
    assert_eq!((resource_uri.as_str(), *prefers_border), (URI, Some(true)));
    let AppLookup::Found(meta) = store.app_lookup(S1.into(), app_id.clone()).await.unwrap() else {
        panic!("the row is missing");
    };
    assert_eq!(meta.doc_bytes, 16);
    // The sanitizer dropped `*` and the origin with a path.
    assert_eq!(meta.csp.connect_domains, ["https://api.example.com"]);
    assert_eq!(meta.tool_input, json!({"start": 1}));
    assert_eq!(
        meta.tool_result,
        Some(json!({"content": [{"type": "text", "text": "2"}]}))
    );
}

#[tokio::test]
async fn capture_claude_candidate_resolves_a_unique_pair() {
    let (store, capture) = rig().await;
    let fake = Arc::new(
        Fake::new()
            .with_tools(vec![
                tool("demo", "show_counter", Some(URI)),
                tool("demo", "bump", None),
            ])
            .with_resource("demo", URI, doc("<html>app</html>")),
    );
    offer_and_settle(
        &capture,
        claude("mcp__demo__show_counter", Some("demo")),
        &fake,
    )
    .await;
    let events = tool_app_events(&store).await;
    let [Event::ToolApp { server, tool, .. }] = events.as_slice() else {
        panic!("one tool.app event expected, got {events:?}");
    };
    // The raw tool name, never the full `mcp__` name.
    assert_eq!((server.as_str(), tool.as_str()), ("demo", "show_counter"));
    assert_eq!(fake.reads(), [("demo".to_owned(), URI.to_owned())]);
}

#[tokio::test]
async fn capture_claude_cross_server_collision_stores_nothing() {
    let (store, capture) = rig().await;
    let fake = Arc::new(
        Fake::new()
            .with_tools(vec![
                tool("a", "b__c", Some("ui://a/one.html")),
                tool("a__b", "c", Some("ui://ab/two.html")),
            ])
            .with_resource("a", "ui://a/one.html", doc("<html>one</html>"))
            .with_resource("a__b", "ui://ab/two.html", doc("<html>two</html>")),
    );
    offer_and_settle(&capture, claude("mcp__a__b__c", None), &fake).await;
    assert!(tool_app_events(&store).await.is_empty());
    assert_eq!(store.app_count().await.unwrap(), 0);
    assert!(fake.reads().is_empty(), "no read_resource call");
}

#[tokio::test]
async fn capture_claude_hint_mismatch_stores_nothing() {
    let (store, capture) = rig().await;
    let fake = Arc::new(
        Fake::new()
            .with_tools(vec![tool("demo", "show_counter", Some(URI))])
            .with_resource("demo", URI, doc("<html>app</html>")),
    );
    offer_and_settle(
        &capture,
        claude("mcp__demo__show_counter", Some("other")),
        &fake,
    )
    .await;
    assert!(tool_app_events(&store).await.is_empty());
    assert_eq!(store.app_count().await.unwrap(), 0);
    assert!(fake.reads().is_empty());
}

#[tokio::test]
async fn capture_claude_tools_are_cached_and_a_miss_refreshes_once() {
    let (store, capture) = rig().await;
    let fake = Arc::new(
        Fake::new()
            .with_tools(vec![tool("demo", "show_counter", Some(URI))])
            .with_resource("demo", URI, doc("<html>app</html>")),
    );
    offer_and_settle(&capture, claude("mcp__demo__show_counter", None), &fake).await;
    offer_and_settle(&capture, claude("mcp__demo__show_counter", None), &fake).await;
    assert_eq!(
        fake.tools_calls.load(Ordering::SeqCst),
        1,
        "the second offer used the cache"
    );
    assert_eq!(store.app_count().await.unwrap(), 2);
    // An unknown name asks the provider once more, then stops.
    offer_and_settle(&capture, claude("mcp__demo__missing", None), &fake).await;
    assert_eq!(fake.tools_calls.load(Ordering::SeqCst), 2);
    // A tool without an app stops without a read.
    let reads = fake.reads().len();
    *fake.tools.lock().unwrap() = Ok(vec![tool("demo", "bump", None)]);
    offer_and_settle(&capture, claude("mcp__demo__bump", None), &fake).await;
    assert_eq!(fake.reads().len(), reads);
    assert_eq!(store.app_count().await.unwrap(), 2);
}

#[tokio::test]
async fn capture_refuses_wrong_mime_oversize_blob_before_decode_and_non_utf8() {
    use base64::Engine as _;
    let (store, capture) = rig().await;
    let b64 = base64::engine::general_purpose::STANDARD;
    let one = |uri: &str, item: Value| (uri.to_owned(), json!({"contents": [item]}));
    let max_blob_chars = MAX_APP_DOC_BYTES.div_ceil(3) * 4;
    let cases = vec![
        one(
            "ui://t/wrong-mime",
            json!({"uri": "ui://t/wrong-mime", "mimeType": "text/html", "text": "<html>a</html>"}),
        ),
        one(
            "ui://t/text-over",
            json!({"uri": "ui://t/text-over", "mimeType": MIME,
                   "text": "x".repeat(MAX_APP_DOC_BYTES + 1)}),
        ),
        one(
            // Valid base64, one group over the limit: refused by its length.
            "ui://t/blob-over",
            json!({"uri": "ui://t/blob-over", "mimeType": MIME,
                   "blob": "A".repeat(max_blob_chars + 4)}),
        ),
        one(
            "ui://t/non-utf8",
            json!({"uri": "ui://t/non-utf8", "mimeType": MIME,
                   "blob": b64.encode([0xff_u8, 0xfe, 0xfd])}),
        ),
        one(
            "ui://t/blank",
            json!({"uri": "ui://t/blank", "mimeType": MIME, "text": "  \n\t "}),
        ),
        one(
            "ui://t/other-uri",
            json!({"uri": "ui://t/else", "mimeType": MIME, "text": "<html>a</html>"}),
        ),
    ];
    let mut fake = Fake::new();
    for (uri, answer) in &cases {
        fake = fake.with_resource("demo", uri, answer.clone());
    }
    // Two documents that pass: a text one and a blob one.
    fake = fake
        .with_resource(
            "demo",
            "ui://t/ok-text",
            json!({"contents": [{"uri": "ui://t/ok-text", "mimeType": " Text/HTML ; profile=mcp-app ",
                                  "text": "<html>ok</html>"}]}),
        )
        .with_resource(
            "demo",
            "ui://t/ok-blob",
            json!({"contents": [{"mimeType": MIME, "blob": b64.encode("<html>blob</html>")}]}),
        );
    let fake = Arc::new(fake);
    for (uri, _) in &cases {
        offer_and_settle(&capture, codex("demo", uri), &fake).await;
        assert_eq!(store.app_count().await.unwrap(), 0, "{uri} is refused");
    }
    offer_and_settle(&capture, codex("demo", "ui://t/ok-text"), &fake).await;
    offer_and_settle(&capture, codex("demo", "ui://t/ok-blob"), &fake).await;
    assert_eq!(store.app_count().await.unwrap(), 2);
    assert_eq!(tool_app_events(&store).await.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn capture_timeout_emits_nothing() {
    let (store, capture) = rig().await;
    let mut fake = Fake::new().with_resource("demo", URI, doc("<html>app</html>"));
    fake.hang = true;
    let fake = Arc::new(fake);
    capture.offer(
        SessionId::from(S1),
        codex("demo", URI),
        Arc::<Fake>::clone(&fake),
    );
    tokio::time::sleep(READ_TIMEOUT + Duration::from_secs(1)).await;
    settle(&capture).await;
    assert_eq!(fake.reads().len(), 1, "the read started");
    assert!(tool_app_events(&store).await.is_empty());
    assert_eq!(store.app_count().await.unwrap(), 0);
}

#[test]
fn sanitize_keeps_only_scheme_origins() {
    let long = format!("https://{}.example.com", "a".repeat(250));
    let meta = json!({"ui": {
        "csp": {
            "connectDomains": [
                "https://*.example.com:8443",
                "HTTPS://Example.COM",
                "ws://localhost:8080",
                "wss://stream.example.com",
                "*",
                "'self'",
                "https://example.com/path",
                "\"https://quoted.example.com\"",
                "javascript:alert(1)",
                "https://",
                "https://a.com:123456",
                "https://a.com:",
                "https://*.*.a.com",
                "https://a..com",
                "ftp://a.com",
                "https://a.com?x=1",
                "https://example.com:8443/",
                "https://*.example.com:8443",
                long,
                7,
            ],
            "resourceDomains": ["https://cdn.example.com", "https://cdn.example.com"],
            "frameDomains": ["https://frame.example.com"],
            "baseUriDomains": ["https://base.example.com"],
        },
        "permissions": {"camera": {}},
        "prefersBorder": "yes",
    }});
    let csp = sanitize::csp(Some(&meta));
    assert_eq!(
        csp.connect_domains,
        [
            "https://*.example.com:8443",
            "HTTPS://Example.COM",
            "ws://localhost:8080",
            "wss://stream.example.com",
        ]
    );
    assert_eq!(
        csp.resource_domains,
        ["https://cdn.example.com"],
        "duplicates go"
    );
    assert_eq!(
        sanitize::prefers_border(Some(&meta)),
        None,
        "only a boolean is kept"
    );
    assert_eq!(
        sanitize::prefers_border(Some(&json!({"ui": {"prefersBorder": false}}))),
        Some(false)
    );
    assert_eq!(
        sanitize::csp(None),
        fleetdeck_proto::apps::AppCsp::default()
    );
    // At most 32 per list.
    let many: Vec<String> = (0..40)
        .map(|n| format!("https://h{n}.example.com"))
        .collect();
    let capped = sanitize::csp(Some(&json!({"ui": {"csp": {"resourceDomains": many}}})));
    assert_eq!(capped.resource_domains.len(), 32);
    assert_eq!(capped.resource_domains[31], "https://h31.example.com");
}

#[tokio::test]
async fn capture_limits_queue_and_concurrency() {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    let capture = Capture::new(store.clone());
    let gate = Arc::new(Semaphore::new(0));
    let shared = Arc::new(Gauge::default());
    let mut bridges: HashMap<&str, Arc<Fake>> = HashMap::new();
    for id in ["s1", "s2", "s3", "s4"] {
        make_session(&store, id).await;
        let mut fake = Fake::new().with_resource("demo", URI, doc("<html>app</html>"));
        fake.gate = Some(Arc::clone(&gate));
        fake.shared = Arc::clone(&shared);
        bridges.insert(id, Arc::new(fake));
    }
    let offer = |session: &str| {
        capture.offer(
            SessionId::from(session),
            codex("demo", URI),
            Arc::<Fake>::clone(&bridges[session]),
        );
    };
    for session in ["s1", "s1", "s2", "s2", "s3", "s4"] {
        offer(session);
    }
    // 4 run (2 per session at most), 2 wait.
    for _ in 0..500 {
        if shared.live() == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(shared.live(), 4);
    assert_eq!((capture.stats().running, capture.stats().queued), (4, 2));
    // 40 more of s1: 30 fit in the queue of 32, the newest 10 are dropped.
    for _ in 0..40 {
        offer("s1");
    }
    assert_eq!(capture.stats().queued, 32);
    gate.add_permits(1000);
    settle(&capture).await;
    assert_eq!(shared.max(), 4, "at most 4 captures at a time");
    for fake in bridges.values() {
        assert!(fake.own.max() <= 2, "at most 2 captures per session");
    }
    // 4 + 2 + 30 captures stored a row each.
    assert_eq!(store.app_count().await.unwrap(), 36);
    let reads: usize = bridges.values().map(|f| f.reads().len()).sum();
    assert_eq!(reads, 36);
}
