//! The coordinator preview (R13 P6 §4.4, §4.5): the allowed hosts, the
//! station-made handle, the preview records and `preview_close` with the
//! owner of the handle. The deck answer is a fake.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction as A};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;

use super::Fail;
use super::authority_tests::Harness;
use super::coordinator::{Extras, preview_host_ok, tailnet_host_of};
use super::coordinator_tests::{DECK, FakeCheck, coordinated, ctx, extras, open_gates};
use super::events::{Events, Kind, MAX_PREVIEWS, PreviewOwner, Record};
use super::fake::{Opts, Rig};
use super::tests::{act_err, eventually, on};

const TAILNET: &str = "box.tail1234.ts.net";
const NOT_FOUND_TEXT: &str = "No window opened by this grant matches that handle.";

async fn open(
    rig: &Rig,
    h: &Harness,
    n: u64,
    context: &CoordinatorContext,
    url: &str,
) -> Result<DesktopActResult, Fail> {
    let action = A::BrowserOpen {
        url: url.to_owned(),
        monitor: None,
        session: None,
    };
    let admitted = coordinated(
        h,
        &action,
        n,
        context,
        FakeCheck::ok(),
        open_gates(),
        extras(),
    );
    rig.desktop.act(&on, action, admitted).await
}

async fn close(
    rig: &Rig,
    h: &Harness,
    n: u64,
    context: &CoordinatorContext,
    link: Extras,
    window_id: &str,
) -> Result<DesktopActResult, Fail> {
    let action = A::PreviewClose {
        window_id: window_id.to_owned(),
    };
    let admitted = coordinated(h, &action, n, context, FakeCheck::ok(), open_gates(), link);
    rig.desktop.act(&on, action, admitted).await
}

fn is_handle(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn not_found(e: Fail) -> bool {
    act_err(e) == ("desktop_window_not_found", NOT_FOUND_TEXT.to_owned())
}

fn closes(rig: &Rig) -> usize {
    rig.calls_of("hyprctl")
        .iter()
        .filter(|c| c.args.iter().any(|a| a == "closewindow"))
        .count()
}

// ------------------------------------------------------------------ hosts

#[test]
fn the_allowed_hosts_are_exact_and_compared_without_case() {
    let tail = Some(TAILNET);
    for url in [
        "http://localhost",
        "http://localhost:3000/a?b#c",
        "https://LOCALHOST/",
        "http://127.0.0.1:8080",
        "HTTP://127.0.0.1/x",
        "http://[::1]",
        "http://[::1]:3000/path",
        "http://box.tail1234.ts.net/",
        "https://BOX.Tail1234.TS.net:8443/x",
    ] {
        assert!(preview_host_ok(url, tail), "{url}");
    }
    for url in [
        "http://example.com",
        "http://localhost.example.com",
        "http://notlocalhost",
        "http://x.localhost",
        "http://localhost.",
        "http://127.0.0.1.example.com",
        "http://127.0.0.2",
        "http://0.0.0.0",
        "http://[::2]",
        "http://[0:0:0:0:0:0:0:1]",
        "http://x.box.tail1234.ts.net",
        "http://box.tail1234.ts.net.example.com",
        "http://tail1234.ts.net",
        "http://box.tail1234.ts.net.",
    ] {
        assert!(!preview_host_ok(url, tail), "{url}");
    }
    // Without a known tailnet name only the three loopback hosts count.
    assert!(!preview_host_ok("http://box.tail1234.ts.net", None));
    assert!(!preview_host_ok("http://box.tail1234.ts.net", Some("")));
    assert!(preview_host_ok("http://localhost", None));
    // The tailnet name of the status: lower case, no trailing dot.
    assert_eq!(
        tailnet_host_of("Box.Tail1234.ts.net.").as_deref(),
        Some(TAILNET)
    );
    assert_eq!(tailnet_host_of("."), None);
    assert_eq!(tailnet_host_of(""), None);
}

#[tokio::test]
async fn every_allowed_host_opens_and_the_answer_is_a_handle_without_the_address() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let urls = [
        "http://localhost:3000/a",
        "http://127.0.0.1:3000/",
        "http://[::1]:3000/",
        "https://BOX.Tail1234.ts.net/x",
    ];
    for (n, url) in (1_u64..).zip(urls) {
        let result = open(&rig, &h, n, &ctx(), url).await.unwrap();
        assert_eq!(result.address, None, "{url}");
        let handle = result.window_id.unwrap();
        assert!(is_handle(&handle), "{handle}");
        // The browser got the URL, as in the owner launch.
        let chromium = rig.calls_of("chromium");
        assert_eq!(chromium.len(), usize::try_from(n).unwrap());
        assert_eq!(chromium[chromium.len() - 1].args[0], format!("--app={url}"));
        // The record belongs to this context and this deck.
        let (owner, record) = rig.desktop.events.preview(&handle).unwrap();
        assert_eq!(
            owner,
            PreviewOwner {
                deck_fp: DECK.to_owned(),
                grant_id: "grant-1".to_owned(),
                caller_station: "01HZZZZZZZZZZZZZZZZZZZZZZA".to_owned(),
                caller_session: "01HZZZZZZZZZZZZZZZZZZZZZZB".to_owned(),
            }
        );
        assert_eq!(record.kind, Kind::Preview);
        assert!(record.address.starts_with("0x"));
    }
}

