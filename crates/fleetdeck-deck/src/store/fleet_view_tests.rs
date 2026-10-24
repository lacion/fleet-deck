//! The fleet view on the deck store (MCP P3 design §2, §6 tests 1, 2a to 5).
//! Test 2 (paging) is in `fleet_view_paging_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_proto::desktop::{DesktopInfo, DesktopKind};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::StationTraits;
use fleetdeck_proto::fleet_view::{FleetView, FleetViewPage, PAGE_MAX_BYTES};
use fleetdeck_proto::integrations::ProjectInput;
use fleetdeck_proto::link::{Scope, StationInfo, StationProvider};
use fleetdeck_proto::session::{Activity, RepoInfo, SessionError};
use fleetdeck_proto::{SessionId, SessionSummary, StationId};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

use super::DeckStore;
use super::fleet_view::{
    BUSY, FleetViewError, MAX_KEYS, NOT_KNOWN, SESSION_LIMIT, SESSION_LIMITED, STATION_LIMIT,
    STATION_LIMITED, Windows,
};
use super::fleet_view_build::platform_of;
use super::grants::{GrantClass, NewGrant};
use super::stations::NewStation;

pub(super) struct World {
    pub(super) det: Det,
    pub(super) s: DeckStore,
    marks: BTreeMap<StationId, u64>,
    keys: BTreeMap<StationId, PeerKey>,
}

pub(super) async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    World {
        det,
        s,
        marks: BTreeMap::new(),
        keys: BTreeMap::new(),
    }
}

impl World {
    pub(super) async fn pin(&mut self, name: &str, info: Option<StationInfo>) -> StationId {
        let key = Identity::generate().unwrap().public_key();
        let id = self
            .s
            .pin_station(NewStation {
                key,
                name: name.to_owned(),
                scopes: BTreeSet::from([Scope::Admin]),
                dial_url: None,
            })
            .await
            .unwrap()
            .station_id;
        self.keys.insert(id.clone(), key);
        self.s.set_reachable(&id, true);
        if let Some(info) = info {
            self.s.set_station_info(&id, info);
        }
        id
    }

    /// Replaces the sessions of `station` (a snapshot with a growing mark).
    pub(super) async fn sessions(&mut self, station: &StationId, sessions: Vec<SessionSummary>) {
        let mark = self.marks.entry(station.clone()).or_insert(0);
        *mark += 1;
        self.s
            .apply_snapshot(station.clone(), "inc".to_owned().into(), *mark, sessions)
            .await
            .unwrap();
    }

    pub(super) async fn revoke(&mut self, station: &StationId) {
        let key = self.keys.remove(station).unwrap();
        self.s.revoke_station(key).await.unwrap().unwrap();
    }

    pub(super) async fn project(&self, station: &StationId, name: &str, path: &str) -> String {
        self.s
            .create_project(
                station.as_str().to_owned(),
                ProjectInput {
                    name: name.to_owned(),
                    station: station.as_str().to_owned(),
                    path: path.to_owned(),
                    forge: None,
                    trackers: vec![],
                    prompt_template: None,
                    review_prompt: None,
                },
            )
            .await
            .unwrap()
            .project_id
    }
}

pub(super) fn summary(id: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.provider = Some(ProviderKind::Claude);
    s.callsign = Some(format!("cs-{id}"));
    s
}

pub(super) fn provider(instance: &str, version: &str) -> StationProvider {
    StationProvider {
        instance: instance.to_owned().into(),
        provider: ProviderKind::Claude,
        version: Some(version.to_owned()),
        auth: "signed_in".to_owned(),
        capabilities: None,
        version_status: Default::default(),
    }
}

pub(super) fn info(name: &str, providers: Vec<StationProvider>) -> StationInfo {
    StationInfo {
        name: name.to_owned(),
        incarnation: "inc".to_owned().into(),
        head: 0,
        live_sessions: 2,
        version: None,
        max_sessions: Some(8),
        allow_shell: None,
        allow_setup_cmd: None,
        providers,
        assist_tokens_today: None,
        assist_cap: None,
        resources: None,
        desktop: None,
        traits: None,
        config: None,
        taildrive: None,
        fleet_git: None,
        fleet_summary: None,
        cli_update: None,
    }
}

