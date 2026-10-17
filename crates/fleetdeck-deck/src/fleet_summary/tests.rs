//! Tests of the builder, the usage port and the schedule. The task with a
//! real link is in `tests/fleet_summary_send.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::time::Duration;

use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::event::RateLimit;
use fleetdeck_proto::event::{RequestKind, ResponseChannel, ToolKind};
use fleetdeck_proto::fleet_summary::{DeckState, MAX_NEEDS_YOU, MAX_SUMMARY_BYTES};
use fleetdeck_proto::resources::CpuLoad;
use fleetdeck_proto::session::{OpenRequest, RepoInfo, RunningTool, SessionError};
use fleetdeck_proto::{CardStatus, SessionSummary, StationId};
use serde_json::json;
use tokio::time::Instant;

use super::Limits;
use super::build::{StationEntry, build, clean, pick_board, rfc3339};
use super::send::Schedule;

const NOW: i64 = 1_792_325_040_000;

fn st(id: &str) -> StationId {
    StationId::from(id)
}

fn station(id: &str, name: &str, up: bool) -> StationEntry {
    StationEntry {
        id: st(id),
        name: name.to_owned(),
        up,
        cpu: None,
    }
}

fn card(
    station_id: &str,
    session: &str,
    status: CardStatus,
    edit: impl FnOnce(&mut SessionSummary),
) -> DeckSession {
    let mut summary = SessionSummary::new(session.into());
    edit(&mut summary);
    DeckSession {
        station_id: st(station_id),
        summary,
        status,
        unread: None,
        project_id: None,
        review_rev: None,
    }
}

fn request(id: &str, at: i64) -> OpenRequest {
    OpenRequest {
        request_id: id.into(),
        node_id: None,
        request_kind: RequestKind::Command,
        title: "a title".to_owned(),
        details: "details".to_owned(),
        input: None,
        options: vec![],
        response: ResponseChannel::RpcReply,
        requested_at: at,
    }
}

fn limit(scope: &str, used: Option<f64>, resets_at: Option<i64>) -> RateLimit {
    RateLimit {
        scope: scope.to_owned(),
        used_percent: used,
        resets_at,
    }
}

fn with_limits(
    station_id: &str,
    session: &str,
    instance: &str,
    updated_at: i64,
    limits: Vec<RateLimit>,
) -> DeckSession {
    card(station_id, session, CardStatus::Idle, |s| {
        s.provider_instance = Some(instance.into());
        s.updated_at = updated_at;
        s.rate_limits = limits;
    })
}

fn one_station() -> Vec<StationEntry> {
    vec![station("st1", "srv", true)]
}

#[test]
fn the_default_limits_are_the_values_of_the_design() {
    let l = Limits::default();
    assert_eq!(
        (l.debounce_ms, l.keepalive_ms, l.timeout_ms),
        (2000, 60_000, 5000)
    );
}

#[test]
fn every_status_has_its_count() {
    use CardStatus::{Ended, Idle, Lost, NeedsYou, Queued, Unreachable, Verifying, Working};
    let cards: Vec<DeckSession> = [
        Working,
        Working,
        Verifying,
        Queued,
        Idle,
        Idle,
        NeedsYou,
        Unreachable,
        Unreachable,
        Unreachable,
        Lost,
        Ended,
        Ended,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, status)| card("st1", &format!("s{i}"), status, |_| {}))
    .collect();
    let summary = build(&cards, &one_station(), NOW, None);
    let c = summary.counts;
    assert_eq!(
        (c.working, c.idle, c.needs_you, c.blocked, c.lost),
        (4, 2, 1, 3, 1)
    );
    assert_eq!(summary.v, 1);
    assert_eq!(summary.deck, DeckState::Up);
}

