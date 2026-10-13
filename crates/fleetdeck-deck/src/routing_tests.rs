#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use fleetdeck_proto::link::{StationInfo, StationProvider};
use fleetdeck_proto::{SessionSummary, StationId};

fn card(id: &str, status: CardStatus, cwd: &str, updated_at: i64) -> DeckSession {
    let mut summary = SessionSummary::new(id.into());
    summary.cwd = Some(cwd.to_owned());
    summary.provider = Some(ProviderKind::Claude);
    summary.updated_at = updated_at;
    DeckSession {
        station_id: StationId::from("st".to_owned()),
        summary,
        status,
    }
}

fn station(name: &str, reachable: bool, max: Option<u32>, live: u64, auth: &str) -> StationView {
    StationView {
        station_id: StationId::from(name.to_owned()),
        name: name.to_owned(),
        key: "k".to_owned(),
        direction: "deck_dials".to_owned(),
        dial_url: None,
        reachable,
        last_seen: None,
        cursor: 0,
        info: Some(StationInfo {
            name: name.to_owned(),
            incarnation: "inc".to_owned().into(),
            head: 0,
            live_sessions: live,
            version: None,
            max_sessions: max,
            allow_shell: None,
            allow_setup_cmd: None,
            providers: vec![StationProvider {
                instance: "claude".to_owned().into(),
                provider: ProviderKind::Claude,
                version: Some("2.1.0".to_owned()),
                auth: auth.to_owned(),
                capabilities: None,
                version_status: Default::default(),
            }],
        }),
        info_at: None,
    }
}

#[test]
fn assign_auto_prefers_idle_then_queued_then_busy_then_least_buried() {
    let cards = vec![
        card("busy", CardStatus::Working, "/p/app", 9),
        card("queued", CardStatus::Queued, "/p/app", 8),
        card("idle-old", CardStatus::Idle, "/p/app", 1),
        card("idle-new", CardStatus::Idle, "/p/app", 5),
        card("waiting", CardStatus::NeedsYou, "/p/app", 10),
        card("gone", CardStatus::Unreachable, "/p/app", 10),
    ];
    let none = BTreeMap::new();
    let needs = Needs::default();
    let pick = |cards: &[DeckSession], buried: &BTreeMap<SessionId, u32>| {
        pick_session(cards, &needs, buried)
            .map(|s| s.summary.session_id.0.clone())
            .unwrap_or_default()
    };
    // Idle first; the most recent of two idle sessions.
    assert_eq!(pick(&cards, &none), "idle-new");
    // The least buried idle session wins over the most recent one.
    let buried = BTreeMap::from([(SessionId::from("idle-new".to_owned()), 2)]);
    assert_eq!(pick(&cards, &buried), "idle-old");
    // Without idle sessions: queued, then busy.
    assert_eq!(pick(&cards[..2], &none), "queued");
    assert_eq!(pick(&cards[..1], &none), "busy");
    // A session that needs you or whose station is unreachable takes no task.
    assert_eq!(pick(&cards[4..], &none), "");
}

#[test]
fn assign_auto_needs_the_right_repo_and_provider() {
    let mut codex = card("codex", CardStatus::Idle, "/p/app", 9);
    codex.summary.provider = Some(ProviderKind::Codex);
    let cards = vec![
        card("other", CardStatus::Idle, "/p/other", 9),
        card("app-like", CardStatus::Idle, "/p/application", 9),
        card("tree", CardStatus::Working, "/p/app--fd-falcon-abcd", 1),
        codex,
    ];
    let none = BTreeMap::new();
    let needs = Needs {
        repo: Some("/p/app/"),
        provider: Some(ProviderKind::Claude),
    };
    let s = pick_session(&cards, &needs, &none).unwrap();
    assert_eq!(
        s.summary.session_id.0, "tree",
        "a worktree of the repo fits"
    );
    let needs = Needs {
        repo: Some("/p/app"),
        provider: Some(ProviderKind::Codex),
    };
    let s = pick_session(&cards, &needs, &none).unwrap();
    assert_eq!(s.summary.session_id.0, "codex");
    let needs = Needs {
        repo: Some("/p/nowhere"),
        provider: None,
    };
    assert!(pick_session(&cards, &needs, &none).is_none());
    assert!(in_repo("/p/app/src", "/p/app"));
    assert!(!in_repo("/p/application", "/p/app"));
    assert!(!in_repo("/p/app", ""));
}

#[test]
fn spawn_on_needs_a_connected_station_under_its_cap_with_a_signed_in_provider() {
    let stations = vec![
        station("full", true, Some(2), 2, "signed_in"),
        station("down", false, Some(8), 0, "signed_in"),
        station("signed-out", true, Some(8), 0, "signed_out"),
        station("small", true, Some(3), 1, "signed_in"),
        station("big", true, Some(8), 1, "signed_in"),
    ];
    let names: Vec<_> = spawn_candidates(&stations, &Needs::default())
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["big", "small"]);
    let codex = Needs {
        repo: None,
        provider: Some(ProviderKind::Codex),
    };
    assert!(spawn_candidates(&stations, &codex).is_empty());
    let mut unknown = station("unknown", true, None, 0, "signed_in");
    unknown.info = None;
    assert!(spawn_candidates(&[unknown], &Needs::default()).is_empty());
}
