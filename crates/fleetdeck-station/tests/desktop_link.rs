//! `desktop.info` and `desktop.act` on the link (R13 P3a, feature
//! `desktop.v1`): the method rows, and a station without a desktop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Identity;
use fleetdeck_link::desktop_authority::issue;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::methods::{authorize, code, method};
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::desktop::{DesktopAction, DesktopInfo};
use fleetdeck_proto::desktop_authority::with_authority;
use fleetdeck_proto::link::{Outcome, Scope, StationInfo};
use fleetdeck_proto::version::{Negotiated, PROTO_MAX, feature};
use fleetdeck_station::desktop::{self, Desktop};
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::ShellConfig;
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(10);

fn negotiated(features: &[&str]) -> Negotiated {
    Negotiated {
        version: PROTO_MAX,
        features: features.iter().map(|f| (*f).to_owned()).collect(),
    }
}

#[test]
fn the_method_rows_are_deck_to_station_rows_of_the_desktop_feature() {
    let info = method("desktop.info").unwrap();
    assert_eq!(info.scope, Scope::Read);
    assert_eq!(info.feature, Some(feature::DESKTOP));
    assert!(!info.mutating);
    let act = method("desktop.act").unwrap();
    assert_eq!(act.scope, Scope::Drive);
    // The deck authorizes each action (R13 §5E item 1).
    assert_eq!(act.feature, Some(feature::DESKTOP_AUTHORITY));
    assert!(!act.mutating);
    assert_eq!(feature::DESKTOP, "desktop.v1");
    assert_eq!(feature::DESKTOP_AUTHORITY, "desktop_authority.v1");
    assert!(feature::ALL.contains(&feature::DESKTOP));
    assert!(feature::ALL.contains(&feature::DESKTOP_AUTHORITY));
}

#[test]
fn the_methods_need_the_feature_and_the_scope() {
    let without = negotiated(&[feature::CONTROL]);
    let with = negotiated(&[
        feature::CONTROL,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
    ]);
    let read: BTreeSet<Scope> = [Scope::Read].into_iter().collect();
    let drive: BTreeSet<Scope> = [Scope::Read, Scope::Drive].into_iter().collect();
    let call = |name: &str, n: &Negotiated, scopes: &BTreeSet<Scope>| {
        authorize(name, Role::Deck, Role::Station, None, n, Some(scopes))
            .map(|_| ())
            .map_err(|e| e.code)
    };
    for name in ["desktop.info", "desktop.act"] {
        assert_eq!(
            call(name, &without, &drive),
            Err(code::FEATURE_NOT_NEGOTIATED.to_owned())
        );
    }
    assert_eq!(call("desktop.info", &with, &read), Ok(()));
    assert_eq!(call("desktop.act", &with, &drive), Ok(()));
    assert_eq!(
        call("desktop.act", &with, &read),
        Err(code::FORBIDDEN.to_owned())
    );
    // A station never sends them.
    let wrong = authorize(
        "desktop.act",
        Role::Station,
        Role::Deck,
        None,
        &with,
        Some(&drive),
    );
    assert_eq!(wrong.unwrap_err().code, code::WRONG_ROLE);
}

struct Fixture {
    _det: Det,
    ctx: Arc<StationLink>,
    listener: LinkListener,
    deck_node: NodeConfig,
}

/// A station on a loopback link, and a process that has no desktop: the
/// adapter gets an empty environment.
async fn fixture() -> Fixture {
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    fixture_with(policy).await
}