#[test]
fn needs_you_items_are_oldest_first_and_at_most_ten() {
    let first = card("st1", "s1", CardStatus::NeedsYou, |s| {
        s.callsign = Some("lynx-12".to_owned());
        s.open_requests = (0..8)
            .map(|i| request(&format!("a{i}"), NOW - 1000 * (100 - i)))
            .collect();
    });
    let second = card("st1", "s2", CardStatus::NeedsYou, |s| {
        s.open_requests = vec![
            request("b0", NOW - 500_000),
            request("b1", NOW - 1000),
            request("b2", NOW + 9_000),
            request("b3", NOW - 95_000),
        ];
    });
    let summary = build(&[first, second], &one_station(), NOW, None);
    assert_eq!(summary.needs_you.len(), MAX_NEEDS_YOU);
    // The oldest request is first; the list is in order of age.
    assert_eq!(summary.needs_you[0].request, "b0");
    assert_eq!(summary.needs_you[0].age_s, 500);
    assert!(
        summary
            .needs_you
            .windows(2)
            .all(|w| w[0].age_s >= w[1].age_s)
    );
    // The two newest requests (`b1`, `b2`) are cut by the limit; `b2` is in
    // the future and has age 0.
    assert!(summary.needs_you.iter().all(|i| i.request != "b2"));
    let first_item = summary
        .needs_you
        .iter()
        .find(|i| i.session == "s1")
        .unwrap();
    assert_eq!(first_item.callsign.as_deref(), Some("lynx-12"));
    assert_eq!(first_item.station, "srv");
    assert_eq!(first_item.station_id, "st1");
    assert_eq!(first_item.kind, RequestKind::Command);
    // The count is the number of cards, not the number of requests.
    assert_eq!(summary.counts.needs_you, 2);
}

#[test]
fn a_future_request_has_age_zero_and_a_bad_callsign_is_null() {
    let long = "a".repeat(33);
    let cases = [
        ("lynx-12", Some("lynx-12")),
        ("Lynx", None),
        ("lynx_12", None),
        ("", None),
        (long.as_str(), None),
    ];
    for (callsign, want) in cases {
        let c = card("st1", "s1", CardStatus::NeedsYou, |s| {
            s.callsign = Some(callsign.to_owned());
            s.open_requests = vec![request("r1", NOW + 60_000)];
        });
        let summary = build(&[c], &one_station(), NOW, None);
        assert_eq!(summary.needs_you[0].age_s, 0);
        assert_eq!(
            summary.needs_you[0].callsign.as_deref(),
            want,
            "{callsign:?}"
        );
    }
}

#[test]
fn a_card_on_a_station_that_is_not_up_gives_no_item() {
    let c = card("st1", "s1", CardStatus::NeedsYou, |s| {
        s.open_requests = vec![request("r1", NOW - 1000)];
    });
    let down = vec![station("st1", "srv", false)];
    let summary = build(std::slice::from_ref(&c), &down, NOW, None);
    assert!(summary.needs_you.is_empty());
    // A card whose station is unknown gives no item either.
    let summary = build(&[c], &[], NOW, None);
    assert!(summary.needs_you.is_empty());
    // A card that is not in `needs_you` gives no item, with open requests.
    let idle = card("st1", "s2", CardStatus::Idle, |s| {
        s.open_requests = vec![request("r2", NOW - 1000)];
    });
    assert!(
        build(&[idle], &one_station(), NOW, None)
            .needs_you
            .is_empty()
    );
}

#[test]
fn the_usage_skips_an_unknown_use() {
    let cards = [with_limits(
        "st1",
        "s1",
        "claude",
        1,
        vec![
            limit("five_hour", None, None),
            limit("seven_day", Some(40.0), None),
        ],
    )];
    let usage = build(&cards, &one_station(), NOW, None).usage.unwrap();
    assert_eq!(
        (usage.label.as_str(), usage.pct, usage.resets_in_s),
        ("claude · 7 d", 40, None)
    );
    let unknown = [with_limits(
        "st1",
        "s1",
        "claude",
        1,
        vec![limit("five_hour", None, None)],
    )];
    assert_eq!(build(&unknown, &one_station(), NOW, None).usage, None);
    assert_eq!(build(&[], &one_station(), NOW, None).usage, None);
}