#[tokio::test]
async fn a_foreign_host_is_refused_before_any_program_runs() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    for (n, url) in (1_u64..).zip([
        "http://example.com/",
        "http://localhost.example.com/",
        "http://x.box.tail1234.ts.net/",
        "http://box.tail1234.ts.net.example.com/",
        "http://192.168.1.5:3000/",
    ]) {
        let e = open(&rig, &h, n, &ctx(), url).await.unwrap_err();
        assert_eq!(act_err(e), ("invalid_params", "`url` is not valid".into()));
    }
    // A URL that breaks the grammar keeps the existing text.
    let e = open(&rig, &h, 9, &ctx(), "ftp://localhost/")
        .await
        .unwrap_err();
    assert_eq!(act_err(e), ("invalid_params", "`url` is not valid".into()));
    assert!(rig.calls().is_empty());
}

// ------------------------------------------------------------------ close

fn other_station() -> CoordinatorContext {
    CoordinatorContext {
        caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ"),
        ..ctx()
    }
}

#[tokio::test]
async fn an_own_handle_closes_the_window_and_ends_the_record() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let handle = open(&rig, &h, 1, &ctx(), "http://localhost:3000/")
        .await
        .unwrap()
        .window_id
        .unwrap();
    let (_, record) = rig.desktop.events.preview(&handle).unwrap();
    let result = close(&rig, &h, 2, &ctx(), extras(), &handle).await.unwrap();
    // The result is empty: no address, no handle.
    assert_eq!((result.address, result.window_id), (None, None));
    assert_eq!(closes(&rig), 1);
    let sent = rig.calls_of("hyprctl");
    assert_eq!(
        sent[sent.len() - 1].args,
        [
            "dispatch",
            "closewindow",
            &format!("address:{}", record.address)
        ]
    );
    eventually("the record goes", || {
        rig.desktop.events.preview(&handle).is_none()
    })
    .await;
    // The handle is gone: a second close finds nothing.
    let e = close(&rig, &h, 3, &ctx(), extras(), &handle)
        .await
        .unwrap_err();
    assert!(not_found(e));
    assert_eq!(closes(&rig), 1);
}

#[tokio::test]
async fn a_handle_of_another_owner_or_an_unknown_handle_is_not_found_with_one_text() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let handle = open(&rig, &h, 1, &ctx(), "http://localhost:3000/")
        .await
        .unwrap()
        .window_id
        .unwrap();
    let foreign: Vec<(CoordinatorContext, Extras)> = vec![
        // Another session of the same station and grant.
        (
            CoordinatorContext {
                caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ"),
                ..ctx()
            },
            extras(),
        ),
        // Another grant (a new grant does not match).
        (
            CoordinatorContext {
                grant_id: "grant-2".into(),
                ..ctx()
            },
            extras(),
        ),
        // Another coordinator station.
        (other_station(), extras()),
        // Another deck.
        (
            ctx(),
            Extras {
                deck_fp: "cd00000000000000000000000000000000000000000000000000000000000002"
                    .to_owned(),
                ..extras()
            },
        ),
    ];
    for (n, (context, link)) in (2_u64..).zip(foreign) {
        let e = close(&rig, &h, n, &context, link, &handle)
            .await
            .unwrap_err();
        assert!(not_found(e), "{n}");
    }
    let e = close(&rig, &h, 10, &ctx(), extras(), &"0".repeat(32))
        .await
        .unwrap_err();
    assert!(not_found(e));
    assert_eq!(closes(&rig), 0);
    assert!(rig.desktop.events.preview(&handle).is_some());
    // A renewal keeps the grant id and the binding may move on: it matches.
    let renewed = CoordinatorContext {
        generation: 9,
        binding_id: "binding-2".into(),
        ..ctx()
    };
    close(&rig, &h, 11, &renewed, extras(), &handle)
        .await
        .unwrap();
    assert_eq!(closes(&rig), 1);
}