/// The same station with a given policy.
async fn fixture_with(policy: Policy) -> Fixture {
    desktop::install(Arc::new(Desktop::with_env(ChildEnv::empty())));
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let manager = SessionManager::start(
        store.clone(),
        policy,
        Adapters::new(),
        det.ids(),
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    let deck = Arc::new(Identity::generate().unwrap());
    store
        .pin_peer(PairedKey {
            key: deck.public_key(),
            role: Role::Deck,
            scopes: BTreeSet::from([Scope::Read, Scope::Drive]),
            dial_url: None,
        })
        .await
        .unwrap();
    let peers = StationPeers::load(store.clone()).await.unwrap();
    let station_id = Arc::new(Identity::generate().unwrap());
    let node = NodeConfig::new(
        Arc::clone(&station_id),
        LocalHello::new(Role::Station, store.incarnation().clone()),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&peers));
    peers.pin_all(&node.pins).await.unwrap();
    let listener = node::listen(node.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let deck_node = NodeConfig::new(
        Arc::clone(&deck),
        LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Grants::new());
    deck_node.pins.pin(station_id.public_key(), Role::Station);
    let ctx = Arc::new(StationLink {
        manager: Arc::new(manager),
        node,
        peers,
        name: "box".to_owned(),
        shells: Arc::new(fleetdeck_station::shell::Shells::new(ShellConfig::default())),
        coordinator: fleetdeck_station::coordinator::LinkForward::default(),
        paste_dir: None,
        summary: std::sync::Arc::default(),
        fleet_git: Arc::new(fleetdeck_station::fleet_git_runtime::FleetGitRuntime::new(
            Arc::new(fleetdeck_station::workspace::GitOptions::isolated),
        )),
    });
    Fixture {
        _det: det,
        ctx,
        listener,
        deck_node,
    }
}

impl Fixture {
    async fn connect(&mut self) -> LiveLink {
        let peer = self.ctx.node.identity.public_key();
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&self.deck_node, self.listener.local_addr, peer),
                self.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_station = fleetdeck_link::start(&self.ctx.node, a.expect("listener"));
        tokio::spawn(link::serve(Arc::clone(&self.ctx), at_station));
        fleetdeck_link::start(&self.deck_node, d.unwrap())
    }
}

async fn call(deck: &LiveLink, method: &str, params: Value) -> Outcome {
    tokio::time::timeout(T, deck.control.request(method, None, params))
        .await
        .expect("response")
        .unwrap()
}

fn refused(o: Outcome) -> (String, String) {
    match o {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    }
}

// The fixtures replace the process-global adapter: one test at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn a_station_without_a_desktop_refuses_the_methods_and_has_no_desktop_field() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    let (code, message) = refused(call(&deck, "desktop.info", json!({})).await);
    assert_eq!(code, "unsupported");
    assert_eq!(message, "This station has no desktop.");
    // A `desktop.act` without a valid authority is refused first, also on a
    // station without a desktop (R13 §5E item 1, DL-230).
    let (code, message) =
        refused(call(&deck, "desktop.act", json!({"action": "screen_lock"})).await);
    assert_eq!(code, "forbidden");
    assert_eq!(message, "The deck did not authorize this desktop action.");
    // `station.info` carries no `desktop` field.
    let Outcome::Ok(raw) = call(&deck, "station.info", json!({})).await else {
        panic!("station.info failed");
    };
    assert!(raw.get("desktop").is_none(), "{raw}");
    let info: StationInfo = serde_json::from_value(raw).unwrap();
    assert!(info.desktop.is_none());
}

#[tokio::test]
async fn a_bad_action_is_invalid_params() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    for params in [
        json!({"action": "exec", "cmd": "id"}),
        json!({"action": "screen_dnd", "on": true, "extra": 1}),
        json!({"action": "screen_dnd"}),
        json!({}),
    ] {
        let (code, _) = refused(call(&deck, "desktop.act", params.clone()).await);
        assert_eq!(code, "invalid_params", "{params}");
    }
}

/// What the deck sends for `action` on `deck`: the authority of this link.
fn authorized(f: &Fixture, deck: &LiveLink, action: &DesktopAction, n: u64) -> Value {
    let node = f.ctx.node.identity.fingerprint().to_string();
    let epoch = deck
        .control
        .params()
        .peer_desktop_epoch
        .clone()
        .expect("the station sent a desktop epoch");
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let authority = issue(action, &node, format!("{n:026}"), epoch, now, None);
    with_authority(action, &authority)
}