#[test]
fn the_highest_use_wins_and_a_tie_goes_by_key() {
    let cards = [
        with_limits(
            "st1",
            "s1",
            "codex",
            1,
            vec![limit("five_hour", Some(50.0), None)],
        ),
        with_limits(
            "st1",
            "s2",
            "claude",
            1,
            vec![limit("five_hour", Some(50.0), None)],
        ),
        with_limits(
            "st1",
            "s3",
            "claude",
            1,
            vec![limit("seven_day", Some(49.0), None)],
        ),
    ];
    let usage = build(&cards, &one_station(), NOW, None).usage.unwrap();
    assert_eq!((usage.label.as_str(), usage.pct), ("claude · 5 h", 50));
    let higher = [
        cards[0].clone(),
        with_limits(
            "st2",
            "s4",
            "codex",
            1,
            vec![limit("five_hour", Some(62.5), None)],
        ),
    ];
    let usage = build(&higher, &one_station(), NOW, None).usage.unwrap();
    assert_eq!((usage.label.as_str(), usage.pct), ("codex · 5 h", 63));
}

#[test]
fn the_latest_session_gives_the_window_of_a_scope() {
    // The same station, instance and scope: the more recently updated
    // session wins, also with the lower use. Another instance has a window
    // of 20 so that the first result is the one of the winner.
    let cards = [
        with_limits(
            "st1",
            "old",
            "claude",
            10,
            vec![limit("five_hour", Some(90.0), None)],
        ),
        with_limits(
            "st1",
            "new",
            "claude",
            20,
            vec![limit("five_hour", Some(30.0), None)],
        ),
        with_limits(
            "st1",
            "other",
            "codex",
            5,
            vec![limit("five_hour", Some(20.0), None)],
        ),
    ];
    let usage = build(&cards, &one_station(), NOW, None).usage.unwrap();
    assert_eq!((usage.label.as_str(), usage.pct), ("claude · 5 h", 30));
}

#[test]
fn a_window_that_reset_has_zero_seconds_left() {
    let at = |resets: Option<i64>| {
        let cards = [with_limits(
            "st1",
            "s1",
            "claude",
            1,
            vec![limit("five_hour", Some(10.0), resets)],
        )];
        build(&cards, &one_station(), NOW, None)
            .usage
            .unwrap()
            .resets_in_s
    };
    assert_eq!(at(Some(NOW + 4_320_000)), Some(4320));
    assert_eq!(at(Some(NOW - 1)), Some(0));
    assert_eq!(at(Some(NOW + 999)), Some(0));
    assert_eq!(at(None), None);
}

#[test]
fn the_scope_label_is_a_closed_set_and_the_instance_has_a_grammar() {
    let label = |instance: &str, scope: &str| {
        let cards = [with_limits(
            "st1",
            "s1",
            instance,
            1,
            vec![limit(scope, Some(10.0), None)],
        )];
        build(&cards, &one_station(), NOW, None)
            .usage
            .unwrap()
            .label
    };
    assert_eq!(label("claude-work_2", "five_hour"), "claude-work_2 · 5 h");
    assert_eq!(label("claude", "seven_day"), "claude · 7 d");
    assert_eq!(label("claude", "weekly_opus"), "claude · other");
    assert_eq!(label("claude", "codex-primary-limit-id"), "claude · other");
    assert_eq!(label("Claude", "five_hour"), "provider · 5 h");
    assert_eq!(label("a b", "five_hour"), "provider · 5 h");
    assert_eq!(label(&"x".repeat(33), "five_hour"), "provider · 5 h");
    assert_eq!(
        label(&"x".repeat(32), "five_hour"),
        format!("{} · 5 h", "x".repeat(32))
    );
}