#[tokio::test]
async fn a_new_signature_a_reconnect_and_an_event_gap_clear_the_handles() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let open_one = |n: u64| {
        let (rig, h) = (&rig, &h);
        async move {
            open(rig, h, n, &ctx(), "http://localhost:3000/")
                .await
                .unwrap()
                .window_id
                .unwrap()
        }
    };
    // A reconnect of the event socket (an EOF).
    let first = open_one(1).await;
    rig.drop_events_connection();
    eventually("the records clear on EOF", || {
        rig.desktop.events.preview(&first).is_none()
    })
    .await;
    let e = close(&rig, &h, 2, &ctx(), extras(), &first)
        .await
        .unwrap_err();
    assert!(not_found(e));
    // A gap in the event stream.
    assert!(
        rig.desktop
            .events
            .wait_connected(Duration::from_secs(10))
            .await
    );
    let second = open_one(3).await;
    rig.flag("drop_events", true);
    eventually("the records clear on a gap", || {
        rig.desktop.events.preview(&second).is_none()
    })
    .await;
    let e = close(&rig, &h, 4, &ctx(), extras(), &second)
        .await
        .unwrap_err();
    assert!(not_found(e));
    // A new compositor signature.
    assert!(
        rig.desktop
            .events
            .wait_connected(Duration::from_secs(10))
            .await
    );
    let third = open_one(5).await;
    rig.desktop.events.ensure(&rig.runtime, "other_signature");
    assert!(rig.desktop.events.preview(&third).is_none());
    let e = close(&rig, &h, 6, &ctx(), extras(), &third)
        .await
        .unwrap_err();
    assert!(not_found(e));
    assert_eq!(closes(&rig), 0);
}

#[tokio::test]
async fn a_reused_address_with_another_class_is_not_closed() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let handle = open(&rig, &h, 1, &ctx(), "http://localhost:3000/")
        .await
        .unwrap()
        .window_id
        .unwrap();
    let (_, record) = rig.desktop.events.preview(&handle).unwrap();
    // The compositor lists the address with another class now.
    std::fs::write(
        rig.dir.path().join("clients.json"),
        serde_json::to_string(&serde_json::json!([
            {"address": record.address, "class": "someone-else"}
        ]))
        .unwrap(),
    )
    .unwrap();
    let e = close(&rig, &h, 2, &ctx(), extras(), &handle)
        .await
        .unwrap_err();
    assert_eq!(act_err(e), ("not_found", "The window is gone.".into()));
    assert_eq!(closes(&rig), 0);
    // The record is dropped with the stale window.
    assert!(rig.desktop.events.preview(&handle).is_none());
}

#[tokio::test]
async fn the_remote_check_comes_before_the_second_read_of_the_record() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let handle = open(&rig, &h, 1, &ctx(), "http://localhost:3000/")
        .await
        .unwrap()
        .window_id
        .unwrap();
    // The handle is removed while the deck answers: the close stops.
    let events = rig.desktop.events.clone();
    let gone = handle.clone();
    let check = FakeCheck::with_hook(Ok(()), move || {
        let (_, record) = events.preview(&gone).unwrap();
        events.forget(&record.address);
    });
    let action = A::PreviewClose {
        window_id: handle.clone(),
    };
    let admitted = coordinated(&h, &action, 2, &ctx(), check, open_gates(), extras());
    let e = rig.desktop.act(&on, action, admitted).await.unwrap_err();
    assert!(not_found(e));
    assert_eq!(closes(&rig), 0);
}

// ---------------------------------------------------------------- records

fn template(class: &str) -> Record {
    Record {
        signature: String::new(),
        address: String::new(),
        class: class.to_owned(),
        kind: Kind::Preview,
        display_id: None,
        monitor: None,
        opener: None,
        launch_id: class.to_owned(),
    }
}

fn owner() -> PreviewOwner {
    PreviewOwner {
        deck_fp: DECK.to_owned(),
        grant_id: "g".to_owned(),
        caller_station: "s".to_owned(),
        caller_session: "x".to_owned(),
    }
}