#[tokio::test]
async fn an_action_needs_the_authority_of_this_connection() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    let action = DesktopAction::ScreenLock;
    // A valid authority is admitted; this station has no desktop, so the
    // action itself stops there.
    let (code, message) =
        refused(call(&deck, "desktop.act", authorized(&f, &deck, &action, 1)).await);
    assert_eq!(code, "unsupported");
    assert_eq!(message, "This station has no desktop.");
    // The same id is refused: one use for the connection epoch.
    let (code, message) =
        refused(call(&deck, "desktop.act", authorized(&f, &deck, &action, 1)).await);
    assert_eq!(code, "forbidden");
    assert_eq!(message, "The deck did not authorize this desktop action.");
    // An authority of another epoch, of another station, or for another
    // action is refused.
    let mut wrong_epoch = authorized(&f, &deck, &action, 2);
    wrong_epoch["authority"]["desktop_epoch"] = json!("ee".repeat(16));
    let mut wrong_station = authorized(&f, &deck, &action, 3);
    wrong_station["authority"]["action_hash"] = json!(
        fleetdeck_link::desktop_authority::action_hash(&action, "other-station")
    );
    let mut wrong_action = authorized(&f, &deck, &DesktopAction::ScreenDnd { on: true }, 4);
    wrong_action["action"] = json!("screen_lock");
    wrong_action.as_object_mut().unwrap().remove("on");
    for params in [wrong_epoch, wrong_station, wrong_action] {
        let (code, message) = refused(call(&deck, "desktop.act", params.clone()).await);
        assert_eq!(code, "forbidden", "{params}");
        assert_eq!(message, "The deck did not authorize this desktop action.");
    }
    // A clock far from the station clock is refused with its own text.
    let mut skewed = authorized(&f, &deck, &action, 5);
    skewed["authority"]["deck_now_ms"] = json!(1);
    let (code, message) = refused(call(&deck, "desktop.act", skewed).await);
    assert_eq!(code, "forbidden");
    assert_eq!(
        message,
        "The station and the deck clocks differ too much for a desktop action."
    );
    // A malformed authority is a parse error, not an authorization.
    let (code, _) = refused(
        call(
            &deck,
            "desktop.act",
            json!({"action": "screen_lock", "authority": {"action_id": 1}}),
        )
        .await,
    );
    assert_eq!(code, "invalid_params");
    // A new connection has a new epoch: the old authority does not work.
    let old = authorized(&f, &deck, &action, 6);
    let again = f.connect().await;
    assert_ne!(
        again.control.params().peer_desktop_epoch,
        deck.control.params().peer_desktop_epoch
    );
    let (code, _) = refused(call(&again, "desktop.act", old).await);
    assert_eq!(code, "forbidden");
}

#[tokio::test]
async fn desktop_info_takes_fresh() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    // Without a desktop, a fresh read is `unsupported` like the cached one.
    let (code, message) = refused(call(&deck, "desktop.info", json!({"fresh": true})).await);
    assert_eq!(code, "unsupported");
    assert_eq!(message, "This station has no desktop.");
    let (code, _) = refused(call(&deck, "desktop.info", json!({"other": 1})).await);
    assert_eq!(code, "invalid_params");
}

#[test]
fn the_desktop_shell_rows_are_in_the_feature_and_need_the_feature_to_be_called() {
    assert_eq!(feature::DESKTOP_SHELL, "desktop_shell.v1");
    assert!(feature::ALL.contains(&feature::DESKTOP_SHELL));
    assert!(!feature::REQUIRED.contains(&feature::DESKTOP_SHELL));
    let spec = method("fleet.summary").unwrap();
    assert_eq!(spec.scope, Scope::Read);
    assert_eq!(spec.feature, Some(feature::DESKTOP_SHELL));
    let without = negotiated(&[feature::CONTROL, feature::DESKTOP]);
    let read: BTreeSet<Scope> = [Scope::Read].into_iter().collect();
    let refused = authorize(
        "fleet.summary",
        Role::Deck,
        Role::Station,
        None,
        &without,
        Some(&read),
    );
    assert_eq!(refused.unwrap_err().code, code::FEATURE_NOT_NEGOTIATED);
}