#[test]
fn a_session_without_an_instance_uses_its_provider_word() {
    let c = card("st1", "s1", CardStatus::Idle, |s| {
        s.provider = Some(fleetdeck_proto::event::ProviderKind::Codex);
        s.rate_limits = vec![limit("five_hour", Some(5.0), None)];
    });
    let usage = build(&[c], &one_station(), NOW, None).usage.unwrap();
    assert_eq!(usage.label, "codex · 5 h");
}

/// The sentinel test (§3.1): every free-form source field of a card holds a
/// unique sentinel, and no sentinel reaches the serialized summary.
#[test]
fn no_free_form_text_reaches_the_summary() {
    let sentinels = std::cell::RefCell::new(Vec::<String>::new());
    let s = |field: &str| {
        let text = format!("SENTINEL-{field}-9d41");
        sentinels.borrow_mut().push(text.clone());
        text
    };
    let mut req = request("r1", NOW - 5000);
    req.title = s("request-title");
    req.details = s("request-details");
    req.input = Some(json!({ "command": s("tool-input"), "path": s("input-path") }));
    let c = card("st1", "s1", CardStatus::NeedsYou, |sum| {
        sum.title = Some(s("session-title"));
        sum.digest = Some(s("chat-text-digest"));
        sum.focus = Some(s("focus"));
        sum.cwd = Some(s("cwd-path"));
        sum.branch = Some(s("branch"));
        sum.worktree = Some(s("worktree-path"));
        sum.repo = Some(RepoInfo {
            root: s("repo-root-path"),
            origin: Some(s("repo-origin")),
        });
        sum.model = Some(s("model"));
        sum.mode = Some(s("mode"));
        sum.slash_commands = Some(vec![s("slash-command")]);
        sum.skills = Some(vec![s("skill")]);
        sum.last_error = Some(SessionError {
            code: s("error-code"),
            message: s("error-message-mail"),
            retry_at: None,
        });
        sum.running_tools = vec![RunningTool {
            node_id: "n1".into(),
            tool_kind: ToolKind::Shell,
            title: s("tool-name"),
            input_summary: s("tool-input-summary"),
        }];
        // A callsign outside the grammar is not copied.
        sum.callsign = Some(s("CALLSIGN"));
        sum.provider_instance = Some(s("Instance").as_str().into());
        sum.updated_at = 10;
        sum.open_requests = vec![req];
        // The rate-limit scope and a Codex `limitId` are free text.
        sum.rate_limits = vec![
            limit(&s("rate-limit-scope"), Some(97.0), None),
            limit(&s("codex-limit-id"), Some(96.0), None),
        ];
    });
    let summary = build(&[c], &one_station(), NOW, Some("https://deck.ts.net:7420"));
    let json = serde_json::to_string(&summary).unwrap();
    for sentinel in sentinels.borrow().iter() {
        assert!(!json.contains(sentinel.as_str()), "{sentinel} is in {json}");
    }
    assert!(!json.contains("SENTINEL"), "{json}");
    // The item and the usage are there, with fixed words only.
    assert_eq!(summary.needs_you.len(), 1);
    assert_eq!(summary.needs_you[0].callsign, None);
    assert_eq!(summary.usage.unwrap().label, "provider · other");
}

#[test]
fn a_summary_over_16_kib_drops_needs_you_items_from_the_end() {
    // Valid ids are at most 64 characters, so a built summary stays far below
    // 16 KiB; `fit` is the safety net and is tested on a stretched summary.
    let cards: Vec<DeckSession> = (0..10)
        .map(|i| {
            card("st1", &format!("s{i}"), CardStatus::NeedsYou, |s| {
                s.open_requests = vec![request("r", NOW - 100_000 + i * 1000)];
            })
        })
        .collect();
    let mut summary = build(&cards, &one_station(), NOW, None);
    assert_eq!(summary.needs_you.len(), 10);
    for item in &mut summary.needs_you {
        item.station = "x".repeat(3000);
    }
    super::build::fit(&mut summary);
    let len = serde_json::to_vec(&summary).unwrap().len();
    assert!(len <= MAX_SUMMARY_BYTES, "{len}");
    let kept = summary.needs_you.len();
    assert!((1..10).contains(&kept), "{kept}");
    // The oldest items stay: `s0` is the oldest.
    assert!(
        summary
            .needs_you
            .iter()
            .enumerate()
            .all(|(i, item)| item.session == format!("s{i}"))
    );
    assert_eq!(summary.counts.needs_you, 10);
}

