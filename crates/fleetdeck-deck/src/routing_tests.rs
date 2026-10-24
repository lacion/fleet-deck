#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use fleetdeck_proto::event::RateLimit;
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
        unread: None,
        project_id: None,
        review_rev: None,
    }
}

fn station(name: &str, reachable: bool, max: Option<u32>, live: u64, auth: &str) -> StationView {
    StationView {
        session_effort: false,
        station_id: StationId::from(name.to_owned()),
        name: name.to_owned(),
        key: "k".to_owned(),
        direction: "deck_dials".to_owned(),
        dial_url: None,
        reachable,
        last_seen: None,
        linked_at: None,
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
                default_mode: None,
            }],
            assist_tokens_today: None,
            assist_cap: None,
            resources: None,
            desktop: None,
            traits: None,
            config: None,
            taildrive: None,
            fleet_git: None,
            fleet_summary: None,
        }),
        info_at: None,
        display_resources: None,
        coordinator_live: false,
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

fn limited(mut card: DeckSession, instance: &str, used: Option<f64>) -> DeckSession {
    card.summary.provider_instance = Some(instance.to_owned().into());
    card.summary.rate_limits = vec![RateLimit {
        scope: "five_hour".to_owned(),
        used_percent: used,
        resets_at: None,
    }];
    card
}

#[test]
fn assign_auto_breaks_a_tie_by_the_instance_with_the_most_room() {
    // Same status, no mail, same activity time: only the quota differs.
    let cards = vec![
        limited(
            card("tight", CardStatus::Idle, "/p/app", 5),
            "a",
            Some(90.0),
        ),
        limited(
            card("roomy", CardStatus::Idle, "/p/app", 5),
            "b",
            Some(20.0),
        ),
        limited(card("unknown", CardStatus::Idle, "/p/app", 5), "c", None),
    ];
    let none = BTreeMap::new();
    let pick = |cards: &[DeckSession]| {
        pick_session(cards, &Needs::default(), &none)
            .map(|s| s.summary.session_id.0.clone())
            .unwrap_or_default()
    };
    assert_eq!(pick(&cards), "roomy");
    // Unknown counts as 50: it beats a tight instance, and loses to a roomy one.
    assert_eq!(
        pick(
            &cards[..1]
                .iter()
                .chain(&cards[2..])
                .cloned()
                .collect::<Vec<_>>()
        ),
        "unknown"
    );
    // The quota wins over recency: the tight instance is the most recent session.
    let recent = vec![
        limited(
            card("tight-new", CardStatus::Idle, "/p/app", 9),
            "a",
            Some(90.0),
        ),
        limited(
            card("roomy-old", CardStatus::Idle, "/p/app", 1),
            "b",
            Some(20.0),
        ),
    ];
    assert_eq!(pick(&recent), "roomy-old");
    // Equal quota: the most recent session wins.
    let equal = vec![
        limited(card("old", CardStatus::Idle, "/p/app", 1), "a", Some(20.0)),
        limited(card("new", CardStatus::Idle, "/p/app", 9), "b", Some(20.0)),
    ];
    assert_eq!(pick(&equal), "new");
    // A better fit still wins over more room.
    let mut busy_roomy = cards.clone();
    busy_roomy[1].status = CardStatus::Working;
    assert_eq!(pick(&busy_roomy), "unknown");
}

#[test]
fn the_quota_of_an_instance_is_its_tightest_latest_window() {
    let old = limited(card("old", CardStatus::Idle, "/p/app", 1), "a", Some(95.0));
    let new = limited(card("new", CardStatus::Idle, "/p/app", 9), "a", Some(30.0));
    let mut weekly = new.clone();
    weekly.summary.rate_limits.push(RateLimit {
        scope: "weekly".to_owned(),
        used_percent: Some(60.0),
        resets_at: None,
    });
    // The latest five_hour window (30) replaces the old one (95); weekly is tighter.
    let cards = vec![old, weekly.clone()];
    assert!((used_percent_of(&cards, &weekly) - 60.0).abs() < f64::EPSILON);
    // Another instance of the station does not share the windows.
    let other = limited(card("o", CardStatus::Idle, "/p/app", 9), "b", None);
    assert!(
        (used_percent_of(&[other.clone(), new], &other) - UNKNOWN_USED_PERCENT).abs()
            < f64::EPSILON
    );
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