#[tokio::test]
async fn fleet_summary_is_refused_with_forbidden_when_the_setting_is_off() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    let (code, message) = refused(call(&deck, "fleet.summary", json!({})).await);
    assert_eq!(code, "forbidden");
    assert_eq!(message, "The fleet summary is off on this station.");
}

#[tokio::test]
async fn fleet_summary_with_the_setting_on_refuses_an_invalid_body_and_stores_a_valid_one() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let on = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new())
        .with_fleet_summary(true);
    let mut f = fixture_with(on).await;
    let deck = f.connect().await;
    let (code, message) = refused(call(&deck, "fleet.summary", json!({})).await);
    assert_eq!(code, "invalid_params");
    assert_eq!(message, "The fleet summary is not valid.");
    assert_eq!(f.ctx.summary.read().summary, None, "nothing is stored");
    let valid = json!({
        "v": 1, "at": "2026-10-18T12:04:00Z", "board": null, "deck": "up",
        "counts": {"working": 1, "idle": 0, "needs_you": 0, "blocked": 0, "lost": 0},
        "needs_you": [], "usage": null, "stations": []
    });
    assert_eq!(
        call(&deck, "fleet.summary", valid).await,
        Outcome::Ok(json!({"ok": true}))
    );
    let read = f.ctx.summary.read();
    assert!(read.seq >= 1);
    assert_eq!(read.summary.unwrap().at, "2026-10-18T12:04:00Z");
}

#[tokio::test]
async fn station_info_carries_the_live_fleet_summary_setting() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    let Outcome::Ok(raw) = call(&deck, "station.info", json!({})).await else {
        panic!("station.info failed");
    };
    assert_eq!(raw["fleet_summary"], json!(false), "{raw}");
    let info: StationInfo = serde_json::from_value(raw).unwrap();
    assert_eq!(info.fleet_summary, Some(false));

    let on = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new())
        .with_fleet_summary(true);
    let mut f = fixture_with(on).await;
    let deck = f.connect().await;
    let Outcome::Ok(raw) = call(&deck, "station.info", json!({})).await else {
        panic!("station.info failed");
    };
    let info: StationInfo = serde_json::from_value(raw).unwrap();
    assert_eq!(info.fleet_summary, Some(true));
}

const MONITORS: &str = r#"[
 {"id":0,"name":"DP-1","description":"Ultrawide","width":3440,"height":1440,"refreshRate":143.99800,"scale":1.25,"focused":true,"dpmsStatus":true,"activeWorkspace":{"id":1,"name":"1"}}
]"#;

/// A small fake Hyprland desktop: a temporary `PATH` with a `hyprctl` that
/// answers the reads and logs every other call, and a `grim` that logs its
/// call; both log into `calls.log`. `install` makes it the process desktop.
struct FakeDesktop {
    dir: tempfile::TempDir,
}

impl FakeDesktop {
    async fn install() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let runtime = root.join("run");
        let bin = root.join("bin");
        for d in [&runtime, &bin, &root.join("home")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(root.join("monitors.json"), MONITORS).unwrap();
        let r = root.display();
        let script = |name: &str, body: String| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(
            "hyprctl",
            format!(
                "case \"$*\" in\n\
                 \"instances -j\") echo '[{{\"instance\":\"sig_t_1\",\"time\":1,\"pid\":1,\"wl_socket\":\"wayland-1\"}}]';;\n\
                 \"-j monitors\") /bin/cat {r}/monitors.json;;\n\
                 \"-j clients\") echo '[]';;\n\
                 *) echo \"hyprctl $*\" >> {r}/calls.log; echo ok;;\n\
                 esac"
            ),
        );
        script("grim", format!("echo \"grim $*\" >> {r}/calls.log\nexit 0"));
        let instance = runtime.join("hypr").join("sig_t_1");
        std::fs::create_dir_all(&instance).unwrap();
        let live = tokio::net::UnixListener::bind(instance.join(".socket.sock")).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = live.accept().await {
                drop(stream);
            }
        });
        let events = tokio::net::UnixListener::bind(instance.join(".socket2.sock")).unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = events.accept().await {
                held.push(stream);
            }
        });
        let env = ChildEnv::scrubbed(
            [
                ("PATH", bin.as_os_str().to_owned()),
                ("XDG_RUNTIME_DIR", runtime.as_os_str().to_owned()),
                ("HOME", root.join("home").into_os_string()),
                ("OMARCHY_PATH", root.join("no-omarchy").into_os_string()),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v)),
        );
        desktop::install(Arc::new(Desktop::with_env(env)));
        Self { dir }
    }

    /// The logged calls that are not reads.
    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
    }
}