#[test]
fn an_item_with_an_id_outside_the_grammar_is_left_out() {
    let long = card("st1", &"x".repeat(65), CardStatus::NeedsYou, |s| {
        s.open_requests = vec![request("r", NOW - 1000)];
    });
    let colon = card("st1", "s:1", CardStatus::NeedsYou, |s| {
        s.open_requests = vec![request("r", NOW - 1000)];
    });
    let summary = build(&[long, colon], &one_station(), NOW, None);
    assert!(summary.needs_you.is_empty());
    assert_eq!(summary.counts.needs_you, 2);
}

#[test]
fn stations_drop_after_every_item_when_the_summary_is_still_too_large() {
    let cards: Vec<DeckSession> = (0..3)
        .map(|i| {
            card("st1", &format!("s{i}"), CardStatus::NeedsYou, |s| {
                // One id alone is above the limit.
                s.open_requests = vec![request(&"r".repeat(20_000), NOW - 1000)];
            })
        })
        .collect();
    let stations: Vec<StationEntry> = (0..40)
        .map(|i| station(&format!("st{i}"), &format!("station-{i}"), true))
        .collect();
    let summary = build(&cards, &stations, NOW, None);
    assert!(summary.needs_you.is_empty());
    // At most 32 stations, in the order of the list; they fit once the items are gone.
    assert_eq!(summary.stations.len(), 32);
    assert_eq!(summary.stations[0].name, "station-0");
    assert_eq!(summary.stations[31].name, "station-31");
    assert!(serde_json::to_vec(&summary).unwrap().len() <= MAX_SUMMARY_BYTES);
}

#[test]
fn texts_lose_control_and_bidirectional_characters_and_are_cut() {
    let name = "sr\u{202E}v\u{200F}\n\t\u{0}x\u{2067}y";
    assert_eq!(clean(name, 64), "srvxy");
    // A cut never splits a character.
    assert_eq!(clean(&"é".repeat(40), 64), "é".repeat(32));
    assert_eq!(clean("abcdef", 3), "abc");
    let long = format!("{}{}", "\u{202E}".repeat(10), "n".repeat(100));
    let summary = build(&[], &[station("st1", &long, true)], NOW, None);
    assert_eq!(summary.stations[0].name, "n".repeat(64));
}

#[test]
fn the_station_list_keeps_its_order_and_the_cpu_load_is_a_rounded_percent() {
    let cpu = |cores, load| {
        Some(CpuLoad {
            cores,
            load1_x100: load,
            load5_x100: 0,
        })
    };
    let mut list = vec![station("b", "beta", true), station("a", "alpha", false)];
    list[0].cpu = cpu(4, 165);
    list[1].cpu = cpu(4, 170);
    let mut more = vec![station("c", "gamma", true), station("d", "delta", true)];
    more[0].cpu = cpu(0, 100);
    more[1].cpu = cpu(4, 800);
    list.extend(more);
    let summary = build(&[], &list, NOW, None);
    let got: Vec<_> = summary
        .stations
        .iter()
        .map(|s| (s.name.as_str(), s.up, s.cpu_load_pct))
        .collect();
    assert_eq!(
        got,
        [
            ("beta", true, Some(41)),
            ("alpha", false, Some(43)),
            ("gamma", true, None),
            ("delta", true, Some(200)),
        ]
    );
    let none = build(&[], &one_station(), NOW, None);
    assert_eq!(none.stations[0].cpu_load_pct, None);
}