pub(super) fn params(
    caller: &str,
    view: &str,
    cursor: Option<&str>,
    station: Option<&StationId>,
) -> Value {
    let mut p = json!({"caller": caller, "view": view});
    if let Some(c) = cursor {
        p["cursor"] = json!(c);
    }
    if let Some(s) = station {
        p["station"] = json!(s.as_str());
    }
    p
}

/// One view; every answer stays within the byte budget.
pub(super) async fn ask(
    w: &World,
    station: &StationId,
    caller: &str,
    view: &str,
    cursor: Option<&str>,
    filter: Option<&StationId>,
) -> Result<FleetViewPage, FleetViewError> {
    let page =
        w.s.fleet_view(station, params(caller, view, cursor, filter))
            .await?;
    let bytes = serde_json::to_string(&page).unwrap().len();
    assert!(bytes <= PAGE_MAX_BYTES, "{view}: {bytes} bytes");
    Ok(page)
}

pub(super) fn refusal(r: Result<FleetViewPage, FleetViewError>) -> (&'static str, &'static str) {
    match r {
        Err(FleetViewError::Refused(code, text)) => (code, text),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

pub(super) fn keys_of(item: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = item
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

// ---------------------------------------------------------------------------
// 1
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_fleet_view_answers_stations_sessions_and_me_without_a_grant() {
    let mut w = world().await;
    let mut alpha_info = info("alpha", vec![provider("claude", "2.1.302")]);
    alpha_info.traits = Some(StationTraits {
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        on_battery: None,
        labels: vec![],
    });
    let alpha = w.pin("alpha", Some(alpha_info)).await;
    let beta = w.pin("beta", None).await;

    let mut coord = summary("coord-1");
    coord.title = Some("Fix the build".to_owned());
    coord.model = Some("opus".to_owned());
    coord.mode = Some("default".to_owned());
    coord.cwd = Some("/home/u/SENTINEL-cwd".to_owned());
    coord.branch = Some("SENTINEL-branch".to_owned());
    coord.worktree = Some("/home/u/SENTINEL-worktree".to_owned());
    coord.digest = Some("SENTINEL-digest".to_owned());
    coord.focus = Some("SENTINEL-focus".to_owned());
    coord.repo = Some(RepoInfo {
        root: "/home/u/SENTINEL-root".to_owned(),
        origin: Some("https://x/repo?access_token=SENTINEL".to_owned()),
    });
    coord.last_error = Some(SessionError {
        code: "SENTINEL-code".to_owned(),
        message: "SENTINEL-message".to_owned(),
        retry_at: None,
    });
    let mut gone = summary("gone-1");
    gone.activity = Activity::Ended;
    w.sessions(&alpha, vec![coord, gone, summary("worker-1")])
        .await;
    w.sessions(&beta, vec![summary("other-1")]).await;
    w.project(&alpha, "app", "/p/SENTINEL-path").await;

    // The station has no grant and no `coordinate` scope; it asks.
    let mut all = Vec::new();

    let stations = ask(&w, &alpha, "coord-1", "stations", None, None)
        .await
        .unwrap();
    assert_eq!(stations.next_cursor, None);
    let items = stations.items.as_array().unwrap();
    assert_eq!(items.len(), 2);
    let expected = [
        "arch",
        "desktop",
        "live_sessions",
        "max_sessions",
        "name",
        "online",
        "platform",
        "project_count",
        "providers",
        "providers_more",
        "station_id",
    ];
    for item in items {
        assert_eq!(keys_of(item), expected);
    }
    assert_eq!(items[0]["name"], "alpha");
    assert_eq!(items[0]["platform"], "linux");
    assert_eq!(items[0]["arch"], "x86_64");
    assert_eq!(items[0]["desktop"], Value::Null);
    assert_eq!(items[0]["online"], true);
    assert_eq!(items[0]["project_count"], 1);
    assert_eq!(items[0]["live_sessions"], 2);
    assert_eq!(items[0]["max_sessions"], 8);
    assert_eq!(items[0]["providers_more"], false);
    assert_eq!(
        items[0]["providers"],
        json!([{"instance": "claude", "provider": "claude", "version": "2.1.302",
                "signed_in": true}])
    );
    // A station without `station.info`: every optional key is `null`.
    assert_eq!(items[1]["name"], "beta");
    assert_eq!(items[1]["platform"], Value::Null);
    assert_eq!(items[1]["providers"], json!([]));
    assert_eq!(items[1]["max_sessions"], Value::Null);
    all.push(stations.items);

    let projects = ask(&w, &alpha, "coord-1", "projects", None, None)
        .await
        .unwrap();
    let items = projects.items.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        keys_of(&items[0]),
        ["name", "project_id", "station", "station_id"]
    );
    assert_eq!(items[0]["station"], "alpha");
    all.push(projects.items);

    let sessions = ask(&w, &alpha, "coord-1", "sessions", None, None)
        .await
        .unwrap();
    let items = sessions.items.as_array().unwrap();
    let ids: Vec<&str> = items
        .iter()
        .map(|i| i["session_id"].as_str().unwrap())
        .collect();
    // By (station name, callsign, session id); the ended session is absent.
    assert_eq!(ids, ["coord-1", "worker-1", "other-1"]);
    for item in items {
        assert_eq!(
            keys_of(item),
            [
                "callsign",
                "provider",
                "session_id",
                "station",
                "station_id",
                "status",
                "title"
            ]
        );
    }
    assert_eq!(items[0]["title"], "Fix the build");
    assert_eq!(items[0]["status"], "queued");
    assert_eq!(items[0]["provider"], "claude");
    assert_eq!(items[1]["title"], Value::Null);
    all.push(sessions.items);

    let me = ask(&w, &alpha, "coord-1", "me", None, None).await.unwrap();
    assert_eq!(me.next_cursor, None);
    assert_eq!(
        keys_of(&me.items),
        [
            "callsign",
            "grant",
            "mode",
            "model",
            "provider",
            "session_id",
            "station",
            "station_id",
            "title"
        ]
    );
    assert_eq!(me.items["grant"], Value::Null);
    assert_eq!(me.items["model"], "opus");
    assert_eq!(me.items["mode"], "default");
    assert_eq!(me.items["station"], "alpha");
    all.push(me.items);

    // The excluded fields never appear in any page.
    let text = serde_json::to_string(&all).unwrap();
    for banned in ["SENTINEL", "access_token", "x/repo", "/home/u", "/p/"] {
        assert!(!text.contains(banned), "{banned} in {text}");
    }
}

// ---------------------------------------------------------------------------
// 2a
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_projection_cache_rebuilds_at_most_once_per_view_every_two_seconds() {
    let mut w = world().await;
    let alpha = w.pin("alpha", Some(info("alpha", vec![]))).await;
    let sessions = (0..8).map(|i| summary(&format!("c{i}"))).collect();
    w.sessions(&alpha, sessions).await;
    w.project(&alpha, "app", "/p/app").await;

    // Eight callers at once (the busy limit) ask for each view.
    for view in ["stations", "projects", "sessions"] {
        let mut tasks = Vec::new();
        for i in 0..8 {
            let s = w.s.clone();
            let alpha = alpha.clone();
            let view = view.to_owned();
            tasks.push(tokio::spawn(async move {
                s.fleet_view(&alpha, params(&format!("c{i}"), &view, None, None))
                    .await
                    .unwrap()
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
    }
    for view in [
        FleetView::Stations,
        FleetView::Projects,
        FleetView::Sessions,
    ] {
        assert_eq!(w.s.fleet_view_rebuilds(view), 1, "{view:?}");
    }
    // Younger than two seconds: no rebuild, whatever changed.
    w.sessions(&alpha, vec![summary("c0")]).await;
    let page = ask(&w, &alpha, "c0", "sessions", None, None).await.unwrap();
    assert_eq!(page.items.as_array().unwrap().len(), 8);
    assert_eq!(w.s.fleet_view_rebuilds(FleetView::Sessions), 1);
    // `me` reads the caller's own rows and builds no projection.
    ask(&w, &alpha, "c0", "me", None, None).await.unwrap();
    // Older than two seconds: one rebuild.
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    let page = ask(&w, &alpha, "c0", "sessions", None, None).await.unwrap();
    assert_eq!(page.items.as_array().unwrap().len(), 1);
    assert_eq!(w.s.fleet_view_rebuilds(FleetView::Sessions), 2);
    assert_eq!(w.s.fleet_view_rebuilds(FleetView::Stations), 1);
}

// ---------------------------------------------------------------------------
// 3
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_fleet_view_limits_hold() {
    let mut w = world().await;
    let alpha = w.pin("alpha", None).await;
    let beta = w.pin("beta", None).await;
    w.sessions(&alpha, vec![summary("a1"), summary("a2")]).await;
    w.sessions(&beta, vec![summary("b1")]).await;

    // The busy semaphore answers at once, with no queue.
    {
        let _all = w.s.fleet_view_hold_all_permits();
        let r = tokio::time::timeout(
            Duration::from_millis(500),
            ask(&w, &alpha, "a1", "stations", None, None),
        )
        .await
        .expect("an immediate answer");
        assert_eq!(refusal(r), BUSY);
        assert_eq!(
            BUSY,
            ("busy", "the fleet view is busy, try again in a moment")
        );
    }
    ask(&w, &alpha, "a1", "stations", None, None).await.unwrap();

    // An unknown caller costs a station slot and no session slot.
    let (stations_before, sessions_before) = w.s.fleet_view_limiter_keys();
    assert_eq!((stations_before, sessions_before), (1, 1));
    for i in 0..10 {
        let r = ask(&w, &beta, &format!("ghost-{i}"), "stations", None, None).await;
        assert_eq!(refusal(r), NOT_KNOWN);
    }
    assert_eq!(w.s.fleet_view_limiter_keys(), (2, 1));
    assert_eq!(
        NOT_KNOWN,
        ("forbidden", "the session is not known on this station")
    );

    // The per-session limit: 60 views per minute, the 61st is refused.
    for _ in 0..SESSION_LIMIT - 1 {
        ask(&w, &alpha, "a1", "me", None, None).await.unwrap();
    }
    let r = ask(&w, &alpha, "a1", "me", None, None).await;
    assert_eq!(refusal(r), SESSION_LIMITED);
    assert_eq!(
        SESSION_LIMITED,
        ("rate_limited", "at most 60 fleet views per minute")
    );
    // Another session of the station still asks.
    ask(&w, &alpha, "a2", "me", None, None).await.unwrap();
    // The window moves with the clock.
    w.det.advance(Duration::from_secs(61));
    ask(&w, &alpha, "a1", "me", None, None).await.unwrap();

    // The station limit counts refused requests and rotating callers: the
    // views above plus these reach 240 in the minute.
    let used = {
        let mut n = 0;
        loop {
            let r = ask(&w, &beta, &format!("rot-{n}"), "me", None, None).await;
            match r {
                Err(FleetViewError::Refused(code, text)) if (code, text) == STATION_LIMITED => {
                    break n;
                }
                Err(FleetViewError::Refused(code, text)) => {
                    assert_eq!((code, text), NOT_KNOWN);
                }
                other => panic!("{other:?}"),
            }
            n += 1;
            assert!(n <= STATION_LIMIT, "the station limit never holds");
        }
    };
    // The ghosts of the first window are gone: 240 views fill the window.
    assert_eq!(used, STATION_LIMIT);
    assert_eq!(
        STATION_LIMITED,
        (
            "rate_limited",
            "this station asked for too many fleet views"
        )
    );
    // The limit is per station: alpha was not touched by beta's flood.
    ask(&w, &alpha, "a2", "me", None, None).await.unwrap();
    // And beta's real session is refused by the station limit first.
    let r = ask(&w, &beta, "b1", "me", None, None).await;
    assert_eq!(refusal(r), STATION_LIMITED);
    // A new window frees it.
    w.det.advance(Duration::from_secs(61));
    ask(&w, &beta, "b1", "me", None, None).await.unwrap();
}

#[test]
fn the_limiter_maps_stay_at_most_4096_keys() {
    let windows: Windows<u32> = Windows::default();
    for key in 0..u32::try_from(MAX_KEYS).unwrap() + 500 {
        assert!(windows.admit(&key, i64::from(key), 1));
        assert!(windows.len() <= MAX_KEYS);
    }
    assert_eq!(windows.len(), MAX_KEYS);
    // The key with the oldest last view went first: key 0 is free again.
    assert!(windows.admit(&0, 10_000, 1));
    // Entries older than the window are removed on each admission.
    assert!(windows.admit(&7, 200_000, 1));
    assert_eq!(windows.len(), 1);
}

// ---------------------------------------------------------------------------
// 4
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_caller_is_checked_on_the_calling_station_only() {
    let mut w = world().await;
    let alpha = w.pin("alpha", None).await;
    let beta = w.pin("beta", None).await;
    w.sessions(&alpha, vec![summary("s1")]).await;
    w.sessions(&beta, vec![summary("s1"), summary("b-only")])
        .await;

    // A caller that exists only on another station is unknown here.
    let r = ask(&w, &alpha, "b-only", "me", None, None).await;
    assert_eq!(refusal(r), NOT_KNOWN);
    let r = ask(&w, &alpha, "b-only", "sessions", None, None).await;
    assert_eq!(refusal(r), NOT_KNOWN);

    // The same session id on two stations: each has its own grant.
    let mut a = NewGrant::new(alpha.clone(), SessionId::from("s1"));
    a.classes = [GrantClass::Read, GrantClass::Mail].into_iter().collect();
    a.stations = [alpha.clone(), beta.clone()].into_iter().collect();
    let a = w.s.create_grant(a).await.unwrap();
    let mut b = NewGrant::new(beta.clone(), SessionId::from("s1"));
    b.classes = [GrantClass::Read].into_iter().collect();
    let b = w.s.create_grant(b).await.unwrap();
    let before = (
        w.s.grant(a.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .last_used_at,
        w.s.grant(b.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .last_used_at,
    );

    let me = ask(&w, &alpha, "s1", "me", None, None).await.unwrap().items;
    let grant = &me["grant"];
    assert_eq!(grant["grant_id"], a.grant_id.as_str());
    assert_eq!(grant["classes"], json!(["read", "mail"]));
    assert_eq!(grant["stations"], json!(["alpha", "beta"]));
    assert_eq!(grant["scope"], "lineage");
    assert_eq!(grant["spawn_used"], 0);
    assert_eq!(
        keys_of(grant),
        [
            "classes",
            "expires_at",
            "grant_id",
            "max_modes",
            "scope",
            "spawn_cap",
            "spawn_used",
            "stations"
        ]
    );
    let me = ask(&w, &beta, "s1", "me", None, None).await.unwrap().items;
    assert_eq!(me["grant"]["grant_id"], b.grant_id.as_str());
    assert_eq!(me["grant"]["stations"], json!(["beta"]));
    assert_eq!(me["station"], "beta");
    // `b-only` has no grant at all.
    let me = ask(&w, &beta, "b-only", "me", None, None)
        .await
        .unwrap()
        .items;
    assert_eq!(me["grant"], Value::Null);

    // Reading changes no `last_used_at`.
    let after = (
        w.s.grant(a.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .last_used_at,
        w.s.grant(b.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .last_used_at,
    );
    assert_eq!(before, after);

    // A revoked grant and an expired grant are `null`.
    assert!(
        w.s.revoke_grant(b.grant_id.clone(), "test".to_owned())
            .await
            .unwrap()
    );
    let me = ask(&w, &beta, "s1", "me", None, None).await.unwrap().items;
    assert_eq!(me["grant"], Value::Null);
    w.det.advance(Duration::from_millis(
        u64::try_from(a.expires_at).unwrap() - w.det.now_ms() + 1,
    ));
    let me = ask(&w, &alpha, "s1", "me", None, None).await.unwrap().items;
    assert_eq!(me["grant"], Value::Null);
}

// ---------------------------------------------------------------------------
// 5
// ---------------------------------------------------------------------------

fn traits(os: &str) -> StationTraits {
    StationTraits {
        os: os.to_owned(),
        arch: "aarch64".to_owned(),
        on_battery: None,
        labels: vec![],
    }
}

fn desktop(kind: DesktopKind) -> DesktopInfo {
    let kind = match kind {
        DesktopKind::Omarchy => "omarchy",
        DesktopKind::Hyprland => "hyprland",
    };
    serde_json::from_value(json!({
        "kind": kind, "omarchy_version": null, "monitors": [], "locked": null,
        "dnd": null, "idle_inhibited": null, "walls": [],
        "allowed": {"desktop": false, "capture": false, "input": false},
        "tools": {"wtype": false, "pointer": false, "recorder": false, "grim": false,
                  "chromium": false, "notify_send": false}
    }))
    .unwrap()
}

#[test]
fn the_platform_rule_maps_omarchy_macos_and_linux() {
    let mut i = info("x", vec![]);
    // Without traits and without a desktop: null.
    assert_eq!(platform_of(&i), None);
    i.traits = Some(traits("linux"));
    assert_eq!(platform_of(&i).as_deref(), Some("linux"));
    i.traits = Some(traits("macos"));
    assert_eq!(platform_of(&i).as_deref(), Some("macos"));
    i.traits = Some(traits("windows"));
    assert_eq!(platform_of(&i).as_deref(), Some("windows"));
    // Hyprland alone does not make Omarchy.
    i.desktop = Some(desktop(DesktopKind::Hyprland));
    i.traits = Some(traits("linux"));
    assert_eq!(platform_of(&i).as_deref(), Some("linux"));
    // Omarchy wins over the OS, also without traits.
    i.desktop = Some(desktop(DesktopKind::Omarchy));
    assert_eq!(platform_of(&i).as_deref(), Some("omarchy"));
    i.traits = None;
    assert_eq!(platform_of(&i).as_deref(), Some("omarchy"));
    // An OS name above 32 characters is cut.
    i.desktop = None;
    i.traits = Some(traits(&"o".repeat(40)));
    let platform = platform_of(&i).unwrap();
    assert_eq!(platform.chars().count(), 32);
    assert!(platform.ends_with('…'));
}

/// MCP P3 PRE-PR finding: `me` keeps the 8 KiB item limit. Sixteen grant
/// stations whose names are 128 four-byte characters make the item too
/// large, so the answer is left out and the caller gets `forbidden`.
#[tokio::test]
async fn an_oversized_me_item_is_left_out() {
    let mut w = world().await;
    let mut stations = BTreeSet::new();
    let mut first = None;
    for n in 0..16 {
        let name = format!("{n:02}{}", "🛰".repeat(126));
        let id = w.pin(&name, None).await;
        first.get_or_insert_with(|| id.clone());
        stations.insert(id);
    }
    let home = first.unwrap();
    w.sessions(&home, vec![summary("s1")]).await;
    let mut g = NewGrant::new(home.clone(), SessionId::from("s1"));
    g.classes = [GrantClass::Read].into_iter().collect();
    g.stations = stations;
    w.s.create_grant(g).await.unwrap();
    let r = ask(&w, &home, "s1", "me", None, None).await;
    assert_eq!(refusal(r), NOT_KNOWN);
}