fn capture_policy(desktop: bool, capture: bool, monitors: &[&str]) -> Policy {
    Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new())
        .with_allow_desktop(desktop)
        .with_allow_desktop_capture(capture)
        .with_capture_monitors(monitors.iter().map(|m| (*m).to_owned()).collect())
}

/// The `desktop` field of `station.info` of a fixture with `policy`.
async fn desktop_field(policy: Policy) -> DesktopInfo {
    let mut f = fixture_with(policy).await;
    let _desktop = FakeDesktop::install().await;
    let deck = f.connect().await;
    let Outcome::Ok(raw) = call(&deck, "station.info", json!({})).await else {
        panic!("station.info failed");
    };
    let info: StationInfo = serde_json::from_value(raw).unwrap();
    info.desktop.expect("the fake desktop is found")
}

#[tokio::test]
async fn desktop_allowed_capture_follows_both_live_gates() {
    let _turn = ONE_AT_A_TIME.lock().await;
    for (desktop, capture, want) in [
        (true, false, false),
        (false, true, false),
        (false, false, false),
        (true, true, true),
    ] {
        let info = desktop_field(capture_policy(desktop, capture, &[])).await;
        assert_eq!(info.allowed.desktop, desktop, "{desktop} {capture}");
        assert_eq!(info.allowed.capture, want, "{desktop} {capture}");
        assert!(!info.allowed.input);
        // The station sends no features: the deck fills them.
        assert!(info.features.is_empty());
    }
}

#[tokio::test]
async fn desktop_allowed_capture_monitors_equal_the_policy_list_on_every_path() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let info = desktop_field(capture_policy(true, true, &[])).await;
    assert!(info.allowed.capture_monitors.is_empty());
    let policy = capture_policy(true, true, &["DP-1", "HDMI-A-1"]);
    let info = desktop_field(policy.clone()).await;
    assert_eq!(info.allowed.capture_monitors, ["DP-1", "HDMI-A-1"]);

    // `desktop.info`, cached and fresh, carries the same live values.
    let mut f = fixture_with(policy).await;
    let _desktop = FakeDesktop::install().await;
    let deck = f.connect().await;
    for params in [json!({}), json!({"fresh": true}), json!({})] {
        let Outcome::Ok(raw) = call(&deck, "desktop.info", params.clone()).await else {
            panic!("desktop.info failed: {params}");
        };
        let info: DesktopInfo = serde_json::from_value(raw).unwrap();
        assert!(info.allowed.capture, "{params}");
        assert_eq!(info.allowed.capture_monitors, ["DP-1", "HDMI-A-1"]);
        assert!(info.features.is_empty());
    }
}

#[tokio::test]
async fn a_desktop_screen_capture_act_is_a_stub_that_runs_no_program() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture_with(capture_policy(true, true, &["DP-1"])).await;
    let fake = FakeDesktop::install().await;
    let deck = f.connect().await;
    let action = DesktopAction::ScreenCapture {
        monitor: "DP-1".to_owned(),
    };
    let (code, message) =
        refused(call(&deck, "desktop.act", authorized(&f, &deck, &action, 1)).await);
    assert_eq!(code, "unsupported");
    assert_eq!(message, "This station cannot take screenshots yet.");
    assert_eq!(fake.calls(), "", "no program ran");
}