#[test]
fn the_time_is_rfc_3339_in_utc() {
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(NOW), "2026-10-18T12:04:00Z");
    assert_eq!(rfc3339(NOW + 999), "2026-10-18T12:04:00Z");
    assert_eq!(
        rfc3339(951_782_400_000 + 86_399_000),
        "2000-02-29T23:59:59Z"
    );
    assert_eq!(build(&[], &[], NOW, None).at, "2026-10-18T12:04:00Z");
}

#[test]
fn the_board_is_the_first_origin_that_is_not_loopback() {
    let pick = |list: &[&str]| pick_board(list.iter().map(|s| (*s).to_owned()));
    assert_eq!(
        pick(&["https://127.0.0.1:7420", "https://deck.tail1.ts.net:7420"]).as_deref(),
        Some("https://deck.tail1.ts.net:7420")
    );
    assert_eq!(
        pick(&["https://localhost:7420", "https://[::1]:7420"]),
        None
    );
    assert_eq!(pick(&[]), None);
    assert_eq!(
        pick(&["https://[fd7a::1]:7420"]).as_deref(),
        Some("https://[fd7a::1]:7420")
    );
    // A path, a query and a fragment are removed; the scheme is lower case.
    assert_eq!(
        pick(&["HTTPS://100.64.0.1:7420/a/b?x=1#f"]).as_deref(),
        Some("https://100.64.0.1:7420")
    );
    assert_eq!(
        pick(&["http://deck.lan"]).as_deref(),
        Some("http://deck.lan")
    );
}

#[test]
fn a_board_that_is_not_an_origin_is_null() {
    let board = |b: &str| build(&[], &[], NOW, Some(b)).board;
    assert_eq!(
        board("https://deck.ts.net:7420").as_deref(),
        Some("https://deck.ts.net:7420")
    );
    for bad in [
        "ftp://deck.ts.net",
        "javascript://deck",
        "deck.ts.net:7420",
        "https://",
        "https://user:pass@deck.ts.net",
        "https://deck ts.net",
        "https://deck.ts.net\n",
        "https://dëck.net",
        "https://\u{202E}deck.net",
    ] {
        assert_eq!(board(bad), None, "{bad:?}");
    }
    let long = format!("https://{}", "a".repeat(2048));
    assert_eq!(board(&long), None);
    let fits = format!("https://{}", "a".repeat(2048 - 8));
    assert_eq!(board(&fits).map(|b| b.len()), Some(2048));
    assert_eq!(build(&[], &[], NOW, None).board, None);
}

fn ids(names: &[&str]) -> HashSet<StationId> {
    names.iter().map(|n| st(n)).collect()
}

fn schedule() -> (Schedule, Instant) {
    (Schedule::new(Limits::default()), Instant::now())
}

const DEBOUNCE: Duration = Duration::from_millis(2000);
const KEEPALIVE: Duration = Duration::from_millis(60_000);

#[test]
fn a_station_that_becomes_eligible_gets_a_push_at_once() {
    let (mut s, t0) = schedule();
    assert_eq!(s.due(t0), Vec::<StationId>::new());
    assert_eq!(s.next_wake(t0), None);
    s.observe(&ids(&["a", "b"]), false);
    assert_eq!(s.due(t0), [st("a"), st("b")]);
    // One request in flight: nothing more, even after the window.
    assert_eq!(s.due(t0 + DEBOUNCE * 2), Vec::<StationId>::new());
    assert_eq!(s.next_wake(t0), None);
}