#[tokio::test]
async fn at_most_256_previews_are_kept_and_the_257th_removes_the_oldest() {
    let tmp = tempfile::tempdir().unwrap();
    let inst = tmp.path().join("hypr/sig");
    std::fs::create_dir_all(&inst).unwrap();
    let listener = UnixListener::bind(inst.join(".socket2.sock")).unwrap();
    let events = Events::new();
    events.ensure(tmp.path(), "sig");
    let (mut peer, _) = listener.accept().await.unwrap();
    assert!(events.wait_connected(Duration::from_secs(2)).await);
    let mut addresses = Vec::new();
    for i in 0..=MAX_PREVIEWS {
        let class = format!("fleetdeck-preview-{i}");
        let rx = events.register(template(&class)).unwrap();
        peer.write_all(format!("openwindow>>{:x},1,{class},t\n", 0x1000 + i).as_bytes())
            .await
            .unwrap();
        let address = rx.await.unwrap();
        events
            .register_preview(&format!("handle-{i}"), owner(), &address)
            .unwrap();
        addresses.push(address);
    }
    // The first one went; the second and the last stay.
    assert!(events.preview("handle-0").is_none());
    assert!(events.preview("handle-1").is_some());
    assert!(events.preview(&format!("handle-{MAX_PREVIEWS}")).is_some());
    // The window record of the removed preview is still there.
    assert!(events.record(&addresses[0]).is_some());
    // A closed window takes its preview with it.
    peer.write_all(format!("closewindow>>{:x}\n", 0x1000 + 1).as_bytes())
        .await
        .unwrap();
    assert!(
        events
            .wait_gone(&addresses[1], Duration::from_secs(2))
            .await
    );
    assert!(events.preview("handle-1").is_none());
    assert!(events.preview("handle-2").is_some());
}

#[tokio::test]
async fn a_preview_needs_a_window_record_of_its_address() {
    let events = Events::new();
    let e = events
        .register_preview("handle", owner(), "0xdead")
        .unwrap_err();
    assert_eq!(act_err(e), ("not_found", "The window is gone.".into()));
    assert!(events.preview("handle").is_none());
}

// ------------------------------------------------ the check before the spawn

/// Answers the first check, then the second as given; it records what the
/// station had done when each check ran.
struct Twice {
    second: Result<(), Fail>,
    calls: std::sync::atomic::AtomicUsize,
    seen: std::sync::Mutex<Vec<(bool, usize)>>,
    runtime: std::path::PathBuf,
    dir: std::path::PathBuf,
}

#[async_trait::async_trait]
impl super::coordinator::DispatchCheck for Twice {
    async fn check(
        &self,
        _phase: fleetdeck_proto::coordinator_desktop_wire::AuthorityPhase,
        _read: Option<&fleetdeck_proto::coordinator_desktop_wire::ProtectedRead>,
    ) -> Result<(), Fail> {
        let log = std::fs::read_to_string(self.dir.join("calls.log")).unwrap_or_default();
        let chromium = log.lines().filter(|l| *l == "== chromium").count();
        let folder = self.runtime.join("fleetdeck").is_dir();
        self.seen.lock().unwrap().push((folder, chromium));
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Ok(())
        } else {
            self.second.clone()
        }
    }
}

fn twice(rig: &Rig, second: Result<(), Fail>) -> std::sync::Arc<Twice> {
    std::sync::Arc::new(Twice {
        second,
        calls: std::sync::atomic::AtomicUsize::new(0),
        seen: std::sync::Mutex::new(Vec::new()),
        runtime: rig.runtime.clone(),
        dir: rig.dir.path().to_owned(),
    })
}

async fn open_with(rig: &Rig, check: std::sync::Arc<Twice>) -> Result<DesktopActResult, Fail> {
    let action = A::BrowserOpen {
        url: "http://localhost:3000/".to_owned(),
        monitor: None,
        session: None,
    };
    let h = Harness::new();
    let admitted = coordinated(&h, &action, 1, &ctx(), check, open_gates(), extras());
    rig.desktop.act(&on, action, admitted).await
}

#[tokio::test]
async fn a_coordinator_preview_checks_again_right_before_the_browser_spawn() {
    let rig = Rig::new(Opts::default());
    let check = twice(&rig, Ok(()));
    open_with(&rig, check.clone()).await.unwrap();
    // The first check is before the profile folder; the second is after it
    // and before the browser started.
    assert_eq!(*check.seen.lock().unwrap(), [(false, 0), (true, 0)]);
    assert_eq!(rig.calls_of("chromium").len(), 1);
}

#[tokio::test]
async fn a_withdrawn_authority_at_the_second_check_spawns_no_browser() {
    let rig = Rig::new(Opts::default());
    let check = twice(&rig, Err(super::coordinator::lost()));
    let e = open_with(&rig, check.clone()).await.unwrap_err();
    assert_eq!(act_err(e).0, "authority_lost");
    assert_eq!(*check.seen.lock().unwrap(), [(false, 0), (true, 0)]);
    assert!(rig.calls_of("chromium").is_empty());
}

#[tokio::test]
async fn an_owner_preview_has_one_start_check_and_no_second_one() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let action = A::BrowserOpen {
        url: "http://localhost:3000/".to_owned(),
        monitor: None,
        session: None,
    };
    let admitted = h.admitted(&action, 1);
    rig.desktop.act(&on, action, admitted).await.unwrap();
    assert_eq!(rig.calls_of("chromium").len(), 1);
}