#[test]
fn two_changes_inside_the_window_give_one_push_with_the_latest_state() {
    let (mut s, t0) = schedule();
    s.observe(&ids(&["a"]), false);
    assert_eq!(s.due(t0), [st("a")]);
    s.finish(&st("a"));
    let ms = Duration::from_millis;
    s.observe(&ids(&["a"]), true);
    assert_eq!(s.due(t0 + ms(100)), Vec::<StationId>::new());
    s.observe(&ids(&["a"]), true);
    assert_eq!(s.due(t0 + ms(1900)), Vec::<StationId>::new());
    // The task wakes at the end of the window.
    assert_eq!(s.next_wake(t0 + ms(1900)), Some(t0 + DEBOUNCE));
    assert_eq!(s.due(t0 + DEBOUNCE), [st("a")]);
    s.finish(&st("a"));
    // No change since: nothing is pending.
    assert_eq!(s.due(t0 + DEBOUNCE * 2), Vec::<StationId>::new());
}

#[test]
fn a_change_during_a_push_waits_for_its_end_and_for_the_window() {
    let (mut s, t0) = schedule();
    s.observe(&ids(&["a"]), false);
    assert_eq!(s.due(t0), [st("a")]);
    s.observe(&ids(&["a"]), true);
    // In flight for 3 s: the window is over, but a push is still running.
    assert_eq!(s.due(t0 + Duration::from_secs(3)), Vec::<StationId>::new());
    s.finish(&st("a"));
    assert_eq!(
        s.next_wake(t0 + Duration::from_secs(3)),
        Some(t0 + DEBOUNCE)
    );
    assert_eq!(s.due(t0 + Duration::from_secs(3)), [st("a")]);
}

#[test]
fn the_keepalive_sends_without_a_change() {
    let (mut s, t0) = schedule();
    s.observe(&ids(&["a"]), false);
    assert_eq!(s.due(t0), [st("a")]);
    s.finish(&st("a"));
    assert_eq!(s.next_wake(t0), Some(t0 + KEEPALIVE));
    assert_eq!(
        s.due(t0 + KEEPALIVE - Duration::from_millis(1)),
        Vec::<StationId>::new()
    );
    assert_eq!(s.due(t0 + KEEPALIVE), [st("a")]);
    s.finish(&st("a"));
    assert_eq!(s.next_wake(t0 + KEEPALIVE), Some(t0 + KEEPALIVE * 2));
}

#[test]
fn a_setting_that_turns_off_stops_the_next_push() {
    let (mut s, t0) = schedule();
    s.observe(&ids(&["a"]), false);
    assert_eq!(s.due(t0), [st("a")]);
    s.finish(&st("a"));
    s.observe(&ids(&["a"]), true);
    // The setting turns off before the window ends: the pending push is gone.
    s.observe(&ids(&[]), true);
    assert_eq!(s.due(t0 + DEBOUNCE), Vec::<StationId>::new());
    assert_eq!(s.due(t0 + KEEPALIVE * 2), Vec::<StationId>::new());
    assert_eq!(s.next_wake(t0 + DEBOUNCE), None);
    // Changes while it is off send nothing.
    s.observe(&ids(&[]), true);
    assert_eq!(s.due(t0 + KEEPALIVE * 3), Vec::<StationId>::new());
}

#[test]
fn a_setting_that_turns_on_sends_and_a_flapping_link_stays_inside_the_window() {
    let (mut s, t0) = schedule();
    let ms = Duration::from_millis;
    s.observe(&ids(&["a"]), false);
    assert_eq!(s.due(t0), [st("a")]);
    s.finish(&st("a"));
    // The link drops and comes up again 300 ms later: a new push, but not
    // before the window of 2 s ends.
    s.observe(&ids(&[]), true);
    assert_eq!(s.due(t0 + ms(100)), Vec::<StationId>::new());
    s.observe(&ids(&["a"]), true);
    assert_eq!(s.due(t0 + ms(300)), Vec::<StationId>::new());
    assert_eq!(s.due(t0 + DEBOUNCE), [st("a")]);
    s.finish(&st("a"));
    // After a long pause a station that turns on is sent at once.
    s.observe(&ids(&[]), true);
    let later = t0 + Duration::from_secs(30);
    assert_eq!(s.due(later), Vec::<StationId>::new());
    s.observe(&ids(&["a"]), true);
    assert_eq!(s.due(later), [st("a")]);
}
